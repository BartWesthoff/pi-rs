// Golden data for configuration and resource resolution: settings loading, merging, getters and
// writes; package source parsing (git URLs, npm specs, local paths); package and resource resolution
// over file trees; skill, prompt template and context file discovery; prompt template arguments;
// model reference and scope resolution against the built-in catalog; keybinding resolution.
// Written to `config.json`; compared by `crates/pi-coding-agent/src/core/golden_config_tests.rs`.
//
// Case shape: `{ name, kind, input, out }`. File-tree cases list their files in `input.files`
// (paths relative to a scratch root); paths in `out` start with `<root>`.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const REF = process.env.PI_TS_REF;
if (!REF) throw new Error("PI_TS_REF must point at the TypeScript pi checkout");
const here = path.dirname(fileURLToPath(import.meta.url));
const OUT_DIR = process.env.GOLDEN_OUT ?? path.resolve(here, "../../crates/pi-coding-agent/tests/fixtures/golden");
const ref = (rel) => import(pathToFileURL(path.join(REF, "packages", rel)).href);

const { SettingsManager } = await ref("coding-agent/src/core/settings-manager.ts");
const { collectSettingsDiagnostics } = await ref("coding-agent/src/core/settings-diagnostics.ts");
const { parseGitUrl } = await ref("coding-agent/src/utils/git.ts");
const { DefaultPackageManager } = await ref("coding-agent/src/core/package-manager.ts");
const { loadSkills } = await ref("coding-agent/src/core/skills.ts");
const { loadPromptTemplates, parseCommandArgs, substituteArgs, expandPromptTemplate } = await ref(
	"coding-agent/src/core/prompt-templates.ts",
);
const { loadProjectContextFiles } = await ref("coding-agent/src/core/resource-loader.ts");
const { resolveModelReference, resolveModelScopeFromModels } = await ref("coding-agent/src/core/model-resolver.ts");
const { KeybindingsManager, migrateKeybindingsConfig } = await ref("coding-agent/src/core/keybindings.ts");
const { getBuiltinProviders, getBuiltinModels } = await ref("ai/src/providers/all.ts");

const cases = [];
const names = new Set();
function add(name, kind, input, out) {
	if (names.has(name)) throw new Error(`duplicate case ${name}`);
	names.add(name);
	cases.push({ name, kind, input, out: JSON.parse(JSON.stringify(out)) });
}

/** `undefined` survives JSON as a marker. */
const u = (value) => (value === undefined ? "<undefined>" : value);

const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "pi-golden-config-"));
let caseCounter = 0;
/** Write `files` (relative path -> content, or `{ symlink: target }`) below a fresh root. */
function makeTree(files = {}) {
	const root = path.join(scratch, `case-${++caseCounter}`);
	fs.mkdirSync(root, { recursive: true });
	for (const [rel, content] of Object.entries(files)) {
		const file = path.join(root, rel);
		fs.mkdirSync(path.dirname(file), { recursive: true });
		if (content !== null && typeof content === "object" && "symlink" in content) {
			fs.symlinkSync(content.symlink, file);
		} else if (content !== null && typeof content === "object" && "dir" in content) {
			fs.mkdirSync(file, { recursive: true });
		} else {
			fs.writeFileSync(file, content);
		}
	}
	return root;
}

/** Replace the root in every string of `value`. */
function relativize(value, root) {
	if (typeof value === "string") return value.split(root).join("<root>");
	if (Array.isArray(value)) return value.map((item) => relativize(item, root));
	if (value && typeof value === "object") {
		return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, relativize(v, root)]));
	}
	return value;
}

function readIfExists(file) {
	return fs.existsSync(file) ? fs.readFileSync(file, "utf-8") : "<missing>";
}

function capture(fn) {
	try {
		return { ok: u(fn()) };
	} catch (error) {
		return { error: error instanceof Error ? error.message : String(error) };
	}
}

// ---------------------------------------------------------------------------------------------
// Settings

const GETTERS = [
	["getSessionDir", (s) => s.getSessionDir()],
	["getDefaultProvider", (s) => s.getDefaultProvider()],
	["getDefaultModel", (s) => s.getDefaultModel()],
	["getSteeringMode", (s) => s.getSteeringMode()],
	["getFollowUpMode", (s) => s.getFollowUpMode()],
	["getThemeSetting", (s) => s.getThemeSetting()],
	["getTheme", (s) => s.getTheme()],
	["getDefaultThinkingLevel", (s) => s.getDefaultThinkingLevel()],
	["getModelThinkingLevel", (s) => s.getModelThinkingLevel("anthropic", "claude-opus-4-8")],
	["getAllModelThinkingLevels", (s) => s.getAllModelThinkingLevels()],
	["getTransport", (s) => s.getTransport()],
	["getSessionShareDestination", (s) => s.getSessionShareDestination()],
	["getCompactionSettings", (s) => s.getCompactionSettings()],
	["getCompactionSettingsForModel", (s) => s.getCompactionSettings({ provider: "anthropic", id: "claude-opus-4-8" })],
	["getBranchSummarySettings", (s) => s.getBranchSummarySettings()],
	["getRetrySettings", (s) => s.getRetrySettings()],
	["getHttpIdleTimeoutMs", (s) => s.getHttpIdleTimeoutMs()],
	["getCacheWarmingMode", (s) => s.getCacheWarmingMode()],
	["getProviderRetrySettings", (s) => s.getProviderRetrySettings()],
	["getWebSocketConnectTimeoutMs", (s) => s.getWebSocketConnectTimeoutMs()],
	["getHideThinkingBlock", (s) => s.getHideThinkingBlock()],
	["getShowCacheMissNotices", (s) => s.getShowCacheMissNotices()],
	["getShellPath", (s) => s.getShellPath()],
	["getQuietStartup", (s) => s.getQuietStartup()],
	["getDefaultProjectTrust", (s) => s.getDefaultProjectTrust()],
	["getShellCommandPrefix", (s) => s.getShellCommandPrefix()],
	["getNpmCommand", (s) => s.getNpmCommand()],
	["getCollapseChangelog", (s) => s.getCollapseChangelog()],
	// pi-rs diverges: this fork defaults to false, so the generated fixtures were adjusted by hand.
	["getEnableInstallTelemetry", (s) => s.getEnableInstallTelemetry()],
	["getEnableAnalytics", (s) => s.getEnableAnalytics()],
	["getPackages", (s) => s.getPackages()],
	["getExtensionPaths", (s) => s.getExtensionPaths()],
	["getSkillPaths", (s) => s.getSkillPaths()],
	["getPromptTemplatePaths", (s) => s.getPromptTemplatePaths()],
	["getThemePaths", (s) => s.getThemePaths()],
	["getEnableSkillCommands", (s) => s.getEnableSkillCommands()],
	["getThinkingBudgets", (s) => s.getThinkingBudgets()],
	["getTerminalCapabilityOverrides", (s) => s.getTerminalCapabilityOverrides()],
	["getShowImages", (s) => s.getShowImages()],
	["getImageWidthCells", (s) => s.getImageWidthCells()],
	["getClearOnShrink", (s) => s.getClearOnShrink()],
	["getShowTerminalProgress", (s) => s.getShowTerminalProgress()],
	["getTuiMode", (s) => s.getTuiMode()],
	["getFullscreenExitOutput", (s) => s.getFullscreenExitOutput()],
	["getFullscreenScrollbar", (s) => s.getFullscreenScrollbar()],
	["getFullscreenCopyOnSelect", (s) => s.getFullscreenCopyOnSelect()],
	["getFullscreenWheelScrollLines", (s) => s.getFullscreenWheelScrollLines()],
	["getImageAutoResize", (s) => s.getImageAutoResize()],
	["getBlockImages", (s) => s.getBlockImages()],
	["getEnabledModels", (s) => s.getEnabledModels()],
	["getDefaultTools", (s) => s.getDefaultTools()],
	["getDoubleEscapeAction", (s) => s.getDoubleEscapeAction()],
	["getTreeFilterMode", (s) => s.getTreeFilterMode()],
	["getShowHardwareCursor", (s) => s.getShowHardwareCursor()],
	["getEditorPaddingX", (s) => s.getEditorPaddingX()],
	["getOutputPad", (s) => s.getOutputPad()],
	["getAutocompleteMaxVisible", (s) => s.getAutocompleteMaxVisible()],
	["getCodeBlockIndent", (s) => s.getCodeBlockIndent()],
	["getMermaidRenderingMode", (s) => s.getMermaidRenderingMode()],
	["getWarnings", (s) => s.getWarnings()],
];

/** Setter calls by name; `args` are JSON. */
function runOp(settings, { op, args = [] }, ctx) {
	switch (op) {
		case "writeGlobal":
			return fs.writeFileSync(path.join(ctx.agentDir, "settings.json"), args[0]);
		case "reload":
			return settings.reload();
		case "setProjectTrusted":
			return settings.setProjectTrusted(args[0]);
		case "applyOverrides":
			return settings.applyOverrides(args[0]);
		default:
			return settings[op](...args);
	}
}

