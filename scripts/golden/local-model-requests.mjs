// Capture the openai-completions request bodies TS Pi sends for the local-model fixtures.
//
//   PI_TS_NODE_MODULES=/path/to/pi-1.4.0/node_modules node scripts/golden/local-model-requests.mjs
//
// PI_TS_NODE_MODULES is a node_modules directory holding the released @earendil-works packages
// (for example the one of an installed Pi release). The script loads
// crates/pi-coding-agent/tests/fixtures/local_models/models.json through the TS ModelRuntime,
// points every provider at a local mock server, sends context.json at each thinking level through
// `runtime.streamSimple`, and writes the raw bodies the server received to request_bodies.json.
// Nothing leaves 127.0.0.1. `tests/local_model_requests.rs` replays the same cases in Rust.
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const modulesDir = process.env.PI_TS_NODE_MODULES;
if (!modulesDir) throw new Error("set PI_TS_NODE_MODULES to a node_modules dir with @earendil-works/pi-coding-agent");
const load = (pkg) => import(pathToFileURL(join(modulesDir, "@earendil-works", pkg, "dist", "index.js")).href);
const { ModelRuntime } = await load("pi-coding-agent");
const piAiVersion = JSON.parse(readFileSync(join(modulesDir, "@earendil-works", "pi-ai", "package.json"), "utf8")).version;

const fixtures = join(dirname(fileURLToPath(import.meta.url)), "../../crates/pi-coding-agent/tests/fixtures/local_models");
export const LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const API_KEY = "sk-local-placeholder";

const bodies = [];
const server = createServer((req, res) => {
	let body = "";
	req.setEncoding("utf8");
	req.on("data", (chunk) => (body += chunk));
	req.on("end", () => {
		bodies.push({ path: req.url, body });
		const chunk = { id: "chatcmpl-local", object: "chat.completion.chunk", model: "m", choices: [{ index: 0, delta: { content: "ok" }, finish_reason: "stop" }] };
		res.writeHead(200, { "content-type": "text/event-stream" });
		res.end(`data: ${JSON.stringify(chunk)}\n\ndata: [DONE]\n\n`);
	});
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const baseUrl = `http://127.0.0.1:${server.address().port}`;

const dir = mkdtempSync(join(tmpdir(), "pi-local-model-requests-"));
const models = JSON.parse(readFileSync(join(fixtures, "models.json"), "utf8").replaceAll("{{BASE_URL}}", baseUrl));
for (const provider of Object.values(models.providers)) provider.apiKey = API_KEY;
writeFileSync(join(dir, "models.json"), JSON.stringify(models, null, 2));
const runtime = await ModelRuntime.create({
	authPath: join(dir, "auth.json"),
	modelsPath: join(dir, "models.json"),
	modelsStorePath: join(dir, "models-store.json"),
	allowModelNetwork: false,
});
const contextTemplate = readFileSync(join(fixtures, "context.json"), "utf8");

const cases = [];
for (const [providerId, provider] of Object.entries(models.providers)) {
	for (const { id } of provider.models) {
		const model = runtime.getModels(providerId).find((m) => m.id === id);
		if (!model) throw new Error(`model ${providerId}/${id} not loaded`);
		const context = JSON.parse(contextTemplate.replaceAll("{{PROVIDER}}", providerId).replaceAll("{{MODEL}}", id));
		for (const level of LEVELS) {
			const before = bodies.length;
			const options = level === "off" ? {} : { reasoning: level };
			const result = await runtime.streamSimple(model, context, options).result();
			if (result.stopReason !== "stop") throw new Error(`${providerId}/${id}@${level}: ${result.errorMessage}`);
			if (bodies.length !== before + 1) throw new Error(`${providerId}/${id}@${level}: expected one request`);
			const { path, body } = bodies[before];
			cases.push({ provider: providerId, model: id, level, path, body });
		}
	}
}
server.close();

const output = { generatedBy: `@earendil-works/pi-ai ${piAiVersion}`, apiKey: API_KEY, cases };
writeFileSync(join(fixtures, "request_bodies.json"), `${JSON.stringify(output, null, "\t")}\n`);
console.log(`wrote ${cases.length} request bodies`);