async function settingsCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	if (input.global !== undefined) fs.writeFileSync(path.join(agentDir, "settings.json"), input.global);
	if (input.project !== undefined) {
		fs.mkdirSync(path.join(cwd, ".pi"), { recursive: true });
		fs.writeFileSync(path.join(cwd, ".pi", "settings.json"), input.project);
	}
	const settings = SettingsManager.create(cwd, agentDir, { projectTrusted: input.trusted ?? true });
	const opResults = [];
	for (const op of input.ops ?? []) {
		try {
			const result = runOp(settings, op, { agentDir });
			if (result instanceof Promise) await result;
			// Writes are queued; let each one land before the next operation, as between user actions.
			await settings.flush();
			opResults.push("ok");
		} catch (error) {
			opResults.push({ error: error.message });
		}
	}
	await settings.flush();
	const getters = {};
	for (const [getter, fn] of GETTERS) getters[getter] = capture(() => fn(settings));
	const out = {
		settings: settings.getSettings(),
		globalSettings: settings.getGlobalSettings(),
		projectSettings: settings.getProjectSettings(),
		getters,
		ops: opResults,
		diagnostics: collectSettingsDiagnostics(settings).map((d) => d.message),
		globalFile: readIfExists(path.join(agentDir, "settings.json")),
		projectFile: readIfExists(path.join(cwd, ".pi", "settings.json")),
	};
	add(name, "settings", input, relativize(out, root));
}

const fullSettings = JSON.stringify(
	{
		defaultProvider: "anthropic",
		defaultModel: "claude-opus-4-8",
		defaultThinkingLevel: "high",
		modelThinkingLevels: { "anthropic/claude-opus-4-8": "low", "openai/gpt-5.5": "xhigh" },
		transport: "sse",
		sessionShareDestination: "gist",
		steeringMode: "all",
		followUpMode: "all",
		theme: "light",
		compaction: { enabled: false, reserveTokens: 1000, keepRecentTokens: 2000, modelOverrides: { "anthropic/claude-opus-4-8": { reserveTokens: 5 } } },
		branchSummary: { reserveTokens: 77, skipPrompt: true },
		retry: { enabled: false, maxRetries: 9, baseDelayMs: 10, maxAgentDelayMs: 20, provider: { timeoutMs: 30, maxRetries: 2, maxRetryDelayMs: 40 } },
		hideThinkingBlock: true,
		showCacheMissNotices: true,
		externalEditor: "vim",
		shellPath: "/bin/zsh",
		quietStartup: "header",
		defaultProjectTrust: "always",
		shellCommandPrefix: "shopt -s expand_aliases",
		npmCommand: ["mise", "exec", "node@20", "--", "npm"],
		collapseChangelog: true,
		enableInstallTelemetry: false,
		enableAnalytics: true,
		trackingId: "track",
		packages: ["npm:foo", { source: "git:github.com/a/b", skills: ["x"] }],
		extensions: ["./ext.ts"],
		skills: ["./skills"],
		prompts: ["./prompts"],
		themes: ["./themes"],
		enableSkillCommands: false,
		terminal: { showImages: false, imageWidthCells: 33.7, clearOnShrink: true, showTerminalProgress: true, images: "kitty", trueColor: false, hyperlinks: true },
		images: { autoResize: false, blockImages: true },
		enabledModels: ["anthropic/*", "gpt-5.5"],
		defaultTools: ["read", "grep"],
		doubleEscapeAction: "fork",
		treeFilterMode: "user-only",
		thinkingBudgets: { low: 100, high: 2000 },
		editorPaddingX: 2,
		outputPad: 0,
		autocompleteMaxVisible: 9,
		showHardwareCursor: true,
		markdown: { codeBlockIndent: "    ", mermaid: "off" },
		warnings: { anthropicExtraUsage: false },
		sessionDir: "./sessions",
		httpIdleTimeoutMs: "disabled",
		cacheWarming: "idle",
		websocketConnectTimeoutMs: 1500.9,
		tuiMode: "regular",
		fullscreenExitOutput: "resume-hint",
		fullscreenScrollbar: "hidden",
		fullscreenCopyOnSelect: false,
		fullscreenWheelScrollLines: 250,
	},
	null,
	2,
);

await settingsCase("settings/empty", {});
await settingsCase("settings/full-global", { global: fullSettings });
await settingsCase("settings/full-project", { project: fullSettings });
await settingsCase("settings/full-project-untrusted", { project: fullSettings, trusted: false });
await settingsCase("settings/deep-merge", {
	global: JSON.stringify({
		theme: "dark",
		compaction: { enabled: false, reserveTokens: 100 },
		terminal: { showImages: false },
		packages: ["npm:a"],
		extensions: ["a.ts"],
		defaultTools: ["read", "bash"],
		markdown: { mermaid: "final" },
	}),
	project: JSON.stringify({
		compaction: { keepRecentTokens: 5 },
		terminal: { showImages: true, imageWidthCells: 0.5 },
		packages: ["npm:b"],
		extensions: [],
		defaultTools: ["+grep", "-bash"],
		markdown: null,
	}),
});
await settingsCase("settings/default-tools-modifiers-only", {
	global: JSON.stringify({ defaultTools: ["-edit", "+find", "+find", "+"] }),
	project: JSON.stringify({ defaultTools: ["-write"] }),
});
await settingsCase("settings/default-tools-malformed", {
	global: JSON.stringify({ defaultTools: "read" }),
	project: JSON.stringify({ defaultTools: ["+grep", 5] }),
});
await settingsCase("settings/migrations", {
	global: JSON.stringify({
		queueMode: "all",
		websockets: true,
		skills: { enableSkillCommands: false, customDirectories: ["/a", "/b"] },
		retry: { maxDelayMs: 1234, provider: { timeoutMs: 5 } },
	}),
	project: JSON.stringify({
		queueMode: "one-at-a-time",
		steeringMode: "all",
		websockets: false,
		transport: "auto",
		skills: { customDirectories: [] },
		retry: { maxDelayMs: 1, provider: { maxRetryDelayMs: 2 } },
	}),
});
await settingsCase("settings/migrations-edge", {
	global: JSON.stringify({ websockets: "yes", skills: { enableSkillCommands: true }, retry: { maxDelayMs: "x" } }),
	project: JSON.stringify({ retry: { maxDelayMs: 9, provider: null } }),
});
await settingsCase("settings/invalid-json", { global: "{\n  \"theme\": \"dark\",\n}", project: "not json" });
await settingsCase("settings/bom-and-empty", { global: "\uFEFF{\"theme\":\"bom\"}", project: "" });
await settingsCase("settings/non-object-json", { global: "[1, 2]", project: "null" });
await settingsCase("settings/scalar-json", { global: "42" });
await settingsCase("settings/invalid-values", {
	global: JSON.stringify({
		steeringMode: "bogus",
		followUpMode: 7,
		doubleEscapeAction: "sideways",
		defaultThinkingLevel: "turbo",
		modelThinkingLevels: { "anthropic/claude-opus-4-8": "ultra", "x/y": null },
		transport: 5,
		sessionShareDestination: "radius",
		compaction: { reserveTokens: -1, keepRecentTokens: 1.5 },
		httpIdleTimeoutMs: "abc",
		websocketConnectTimeoutMs: -5,
		quietStartup: "yes",
		defaultProjectTrust: "never",
		cacheWarming: "warm",
		terminal: { imageWidthCells: "wide", images: false, trueColor: "auto", hyperlinks: "auto" },
		fullscreenWheelScrollLines: 0,
		tuiMode: "fullscreen",
		fullscreenScrollbar: "always",
		treeFilterMode: "bogus",
		outputPad: 1,
		markdown: { mermaid: "final" },
		theme: "./themes/custom.json",
		showHardwareCursor: false,
		enabledModels: [],
		sessionDir: "",
		shellPath: "",
	}),
});
await settingsCase("settings/non-boolean-flags", {
	global: JSON.stringify({
		hideThinkingBlock: 1,
		showCacheMissNotices: "yes",
		collapseChangelog: 0,
		enableInstallTelemetry: "",
		enableAnalytics: [],
		enableSkillCommands: null,
		terminal: { showImages: "no", clearOnShrink: 0, showTerminalProgress: {} },
		images: { autoResize: null, blockImages: 1 },
		compaction: { enabled: 0 },
		retry: { enabled: "" },
		branchSummary: { skipPrompt: 1 },
		fullscreenCopyOnSelect: 0,
		showHardwareCursor: "1",
	}),
});
await settingsCase("settings/model-override-errors", {
	global: JSON.stringify({
		compaction: { modelOverrides: { "anthropic/claude-opus-4-8": "big" } },
	}),
});
await settingsCase("settings/model-override-value-error", {
	global: JSON.stringify({
		compaction: { reserveTokens: 10, modelOverrides: { "anthropic/claude-opus-4-8": { keepRecentTokens: "x" } } },
	}),
});
await settingsCase("settings/timeouts", {
	global: JSON.stringify({ httpIdleTimeoutMs: " 1500.7 ", websocketConnectTimeoutMs: "0x10" }),
});
await settingsCase("settings/timeouts-numbers", {
	global: JSON.stringify({ httpIdleTimeoutMs: 0, websocketConnectTimeoutMs: "Disabled" }),
});
await settingsCase("settings/setters-global", {
	global: JSON.stringify({ theme: "dark", compaction: { enabled: true, custom: 1 }, unknownKey: { a: 1 }, float: 1.0 }, null, 4),
	ops: [
		{ op: "setDefaultModelAndProvider", args: ["openai", "gpt-5.5"] },
		{ op: "setTheme", args: ["light"] },
		{ op: "setCompactionEnabled", args: [false] },
		{ op: "setModelThinkingLevel", args: ["openai", "gpt-5.5", "high"] },
		{ op: "setModelThinkingLevel", args: ["a", "b", "low"] },
		{ op: "removeModelThinkingLevel", args: ["a", "b"] },
		{ op: "setShowImages", args: [false] },
		{ op: "setImageWidthCells", args: [12.9] },
		{ op: "setMermaidRenderingMode", args: ["final"] },
		{ op: "setPackages", args: [["npm:x", { source: "./local", extensions: [] }]] },
		{ op: "setEnabledModels", args: [undefined] },
		{ op: "setShellPath", args: ["~/bin/zsh"] },
		{ op: "setHttpIdleTimeoutMs", args: [1234.5] },
		{ op: "setHttpIdleTimeoutMs", args: [-1] },
		{ op: "setFullscreenWheelScrollLines", args: [500] },
		{ op: "setEditorPaddingX", args: [9] },
		{ op: "setAutocompleteMaxVisible", args: [1] },
		{ op: "setSteeringMode", args: ["all"] },
		{ op: "setQuietStartup", args: ["header"] },
		{ op: "setNpmCommand", args: [["pnpm"]] },
		{ op: "setWarnings", args: [{ anthropicExtraUsage: false }] },
		{ op: "setRetryEnabled", args: [false] },
		{ op: "setBlockImages", args: [true] },
		{ op: "setClearOnShrink", args: [true] },
		{ op: "setOutputPad", args: [0] },
		{ op: "setTreeFilterMode", args: ["labeled-only"] },
		{ op: "setDefaultProjectTrust", args: ["never"] },
	],
});
await settingsCase("settings/number-formatting", {
	global: '{"a": 1.0, "b": 1e3, "c": -0, "d": 1.5e-7, "e": 12345678901234567890, "f": 0.1, "g": 1e21, "h": [2.50, {"i": 3.0}], "s": "\\u2028\\u0001\\/"}',
	ops: [{ op: "setTheme", args: ["x"] }],
});
await settingsCase("settings/setters-merge-external-edit", {
	global: JSON.stringify({ terminal: { showImages: true, imageWidthCells: 10 } }),
	ops: [
		{ op: "setShowImages", args: [false] },
		{ op: "writeGlobal", args: ["{\"terminal\":{\"showImages\":true,\"clearOnShrink\":true},\"theme\":\"external\"}"] },
		{ op: "setImageWidthCells", args: [20] },
	],
});
await settingsCase("settings/setters-after-load-error", {
	global: "{ broken",
	ops: [{ op: "setTheme", args: ["light"] }],
});
await settingsCase("settings/project-setters", {
	project: JSON.stringify({ prompts: ["a"] }),
	ops: [
		{ op: "setProjectPackages", args: [["npm:p"]] },
		{ op: "setProjectExtensionPaths", args: [["./e.ts"]] },
		{ op: "setProjectSkillPaths", args: [[]] },
		{ op: "setProjectThemePaths", args: [["t"]] },
		{ op: "setProjectPromptTemplatePaths", args: [["b", "c"]] },
	],
});
await settingsCase("settings/project-setters-untrusted", {
	project: JSON.stringify({ prompts: ["a"] }),
	trusted: false,
	ops: [{ op: "setProjectPackages", args: [["npm:p"]] }],
});
await settingsCase("settings/trust-toggle", {
	global: JSON.stringify({ theme: "g" }),
	project: JSON.stringify({ theme: "p", extensions: ["x"] }),
	trusted: false,
	ops: [{ op: "setProjectTrusted", args: [true] }],
});
await settingsCase("settings/trust-toggle-off", {
	global: JSON.stringify({ theme: "g" }),
	project: JSON.stringify({ theme: "p" }),
	ops: [{ op: "setProjectTrusted", args: [false] }],
});
await settingsCase("settings/apply-overrides", {
	global: JSON.stringify({ theme: "g", terminal: { showImages: false }, defaultTools: ["read"] }),
	ops: [{ op: "applyOverrides", args: [{ theme: "o", terminal: { clearOnShrink: true }, defaultTools: ["+edit"] }] }],
});
await settingsCase("settings/reload-after-change", {
	global: JSON.stringify({ theme: "g" }),
	ops: [{ op: "writeGlobal", args: ["{\"theme\":\"changed\"}"] }, { op: "reload" }],
});


// ---------------------------------------------------------------------------------------------
// Git URLs

const gitInputs = [
	"github.com/user/repo",
	"git:github.com/user/repo",
	"git:github.com/user/repo@v1.0.0",
	"git:github.com/user/repo.git",
	"git:github.com/user/repo#main",
	"git:git@github.com:user/repo.git",
	"git:git@github.com:user/repo@feature/x",
	"git@github.com:user/repo",
	"https://github.com/user/repo",
	"https://github.com/user/repo.git",
	"https://github.com/user/repo@v2",
	"https://github.com/user/repo/tree/main",
	"https://github.com/user/repo#dev",
	"HTTPS://GitHub.com/User/Repo",
	"http://github.com/user/repo",
	"ssh://git@github.com/user/repo.git",
	"ssh://git@github.com/user/repo.git@abc123",
	"git://github.com/user/repo",
	"git+ssh://git@github.com/user/repo.git",
	"git:gitlab.com/group/subgroup/project",
	"https://gitlab.com/group/subgroup/project.git",
	"https://gitlab.com/group/project/-/tree/main",
	"git:bitbucket.org/team/repo",
	"https://bitbucket.org/team/repo@v1",
	"git:gist.github.com/1234abcd",
	"https://gist.github.com/user/1234abcd",
	"git:git.sr.ht/~user/repo",
	"https://git.sr.ht/~user/repo",
	"git:example.com/team/repo",
	"git:example.com/team/repo@ref",
	"https://example.com/team/repo.git",
	"https://example.com/team/repo.git@v3",
	"https://example.com:8443/team/repo",
	"https://user:pass@example.com/team/repo",
	"ssh://git@example.com:2222/team/repo.git",
	"git:git@example.com:team/repo.git",
	"git:git@example.com:team/repo.git@main",
	"git:localhost/team/repo",
	"git:noperiod/team/repo",
	"git:example.com/repo",
	"https://example.com/repo",
	"https://example.com/team/../repo",
	"https://example.com/team/%2e%2e/repo",
	"https://example.com/team/%zz/repo",
	"git:example.com/team\\repo",
	"git:example.com//team/repo",
	"https://example.com/",
	"git:user/repo",
	"git:user/repo#v1",
	"git:github:user/repo",
	"git:gitlab:user/repo",
	"git:bitbucket:user/repo#dev",
	"npm:foo",
	"./local/path",
	"/abs/path",
	"just-text",
	"  git:github.com/user/repo  ",
	"git:  github.com/user/repo",
	"https://github.com/user",
	"https://github.com/user/repo/issues",
	"https://www.github.com/user/repo",
	"git:github.com/user/repo@",
	"git:github.com/user/@ref",
	"https://github.com/user/repo@",
	"git:git@github.com:user/repo@",
	"https://github.com/user/re%20po",
	"git:github.com/user/repo@v1#v2",
];
for (const input of gitInputs) add(`git/${input}`, "git_url", { source: input }, u(parseGitUrl(input)));

// ---------------------------------------------------------------------------------------------
// Package sources

const fakeCwd = "/home/golden/project";
const fakeAgentDir = "/home/golden/.pi/agent";
{
	const settings = SettingsManager.inMemory({});
	const pm = new DefaultPackageManager({ cwd: fakeCwd, agentDir: fakeAgentDir, settingsManager: settings });
	const sources = [
		"npm:foo",
		"npm:foo@1.2.3",
		"npm:foo@^1.2.0",
		"npm:foo@latest",
		"npm:@scope/pkg",
		"npm:@scope/pkg@2.0.0",
		"npm:@scope/pkg@>=1 <3",
		"npm:  bar@~1.0  ",
		"npm:foo@",
		"npm:@",
		"npm:",
		"npm:@scope@1.0.0",
		"npm:foo@1@2",
		"npm:foo@v1.0.0",
		"npm:foo@1.0.0-beta.1",
		"npm:foo@*",
		"npm:foo@x",
		"npm:foo@not a range",
		"./local",
		"../up",
		"/abs/dir",
		"relative/dir",
		"git:github.com/a/b",
		"https://github.com/a/b@v1",
		"github.com/a/b",
		"plain",
		"C:\\win\\path",
	];
	for (const source of sources) {
		add(`pm-source/${source}`, "pm_source", { source, cwd: fakeCwd, agentDir: fakeAgentDir }, {
			parsed: pm.parseSource(source),
			identity: pm.getPackageIdentity(source),
			identityUser: pm.getPackageIdentity(source, "user"),
			identityProject: pm.getPackageIdentity(source, "project"),
			identityTemporary: pm.getPackageIdentity(source, "temporary"),
			settingsUser: pm.normalizePackageSourceForSettings(source, "user"),
			settingsProject: pm.normalizePackageSourceForSettings(source, "project"),
			matchKey: pm.getSourceMatchKeyForInput(source),
		});
	}
	const configured = ["npm:foo@1.0.0", "git:github.com/a/b@v2", "./local", { source: "npm:@s/p", skills: [] }];
	for (const source of ["foo", "npm:bar", "@s/p", "github.com/a/b", "github.com/a/b@v2", "foo@1.0.0", "a/b"]) {
		add(`pm-no-match/${source}`, "pm_no_match", { source, configured, cwd: fakeCwd, agentDir: fakeAgentDir }, pm.buildNoMatchingPackageMessage(source, configured));
	}
	const npmCommands = [
		undefined,
		["npm"],
		["pnpm"],
		["/usr/local/bin/bun"],
		["mise", "exec", "node@20", "--", "npm"],
		["mise", "exec", "--"],
		["corepack", "pnpm"],
		["wrapper", "pnpm", "bun"],
		["npm.cmd"],
		["C:\\tools\\PNPM.EXE"],
		["yarn"],
		[""],
	];
	for (const [index, npmCommand] of npmCommands.entries()) {
		const s = SettingsManager.inMemory(npmCommand ? { npmCommand } : {});
		const manager = new DefaultPackageManager({ cwd: fakeCwd, agentDir: fakeAgentDir, settingsManager: s });
		add(`pm-npm-command/${index}`, "pm_npm_command", { npmCommand: u(npmCommand), cwd: fakeCwd, agentDir: fakeAgentDir }, {
			name: capture(() => manager.getPackageManagerName()),
			installArgs: capture(() => manager.getNpmInstallArgs(["a", "b@1"], "/root/x")),
			gitDependencyArgs: capture(() => manager.getGitDependencyInstallArgs()),
		});
	}
}

// ---------------------------------------------------------------------------------------------
// Package and resource resolution over file trees

async function resolveCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	if (input.global !== undefined) fs.writeFileSync(path.join(agentDir, "settings.json"), JSON.stringify(input.global));
	if (input.project !== undefined) {
		fs.mkdirSync(path.join(cwd, ".pi"), { recursive: true });
		fs.writeFileSync(path.join(cwd, ".pi", "settings.json"), JSON.stringify(input.project));
	}
	const settings = SettingsManager.create(cwd, agentDir, { projectTrusted: input.trusted ?? true });
	const pm = new DefaultPackageManager({
		cwd,
		agentDir,
		settingsManager: settings,
		builtinExtensions: input.builtins ?? [],
	});
	const keep = (resources) =>
		resources.filter((r) => r.path.startsWith(root) || r.path.startsWith("builtin:")).map((r) => ({ ...r }));
	const shape = (resolved) => ({
		extensions: keep(resolved.extensions),
		skills: keep(resolved.skills),
		prompts: keep(resolved.prompts),
		themes: keep(resolved.themes),
	});
	const out = {};
	try {
		out.resolved = shape(await pm.resolve(async () => "skip"));
	} catch (error) {
		out.resolved = { error: error.message };
	}
	if (input.cliSources) {
		try {
			out.cli = shape(await pm.resolveExtensionSources(input.cliSources, { temporary: true }));
		} catch (error) {
			out.cli = { error: error.message };
		}
	}
	out.configured = pm.listConfiguredPackages();
	for (const op of input.ops ?? []) {
		out.ops ??= [];
		try {
			const options = { local: op.local };
			const result =
				op.op === "add" ? pm.addSourceToSettings(op.source, options) : pm.removeSourceFromSettings(op.source, options);
			out.ops.push(result);
		} catch (error) {
			out.ops.push({ error: error.message });
		}
	}
	if (input.ops) {
		await settings.flush();
		out.globalFile = readIfExists(path.join(agentDir, "settings.json"));
		out.projectFile = readIfExists(path.join(cwd, ".pi", "settings.json"));
	}
	add(name, "pm_resolve", input, relativize(out, root));
}

const ext = "export default function () {}\n";
const skill = (name, description = `${name} skill`) => `---\nname: ${name}\ndescription: ${description}\n---\nBody of ${name}\n`;
const prompt = (description) => `---\ndescription: ${description}\n---\nPrompt body\n`;
const theme = (name) => JSON.stringify({ name, vars: {}, colors: {} });

await resolveCase("resolve/empty", {});
await resolveCase("resolve/auto-discovery", {
	files: {
		"agent/extensions/zeta.ts": ext,
		"agent/extensions/alpha.js": ext,
		"agent/extensions/readme.md": "x",
		"agent/extensions/dir-with-index/index.ts": ext,
		"agent/extensions/dir-with-index/helper.ts": ext,
		"agent/extensions/dir-with-manifest/package.json": JSON.stringify({ pi: { extensions: ["./src/main.ts", "./missing.ts"] } }),
		"agent/extensions/dir-with-manifest/src/main.ts": ext,
		"agent/extensions/dir-without-entry/other.ts": ext,
		"agent/extensions/.hidden.ts": ext,
		"agent/extensions/node_modules/dep/index.ts": ext,
		"agent/skills/top.md": skill("top"),
		"agent/skills/b-skill/SKILL.md": skill("b-skill"),
		"agent/skills/b-skill/nested/SKILL.md": skill("nested"),
		"agent/skills/a-group/inner/SKILL.md": skill("inner"),
		"agent/skills/a-group/loose.md": skill("loose"),
		"agent/prompts/one.md": prompt("one"),
		"agent/prompts/two.md": prompt("two"),
		"agent/prompts/sub/three.md": prompt("three"),
		"agent/prompts/notes.txt": "x",
		"agent/themes/t1.json": theme("t1"),
		"agent/themes/sub/t2.json": theme("t2"),
		"project/.pi/extensions/p.ts": ext,
		"project/.pi/skills/p-skill/SKILL.md": skill("p-skill"),
		"project/.pi/prompts/p.md": prompt("p"),
		"project/.pi/themes/p.json": theme("p"),
		"project/.agents/skills/agent-skill/SKILL.md": skill("agent-skill"),
		"project/.agents/skills/flat.md": skill("flat"),
		"project/.agents/skills/group/deep.md": skill("deep"),
	},
});
await resolveCase("resolve/auto-discovery-untrusted", {
	trusted: false,
	files: {
		"agent/extensions/u.ts": ext,
		"project/.pi/extensions/p.ts": ext,
		"project/.pi/skills/p-skill/SKILL.md": skill("p-skill"),
		"project/.agents/skills/agent-skill/SKILL.md": skill("agent-skill"),
	},
});
await resolveCase("resolve/ignore-files", {
	files: {
		"agent/skills/.gitignore": "ignored-skill/\n# comment\n\\#literal.md\n!keep.md\n/rooted.md\n",
		"agent/skills/ignored-skill/SKILL.md": skill("ignored-skill"),
		"agent/skills/kept-skill/SKILL.md": skill("kept-skill"),
		"agent/skills/#literal.md": skill("literal"),
		"agent/skills/rooted.md": skill("rooted"),
		"agent/skills/keep.md": skill("keep"),
		"agent/skills/group/.ignore": "*.md\n!SKILL.md\n",
		"agent/skills/group/sub/SKILL.md": skill("sub"),
		"agent/extensions/.fdignore": "skip.ts\nskipdir/\n",
		"agent/extensions/skip.ts": ext,
		"agent/extensions/take.ts": ext,
		"agent/extensions/skipdir/index.ts": ext,
		"agent/prompts/.gitignore": "draft*.md\n",
		"agent/prompts/draft-1.md": prompt("d"),
		"agent/prompts/final.md": prompt("f"),
	},
});
await resolveCase("resolve/settings-entries-and-patterns", {
	global: {
		extensions: ["./custom-ext", "!**/disabled.ts", "-custom-ext/forced-off.ts", "+extensions/auto-off.ts", "!extensions/auto-off.ts"],
		skills: ["./more-skills", "!**/hidden-skill"],
		prompts: ["./more-prompts/a.md", "./more-prompts", "./missing-dir"],
		themes: ["./more-themes", "-more-themes/x.json"],
	},
	project: {
		extensions: ["../extra.ts", "!**/p-off.ts"],
		skills: ["-skills/proj-off/SKILL.md"],
	},
	files: {
		"agent/custom-ext/a.ts": ext,
		"agent/custom-ext/disabled.ts": ext,
		"agent/custom-ext/forced-off.ts": ext,
		"agent/custom-ext/sub/index.ts": ext,
		"agent/extensions/auto-off.ts": ext,
		"agent/extensions/auto-on.ts": ext,
		"agent/more-skills/hidden-skill/SKILL.md": skill("hidden-skill"),
		"agent/more-skills/shown/SKILL.md": skill("shown"),
		"agent/more-skills/root.md": skill("root"),
		"agent/more-prompts/a.md": prompt("a"),
		"agent/more-prompts/b.md": prompt("b"),
		"agent/more-prompts/deep/c.md": prompt("c"),
		"agent/more-themes/x.json": theme("x"),
		"agent/more-themes/y.json": theme("y"),
		"project/extra.ts": ext,
		"project/.pi/extensions/p-off.ts": ext,
		"project/.pi/extensions/p-on.ts": ext,
		"project/.pi/skills/proj-off/SKILL.md": skill("proj-off"),
		"project/.pi/skills/proj-on/SKILL.md": skill("proj-on"),
	},
});
await resolveCase("resolve/local-packages", {
	global: {
		packages: [
			"./pkg-conventions",
			"./pkg-manifest",
			{ source: "./pkg-filtered", extensions: ["extensions/a.ts", "!extensions/b.ts"], skills: [], prompts: ["+prompts/forced.md", "-prompts/one.md"] },
			{ source: "./pkg-delta", autoload: false, extensions: ["+extensions/x.ts", "-extensions/y.ts"] },
			"./single-file.ts",
			"./empty-dir",
			"./missing-pkg",
		],
	},
	project: {
		packages: [{ source: "../agent/pkg-delta", autoload: false, skills: ["!*"] }, "../agent/pkg-conventions"],
	},
	files: {
		"agent/pkg-conventions/extensions/one.ts": ext,
		"agent/pkg-conventions/skills/s1/SKILL.md": skill("s1"),
		"agent/pkg-conventions/skills/top.md": skill("top-conv"),
		"agent/pkg-conventions/prompts/p1.md": prompt("p1"),
		"agent/pkg-conventions/themes/t.json": theme("conv"),
		"agent/pkg-manifest/package.json": JSON.stringify({
			name: "pkg-manifest",
			pi: {
				extensions: ["./src/*.ts", "!./src/skip.ts", "./other/entry.js"],
				skills: ["./my-skills"],
				prompts: ["./prompts/**/*.md"],
				themes: [],
			},
		}),
		"agent/pkg-manifest/src/a.ts": ext,
		"agent/pkg-manifest/src/skip.ts": ext,
		"agent/pkg-manifest/src/.hidden.ts": ext,
		"agent/pkg-manifest/other/entry.js": ext,
		"agent/pkg-manifest/my-skills/m/SKILL.md": skill("m"),
		"agent/pkg-manifest/prompts/x/y.md": prompt("y"),
		"agent/pkg-manifest/prompts/z.md": prompt("z"),
		"agent/pkg-manifest/themes/ignored.json": theme("ignored"),
		"agent/pkg-filtered/extensions/a.ts": ext,
		"agent/pkg-filtered/extensions/b.ts": ext,
		"agent/pkg-filtered/extensions/c.ts": ext,
		"agent/pkg-filtered/skills/s/SKILL.md": skill("filtered"),
		"agent/pkg-filtered/prompts/one.md": prompt("one"),
		"agent/pkg-filtered/prompts/forced.md": prompt("forced"),
		"agent/pkg-delta/extensions/x.ts": ext,
		"agent/pkg-delta/extensions/y.ts": ext,
		"agent/pkg-delta/extensions/z.ts": ext,
		"agent/pkg-delta/skills/d/SKILL.md": skill("d"),
		"agent/single-file.ts": ext,
		"agent/empty-dir/readme.txt": "x",
	},
});
await resolveCase("resolve/installed-npm-and-git", {
	global: {
		packages: [
			"npm:installed-pkg",
			"npm:@scope/scoped@^1.0.0",
			"npm:pinned@2.0.0",
			"git:github.com/owner/repo",
			{ source: "git:github.com/owner/filtered@v1", extensions: [] },
		],
	},
	project: { packages: ["npm:project-missing", "git:example.com/team/missing"] },
	files: {
		"agent/npm/node_modules/installed-pkg/package.json": JSON.stringify({ name: "installed-pkg", version: "1.0.0" }),
		"agent/npm/node_modules/installed-pkg/extensions/e.ts": ext,
		"agent/npm/node_modules/@scope/scoped/package.json": JSON.stringify({ name: "@scope/scoped", version: "1.5.0", pi: { skills: ["./s"] } }),
		"agent/npm/node_modules/@scope/scoped/s/x/SKILL.md": skill("x"),
		"agent/npm/node_modules/pinned/package.json": JSON.stringify({ name: "pinned", version: "1.0.0" }),
		"agent/npm/node_modules/pinned/extensions/e.ts": ext,
		"agent/git/github.com/owner/repo/extensions/g.ts": ext,
		"agent/git/github.com/owner/repo/prompts/gp.md": prompt("gp"),
		"agent/git/github.com/owner/filtered/extensions/f.ts": ext,
	},
});
await resolveCase("resolve/project-wins-dedupe", {
	global: { packages: ["../shared", "npm:dup"] },
	project: { packages: ["../../shared", { source: "npm:dup", extensions: ["!*"] }] },
	files: {
		"shared/extensions/s.ts": ext,
		"agent/npm/node_modules/dup/package.json": JSON.stringify({ name: "dup", version: "1.0.0" }),
		"agent/npm/node_modules/dup/extensions/d.ts": ext,
		"project/.pi/npm/node_modules/dup/package.json": JSON.stringify({ name: "dup", version: "1.0.0" }),
		"project/.pi/npm/node_modules/dup/extensions/d.ts": ext,
	},
});
await resolveCase("resolve/collisions-and-symlinks", {
	global: { skills: ["./linked", "./skills/real"], packages: ["./pkg"] },
	project: { skills: ["../../agent/skills/real"] },
	files: {
		"agent/skills/real/SKILL.md": skill("real"),
		"agent/linked": { symlink: "skills" },
		"agent/pkg/skills/real/SKILL.md": skill("real"),
		"agent/extensions/dangling.ts": { symlink: "nowhere.ts" },
		"agent/extensions/link.ts": { symlink: "../pkg/skills/real/SKILL.md" },
	},
});
await resolveCase("resolve/builtins", {
	builtins: ["mcp", "codemode", "llama"],
	global: { extensions: ["-builtin:llama", "!builtin:codemode"] },
	project: { extensions: ["+builtin:codemode"] },
	cliSources: ["builtin:mcp", "builtin:unknown", "./cli-ext.ts", "./cli-pkg"],
	files: {
		"project/cli-ext.ts": ext,
		"project/cli-pkg/extensions/c.ts": ext,
		"project/cli-pkg/skills/c/SKILL.md": skill("c"),
	},
});
await resolveCase("resolve/settings-ops", {
	global: { packages: ["npm:foo@1.0.0", { source: "./local-pkg", skills: [] }] },
	project: { packages: ["git:github.com/a/b"] },
	ops: [
		{ op: "add", source: "npm:foo@2.0.0" },
		{ op: "add", source: "npm:foo@2.0.0" },
		{ op: "add", source: "./agent/local-pkg" },
		{ op: "add", source: "./new-local" },
		{ op: "add", source: "https://github.com/a/b@v3", local: true },
		{ op: "add", source: "./project-local", local: true },
		{ op: "remove", source: "npm:foo" },
		{ op: "remove", source: "npm:missing" },
		{ op: "remove", source: "git:github.com/a/b", local: true },
	],
	files: { "agent/local-pkg/extensions/l.ts": ext },
});

// ---------------------------------------------------------------------------------------------
// Skills, prompt templates and context files

function skillsCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	const result = loadSkills({ cwd, agentDir, skillPaths: input.skillPaths ?? [], includeDefaults: input.includeDefaults ?? true });
	add(name, "skills", input, relativize(result, root));
}

skillsCase("skills/defaults", {
	files: {
		"agent/skills/alpha/SKILL.md": skill("alpha"),
		"agent/skills/bad name/SKILL.md": `---\nname: Bad_Name--\ndescription: "${"d".repeat(1030)}"\n---\n`,
		"agent/skills/no-desc/SKILL.md": "---\nname: no-desc\n---\nbody",
		"agent/skills/empty-desc/SKILL.md": "---\ndescription: '   '\n---\n",
		"agent/skills/fallback-name/SKILL.md": "---\ndescription: uses dir name\ndisable-model-invocation: true\n---\n",
		"agent/skills/root-file.md": "---\ndescription: root file skill\n---\n",
		"agent/skills/root-no-desc.md": "# just markdown",
		"agent/skills/broken/SKILL.md": "---\nname: [unterminated\n---\n",
		"agent/skills/broken-root.md": "---\ndescription: [unterminated\n---\n",
		"agent/skills/nested/deeper/SKILL.md": skill("deeper"),
		"agent/skills/nested/ignored.md": skill("ignored-nested"),
		"agent/skills/disabled-true-string/SKILL.md": "---\ndescription: x\ndisable-model-invocation: \"true\"\n---\n",
		"agent/skills/numeric-name/SKILL.md": "---\nname: 123\ndescription: numeric\n---\n",
		"agent/skills/crlf/SKILL.md": "---\r\nname: crlf\r\ndescription: windows\r\n---\r\nbody",
		"agent/skills/bom/SKILL.md": "\uFEFF---\nname: bom\ndescription: bom skill\n---\n",
		"agent/skills/long-name/SKILL.md": `---\nname: ${"a".repeat(70)}\ndescription: long\n---\n`,
		"project/.pi/skills/alpha/SKILL.md": skill("alpha", "project alpha"),
		"project/.pi/skills/beta/SKILL.md": skill("beta"),
	},
});
skillsCase("skills/explicit-paths", {
	includeDefaults: false,
	skillPaths: ["./a", "../agent/skills", "./file.md", "./not-md.txt", "./missing", "  ./trimmed  ", "./a/one/SKILL.md"],
	files: {
		"project/a/one/SKILL.md": skill("one"),
		"project/a/two.md": skill("two"),
		"project/file.md": skill("file"),
		"project/not-md.txt": "x",
		"project/trimmed/t/SKILL.md": skill("t"),
		"agent/skills/u/SKILL.md": skill("u"),
		"project/.pi/skills/q/SKILL.md": skill("q"),
	},
});
skillsCase("skills/symlink-duplicates", {
	includeDefaults: false,
	skillPaths: ["./real", "./link", "./other"],
	files: {
		"project/real/s/SKILL.md": skill("s"),
		"project/link": { symlink: "real" },
		"project/other/s/SKILL.md": skill("s", "other s"),
	},
});

function promptsCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	const result = loadPromptTemplates({ cwd, agentDir, promptPaths: input.promptPaths ?? [], includeDefaults: input.includeDefaults ?? true });
	add(name, "prompts", input, relativize(result, root));
}

promptsCase("prompts/defaults", {
	promptPaths: ["./extra", "./single.md", "./missing", "./not-md.txt", "../agent/prompts/a.md"],
	files: {
		"agent/prompts/a.md": "---\ndescription: from frontmatter\nargument-hint: \"<file>\"\n---\nBody $1",
		"agent/prompts/b.md": "\n\n   \nFirst real line that is definitely longer than sixty characters in total length\nsecond",
		"agent/prompts/c.md": "---\ndescription: ''\nargument-hint: ''\n---\n\nfallback line",
		"agent/prompts/d.md": "---\ninvalid: [yaml\n---\nbody",
		"agent/prompts/e.md": "",
		"agent/prompts/sub/nested.md": "nested",
		"agent/prompts/f.md.txt": "no",
		"agent/prompts/g.MD": "upper",
		"agent/prompts/emoji.md": `${"😀".repeat(40)}`,
		"project/.pi/prompts/a.md": "project a",
		"project/extra/x.md": "extra x",
		"project/single.md": "---\ndescription: 5\n---\nnumeric description",
		"project/not-md.txt": "x",
	},
});

function contextCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, input.cwd ?? "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	const result = loadProjectContextFiles({ cwd, agentDir }).filter((file) => file.path.startsWith(root));
	add(name, "context_files", input, relativize(result, root));
}

contextCase("context/ancestors", {
	cwd: "repo/sub/dir",
	files: {
		"agent/AGENTS.md": "global",
		"AGENTS.md": "root",
		"repo/CLAUDE.md": "repo claude",
		"repo/AGENTS.md": "repo agents",
		"repo/sub/AGENTS.override.md": "override",
		"repo/sub/AGENTS.md": "shadowed by override",
		"repo/sub/dir/CLAUDE.MD": "upper claude",
		"repo/sub/dir/AGENTS.MD/": { dir: true },
	},
});
contextCase("context/directory-candidate", {
	files: {
		"project/AGENTS.md/inner.txt": "dir named like a context file",
		"project/CLAUDE.md": "claude",
		"agent/CLAUDE.md": "\uFEFFglobal claude",
	},
});

const { DefaultResourceLoader } = await ref("coding-agent/src/core/resource-loader.ts");
async function loaderCase(name, input) {
	const root = makeTree(input.files);
	const cwd = path.join(root, "project");
	const agentDir = path.join(root, "agent");
	fs.mkdirSync(cwd, { recursive: true });
	fs.mkdirSync(agentDir, { recursive: true });
	const settingsManager = SettingsManager.create(cwd, agentDir, { projectTrusted: input.trusted ?? true });
	const loader = new DefaultResourceLoader({
		cwd,
		agentDir,
		settingsManager,
		noExtensions: true,
		additionalSkillPaths: input.skillPaths,
		additionalPromptTemplatePaths: input.promptPaths,
		additionalThemePaths: input.themePaths,
		noSkills: input.noSkills,
		noPromptTemplates: input.noPrompts,
		noThemes: input.noThemes,
		noContextFiles: input.noContextFiles,
		systemPrompt: input.systemPrompt,
		appendSystemPrompt: input.appendSystemPrompt,
	});
	await loader.reload();
	const skills = loader.getSkills();
	const prompts = loader.getPrompts();
	const themes = loader.getThemes();
	const out = {
		skills: skills.skills.map((s) => ({ name: s.name, filePath: s.filePath, sourceInfo: s.sourceInfo })),
		skillDiagnostics: skills.diagnostics,
		prompts: prompts.prompts.map((p) => ({ name: p.name, filePath: p.filePath, sourceInfo: p.sourceInfo })),
		promptDiagnostics: prompts.diagnostics,
		themes: themes.themes.map((t) => ({ name: u(t.name), sourcePath: u(t.sourcePath), sourceInfo: u(t.sourceInfo) })),
		themeDiagnostics: themes.diagnostics,
		agentsFiles: loader.getAgentsFiles().agentsFiles.filter((f) => f.path.startsWith(root)),
		systemPrompt: u(loader.getSystemPrompt()),
		systemPromptSource: u(loader.getSystemPromptSource()),
		appendSystemPrompt: loader.getAppendSystemPrompt(),
		appendSystemPromptSources: loader.getAppendSystemPromptSources(),
	};
	add(name, "resource_loader", input, relativize(out, root));
}
const goodTheme = (name) => fs.readFileSync(path.join(REF, "packages/coding-agent/src/modes/interactive/theme/dark.json"), "utf-8").replace('"name": "dark"', `"name": "${name}"`);
await loaderCase("loader/full", {
	skillPaths: ["./cli-skills", "./missing-skills"],
	promptPaths: ["./cli-prompts/x.md", "./missing.md"],
	themePaths: ["./cli-themes", "./missing-theme.json"],
	files: {
		"agent/skills/user-skill/SKILL.md": skill("user-skill"),
		"agent/skills/dup/SKILL.md": skill("dup", "user dup"),
		"project/.pi/skills/dup/SKILL.md": skill("dup", "project dup"),
		"project/.agents/skills/agents-skill/SKILL.md": skill("agents-skill"),
		"project/cli-skills/cli-skill/SKILL.md": skill("cli-skill"),
		"agent/prompts/p.md": prompt("user p"),
		"project/.pi/prompts/p.md": prompt("project p"),
		"project/cli-prompts/x.md": prompt("x"),
		"agent/themes/mine.json": goodTheme("mine"),
		"agent/themes/broken.json": "{ nope",
		"project/.pi/themes/mine.json": goodTheme("mine"),
		"project/cli-themes/other.json": goodTheme("other"),
		"agent/AGENTS.md": "global agents",
		"project/AGENTS.md": "project agents",
		"agent/SYSTEM.md": "global system",
		"project/.pi/APPEND_SYSTEM.md": "project append",
		"agent/APPEND_SYSTEM.md": "global append",
	},
});
await loaderCase("loader/untrusted", {
	trusted: false,
	files: {
		"project/.pi/skills/p/SKILL.md": skill("p"),
		"project/.pi/SYSTEM.md": "project system",
		"agent/SYSTEM.md": "global system",
		"project/.pi/APPEND_SYSTEM.md": "project append",
	},
});
await loaderCase("loader/disabled", {
	noSkills: true,
	noPrompts: true,
	noThemes: true,
	noContextFiles: true,
	skillPaths: ["./s"],
	systemPrompt: "inline system prompt",
	appendSystemPrompt: ["./append.md", "inline append"],
	files: {
		"project/s/x/SKILL.md": skill("x"),
		"agent/skills/u/SKILL.md": skill("u"),
		"project/append.md": "\uFEFFfile append",
		"project/AGENTS.md": "agents",
	},
});

// ---------------------------------------------------------------------------------------------
// Prompt template arguments

const argInputs = [
	"",
	"one two  three",
	"\"quoted arg\" 'single quoted' plain",
	"\"\" '' x",
	"mixed\"quo ted\"word",
	"unterminated \"quote here",
	"tab\tseparated\nnewline",
	"nbsp\u00a0separated",
	"nel\u0085not-space",
	"emoji 😀 args",
	"'it''s'",
];
for (const input of argInputs) add(`args/${JSON.stringify(input)}`, "parse_args", { input }, parseCommandArgs(input));

const templates = [
	"$1 and $2",
	"$@ | $ARGUMENTS",
	"${1:-default} ${3:-fallback}",
	"${@:-none} ${ARGUMENTS:-none}",
	"${@:2} | ${@:2:1} | ${@:0} | ${@:5}",
	"$10 $0 $1$2",
	"${10:-ten}",
	"$ARGUMENTSX $@@",
	"literal $ sign and ${unknown}",
	"${1:-a}}",
	"${@:1:0}",
	"${@:99999999999999999999}",
];
const argSets = [[], ["a"], ["a", "b", "c"], ["", "b"], ["$1", "${@:1}"]];
for (const template of templates) {
	for (const [index, args] of argSets.entries()) {
		add(`substitute/${template}/${index}`, "substitute", { template, args }, substituteArgs(template, args));
	}
}

const expandTemplates = [
	{ name: "review", content: "Review $1 with ${2:-care}: $@", description: "", sourceInfo: {}, filePath: "/x/review.md" },
	{ name: "dup", content: "first", description: "", sourceInfo: {}, filePath: "/x/dup.md" },
	{ name: "dup", content: "second", description: "", sourceInfo: {}, filePath: "/x/dup2.md" },
];
for (const text of ["/review a.ts b", "/review", "/review   spaced   args  ", "/review\nmultiline\nargs", "/unknown x", "review x", "/dup", "/", "/ review", "/review\u00a0nbsp", "/review\u0085nel"]) {
	add(`expand/${JSON.stringify(text)}`, "expand", { text, templates: expandTemplates.map(({ name, content }) => ({ name, content })) }, expandPromptTemplate(text, expandTemplates));
}

// ---------------------------------------------------------------------------------------------
// Model references and scopes

const catalog = getBuiltinProviders().flatMap((provider) => getBuiltinModels(provider));
const key = (model) => `${model.provider}/${model.id}`;
function shapeReference(result) {
	const out = { status: result.status };
	if (result.status === "resolved") {
		out.model = key(result.model);
	} else if (result.status === "ambiguous") {
		out.candidates = result.candidates.map(key);
		out.usableCount = result.usableCount;
	} else if (result.target) {
		out.target = {
			provider: result.target.provider,
			providerModels: result.target.providerModels.map(key),
			modelText: result.target.modelText,
			modelId: result.target.modelId,
			thinkingLevel: u(result.target.thinkingLevel),
		};
	}
	if (result.status !== "not-found") {
		out.thinkingLevel = u(result.thinkingLevel);
		out.warning = u(result.warning);
	}
	return out;
}

const references = [
	"claude-opus-4-8",
	"anthropic/claude-opus-4-8",
	"ANTHROPIC/Claude-Opus-4-8",
	"claude-opus-4-8:high",
	"claude-opus-4-8:bogus",
	"opus",
	"sonnet",
	"haiku",
	"gpt-5.5",
	"gpt-5",
	"gpt",
	"openai/gpt-5.5:xhigh",
	"openrouter/free",
	"qwen/qwen3-coder:exacto",
	"qwen3-coder",
	"zai/glm-5",
	"glm-5.1",
	"gemini",
	"gemini-3.1-pro-preview",
	"kimi-k2.6",
	"moonshotai/kimi-k2.6",
	"openai/unknown-model",
	"openai/unknown-model:high",
	"openai/unknown-model:bogus",
	"totally-unknown",
	"unknown-provider/model",
	"  claude-opus-4-8  ",
	"",
	"deepseek",
	"grok",
	"llama",
	"mistral",
	"o3",
	"o4-mini",
	"4o",
	"claude",
	"anthropic/opus",
	"openai/gpt-5.5:off",
	"nvidia/nemotron-3.5-super-vl-120b-a12b",
	"bedrock",
	"amazon-bedrock/claude",
];
const usableSets = { none: [], anthropic: ["anthropic"], openai: ["openai", "openrouter"], all: null };
for (const text of references) {
	for (const thinkingSuffix of ["none", "strict", "lenient"]) {
		for (const fuzzy of [false, true]) {
			for (const [usableName, usableProviders] of Object.entries(usableSets)) {
				const request = { text, thinkingSuffix, fuzzy };
				const usable = (provider) => usableProviders === null || usableProviders.includes(provider);
				add(
					`model-ref/${text}/${thinkingSuffix}/${fuzzy}/${usableName}`,
					"model_reference",
					{ request, usable: usableProviders },
					shapeReference(resolveModelReference(request, catalog, usable)),
				);
			}
		}
	}
}
for (const [text, provider] of [
	["gpt-5.5", "openai"],
	["gpt-5.5", "OpenAI"],
	["claude-opus-4-8", "openai"],
	["custom-id", "anthropic"],
	["custom-id:high", "anthropic"],
	["sonnet", "anthropic"],
	["x", "no-such-provider"],
]) {
	for (const thinkingSuffix of ["none", "strict"]) {
		const request = { text, provider, thinkingSuffix, fuzzy: true };
		add(`model-ref-provider/${provider}/${text}/${thinkingSuffix}`, "model_reference", { request, usable: null }, shapeReference(resolveModelReference(request, catalog, () => true)));
	}
}

const scopePatterns = [
	["anthropic/*"],
	["*sonnet*"],
	["*opus*:high"],
	["claude-opus-4-8", "anthropic/claude-opus-4-8", "opus"],
	["gpt-5.5:low", "gpt-5.5:bogus"],
	["nothing-matches", "*nothing*"],
	["openai/gpt-5.?"],
	["openai/[g]pt-5.5"],
	["*/kimi-k2.6"],
	["gemini-3.1-pro-preview"],
	["glm-5.1"],
	["anthropic/*:off", "openai/*:xhigh"],
	["{anthropic,openai}/claude-opus-4-*"],
	["**"],
];
for (const patterns of scopePatterns) {
	const result = resolveModelScopeFromModels(patterns, catalog);
	add(`model-scope/${patterns.join(",")}`, "model_scope", { patterns }, {
		scopedModels: result.scopedModels.map((scoped) => ({ model: key(scoped.model), thinkingLevel: u(scoped.thinkingLevel) })),
		diagnostics: result.diagnostics,
	});
}

// ---------------------------------------------------------------------------------------------
// models.json

const { ModelConfig } = await ref("coding-agent/src/core/model-config.ts");
const modelsJsonInputs = {
	valid: JSON.stringify({
		providers: {
			ollama: {
				baseUrl: "http://localhost:11434/v1",
				api: "openai-completions",
				apiKey: "ollama",
				models: [{ id: "llama3", name: "Llama 3", contextWindow: 8192, reasoning: false, input: ["text", "image"] }],
			},
			anthropic: { modelOverrides: { "claude-opus-4-8": { name: "Opus", maxTokens: 1000 } }, headers: { "x-a": "b" } },
			"1": { name: "numeric id" },
		},
	}),
	comments: '{\n  // comment\n  "providers": { /* block */ "x": { "baseUrl": "https://x.dev" } },\n}',
	bom: '\uFEFF{"providers": {}}',
	syntax: '{"providers": {',
	notObject: "[]",
	missingProviders: "{}",
	wrongTypes: JSON.stringify({ providers: { a: { baseUrl: 5, models: [{ name: "no id" }], authHeader: "yes" } } }),
	emptyStrings: JSON.stringify({ providers: { a: { name: "", apiKey: "", models: [{ id: "" }] } } }),
	badModalities: JSON.stringify({ providers: { a: { models: [{ id: "m", input: ["text", "video"], contextWindow: 0 }] } } }),
	badOauth: JSON.stringify({ providers: { a: { oauth: "github", baseUrl: "https://r" } } }),
	extraKeys: JSON.stringify({ $schema: "x", other: 1, providers: { a: { unknown: true } } }),
};
for (const [label, content] of Object.entries(modelsJsonInputs)) {
	const root = makeTree({ "models.json": content });
	const config = await ModelConfig.load(path.join(root, "models.json"));
	add(`models-json/${label}`, "model_config", { content }, relativize({
		error: u(config.getError()),
		providerIds: config.getProviderIds(),
		providers: Object.fromEntries(config.getProviderIds().map((id) => {
			const provider = config.getProvider(id);
			return [id, { baseUrl: u(provider.baseUrl), api: u(provider.api), models: (provider.models ?? []).map((m) => m.id), overrides: Object.keys(provider.modelOverrides ?? {}) }];
		})),
	}, root));
}

// Provider composition from models.json (no extensions).
const { composeModelProvider } = await ref("coding-agent/src/core/provider-composer.ts");
const { builtinProviders } = await ref("ai/src/providers/all.ts");
const builtinById = new Map(builtinProviders().map((provider) => [provider.id, provider]));
const projectModel = (m) => ({
	id: m.id,
	name: m.name,
	api: m.api,
	baseUrl: m.baseUrl,
	reasoning: m.reasoning,
	input: m.input,
	contextWindow: m.contextWindow,
	maxTokens: m.maxTokens,
	cost: m.cost,
	compat: u(m.compat),
	thinkingLevelMap: u(m.thinkingLevelMap),
	samplingParams: u(m.samplingParams),
	samplingParamsByThinkingLevel: u(m.samplingParamsByThinkingLevel),
	promptCache: u(m.promptCache),
	inputLimits: u(m.inputLimits),
});
const composeInputs = [
	["custom-provider", "ollama", { providers: { ollama: { baseUrl: "http://localhost:11434/v1", api: "openai-completions", apiKey: "x", compat: { supportsDeveloperRole: false }, models: [{ id: "llama3" }, { id: "qwen", name: "Qwen", reasoning: true, input: ["text", "image"], contextWindow: 32000, maxTokens: 4096, cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0 }, compat: { supportsStore: false } }] } } }],
	["custom-no-api", "local", { providers: { local: { baseUrl: "http://x", apiKey: "k", models: [{ id: "m" }] } } }],
	["custom-no-base-url", "local", { providers: { local: { api: "openai-completions", apiKey: "k", models: [{ id: "m" }] } } }],
	["empty-config", "anthropic", { providers: { anthropic: {} } }],
	["oauth-without-base-url", "x", { providers: { x: { oauth: "radius" } } }],
	["no-auth", "nothing", { providers: { nothing: { baseUrl: "http://x", api: "openai-completions", models: [{ id: "m" }] } } }],
	["builtin-overrides", "anthropic", { providers: { anthropic: { modelOverrides: { "claude-opus-4-8": { name: "Opus Custom", maxTokens: 1000, cost: { input: 9 }, thinkingLevelMap: { minimal: null }, samplingParams: { top_k: 5 }, promptCache: { short: 300, long: 3600 } }, "missing-model": { name: "x" } } } } }],
	["builtin-base-url-and-custom", "openai", { providers: { openai: { baseUrl: "https://proxy.example/v1", compat: { openRouterRouting: { order: ["a"] } }, models: [{ id: "gpt-5.5", name: "Replaced" }, { id: "brand-new", api: "openai-responses" }] } } }],
	["builtin-headers-only", "groq", { providers: { groq: { headers: { "x-test": "1" } } } }],
];
for (const [label, providerId, json] of composeInputs) {
	const root = makeTree({ "models.json": JSON.stringify(json) });
	const config = await ModelConfig.load(path.join(root, "models.json"));
	let out;
	try {
		const provider = composeModelProvider(providerId, builtinById.get(providerId), config, undefined);
		out = { name: provider.name, baseUrl: u(provider.baseUrl), models: provider.getModels().map(projectModel) };
	} catch (error) {
		out = { error: error.message };
	}
	add(`compose/${label}`, "compose", { providerId, content: JSON.stringify(json) }, out);
}

// Configured request auth status and compatibility request config.
const { configuredRequestAuthStatus, resolveCompatibilityRequestConfig } = await ref("coding-agent/src/core/provider-composer.ts");
process.env.PI_GOLDEN_SET_KEY = "secret";
process.env.PI_GOLDEN_EMPTY_KEY = "";
delete process.env.PI_GOLDEN_UNSET_KEY;
const authProviders = {
	literal: { apiKey: "sk-literal", baseUrl: "http://x" },
	envSet: { apiKey: "$PI_GOLDEN_SET_KEY" },
	envBraced: { apiKey: "${PI_GOLDEN_SET_KEY}" },
	envUnset: { apiKey: "$PI_GOLDEN_UNSET_KEY" },
	envEmpty: { apiKey: "$PI_GOLDEN_EMPTY_KEY" },
	envMixed: { apiKey: "pre-${PI_GOLDEN_SET_KEY}-${PI_GOLDEN_UNSET_KEY}" },
	bareEnvName: { apiKey: "PI_GOLDEN_SET_KEY" },
	command: { apiKey: "!echo hi" },
	none: { baseUrl: "http://x" },
	headers: { baseUrl: "http://x", headers: { "x-a": "$PI_GOLDEN_SET_KEY", "x-b": "literal" }, authHeader: true },
	modelHeaders: { baseUrl: "http://x", models: [{ id: "m", api: "openai-completions", headers: { "x-m": "model" } }], headers: { "x-p": "provider" } },
};
for (const [label, provider] of Object.entries(authProviders)) {
	const root = makeTree({ "models.json": JSON.stringify({ providers: { p: provider } }) });
	const config = await ModelConfig.load(path.join(root, "models.json"));
	const providerConfig = config.getProvider("p");
	const model = { id: "m", provider: "p", api: "openai-completions", headers: label === "modelHeaders" ? undefined : { "x-model-own": "1" } };
	add(`auth/${label}`, "auth_status", { content: JSON.stringify({ providers: { p: provider } }) }, {
		error: u(config.getError()),
		status: u(configuredRequestAuthStatus(providerConfig, undefined)),
		compat: capture(() => resolveCompatibilityRequestConfig(model, providerConfig, undefined)),
	});
}

// ---------------------------------------------------------------------------------------------
// Keybindings

const keybindingInputs = {
	missing: undefined,
	empty: "{}",
	invalid: "{ not json",
	array: "[\"ctrl+a\"]",
	null: "null",
	overrides: JSON.stringify({
		"app.model.select": "ctrl+k",
		"app.clear": ["ctrl+c", "ctrl+c", "escape"],
		"tui.editor.undo": [],
		"app.unknown": "ctrl+u",
		$schema: "https://pi.dev/schemas/keybindings.schema.json",
	}),
	legacy: JSON.stringify({ selectModel: "ctrl+k", cursorUp: ["up", "ctrl+p"], interrupt: "ctrl+]", "app.interrupt": "escape" }),
	conflicts: JSON.stringify({ "app.clear": "ctrl+k", "app.model.select": "ctrl+k", "tui.editor.cursorUp": ["ctrl+k", "up"] }),
	invalidValues: JSON.stringify({ "app.clear": 5, "app.exit": ["ctrl+d", 3], "app.suspend": "not-a-key", "app.model.select": "ctrl+shift+alt+super+x" }),
	bom: "\uFEFF{\"app.clear\": \"ctrl+q\"}",
};
for (const [label, content] of Object.entries(keybindingInputs)) {
	const root = makeTree(content === undefined ? {} : { "agent/keybindings.json": content });
	const manager = KeybindingsManager.create(path.join(root, "agent"));
	let migrated;
	try {
		const parsed = JSON.parse((content ?? "{}").replace(/^\uFEFF/, ""));
		migrated = typeof parsed === "object" && parsed !== null ? migrateKeybindingsConfig(parsed) : "<not object>";
	} catch {
		migrated = "<invalid json>";
	}
	add(`keybindings/${label}`, "keybindings", { content: u(content) }, {
		effective: manager.getEffectiveConfig(),
		conflicts: manager.getConflicts(),
		migrated,
	});
}

fs.rmSync(scratch, { recursive: true, force: true });
fs.mkdirSync(OUT_DIR, { recursive: true });
fs.writeFileSync(path.join(OUT_DIR, "config.json"), `${JSON.stringify({ cases }, null, 1)}\n`);
console.log(`config.json: ${cases.length} cases`);
