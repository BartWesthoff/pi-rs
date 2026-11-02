// pi-ai core for extensions running in pi-jsrt (virtual module `pi-rs:ai-core`).
//
// JavaScript ports of the side-effect-free parts of `@earendil-works/pi-ai` that extensions
// call with JavaScript objects (streams, providers, credential stores, transcripts): event
// streams, abort helpers, transcripts, JSON repair, overflow and retry classification, client
// identity, auth resolution, credential and model stores, `createProvider()` and
// `createModels()`. Tool-argument validation and partial-JSON parsing use the Rust pi-ai crate.

import { call } from "pi-rs:bridge";
import { createRequire } from "module";

// TypeBox (the largest module behind pi-ai) loads on first use: `StringEnum` is its only user here.
// `require` returns the same module instance an `import` of "typebox" gets.
const requireModule = createRequire(import.meta.url);
let typeboxType;
const lazyType = () => (typeboxType ??= requireModule("typebox").Type);

const ai = (fn, ...args) => {
	const value = call("ai", { fn, args });
	return value === null ? undefined : value;
};

// ---------------------------------------------------------------------------
// utils/version.ts
// ---------------------------------------------------------------------------

export const PI_AI_VERSION = call("ai", { fn: "version", args: [] });

// ---------------------------------------------------------------------------
// utils/event-stream.ts
// ---------------------------------------------------------------------------

class FifoQueue {
	incoming = [];
	outgoing = [];
	get length() {
		return this.incoming.length + this.outgoing.length;
	}
	enqueue(value) {
		this.incoming.push(value);
	}
	dequeue() {
		if (this.outgoing.length === 0) {
			while (this.incoming.length > 0) this.outgoing.push(this.incoming.pop());
		}
		return this.outgoing.pop();
	}
}

/** Generic event stream class for async iteration. */
export class EventStream {
	#queue = new FifoQueue();
	#waiting = new FifoQueue();
	done = false;
	#finalResultPromise;
	#resolveFinalResult;
	#isComplete;
	#extractResult;

	constructor(isComplete, extractResult) {
		this.#isComplete = isComplete;
		this.#extractResult = extractResult;
		this.#finalResultPromise = new Promise((resolve) => {
			this.#resolveFinalResult = resolve;
		});
	}

	push(event) {
		if (this.done) return;
		if (this.#isComplete(event)) {
			this.done = true;
			this.#resolveFinalResult(this.#extractResult(event));
		}
		const waiter = this.#waiting.dequeue();
		if (waiter) waiter({ value: event, done: false });
		else this.#queue.enqueue(event);
	}

	end(result) {
		this.done = true;
		if (result !== undefined) this.#resolveFinalResult(result);
		while (this.#waiting.length > 0) this.#waiting.dequeue()({ value: undefined, done: true });
	}

	async *[Symbol.asyncIterator]() {
		while (true) {
			if (this.#queue.length > 0) {
				yield this.#queue.dequeue();
			} else if (this.done) {
				return;
			} else {
				const result = await new Promise((resolve) => this.#waiting.enqueue(resolve));
				if (result.done) return;
				yield result.value;
			}
		}
	}

	result() {
		return this.#finalResultPromise;
	}
}

/**
 * Event stream of one assistant response; the final message gets `durationMs` measured from the
 * stream's creation unless it already has one or predates the stream.
 */
export class AssistantMessageEventStream extends EventStream {
	#startedAt = Date.now();
	#startedAtMonotonic = performance.now();

	constructor() {
		super(
			(event) => event.type === "done" || event.type === "error",
			(event) => {
				if (event.type === "done") return event.message;
				if (event.type === "error") return event.error;
				throw new Error("Unexpected event type for final result");
			},
		);
	}

	push(event) {
		if (event.type === "done") this.#time(event.message);
		else if (event.type === "error") this.#time(event.error);
		super.push(event);
	}

	end(result) {
		if (result !== undefined) this.#time(result);
		super.end(result);
	}

	#time(message) {
		if (this.done || message.durationMs !== undefined || message.timestamp < this.#startedAt) return;
		message.durationMs = Math.max(0, Math.round(performance.now() - this.#startedAtMonotonic));
	}
}

/** Factory function for AssistantMessageEventStream (for use in extensions). */
export function createAssistantMessageEventStream() {
	return new AssistantMessageEventStream();
}

// ---------------------------------------------------------------------------
// utils/uuid.ts
// ---------------------------------------------------------------------------

const MAX_UUID_V7_TIMESTAMP = 0xffffffffffff;
let lastOrdinaryTimestamp = -1;
let sequence;

export function uuidv7(timestampMs) {
	const requestedTimestamp = timestampMs ?? Date.now();
	if (!Number.isInteger(requestedTimestamp) || requestedTimestamp < 0 || requestedTimestamp > MAX_UUID_V7_TIMESTAMP) {
		throw new RangeError(`UUIDv7 timestamp must be an integer between 0 and ${MAX_UUID_V7_TIMESTAMP}`);
	}
	const effectiveTimestamp =
		timestampMs === undefined ? Math.max(requestedTimestamp, lastOrdinaryTimestamp) : timestampMs;
	if (timestampMs === undefined) lastOrdinaryTimestamp = effectiveTimestamp;
	const bytes = new Uint8Array(16);
	globalThis.crypto.getRandomValues(bytes);
	if (sequence === undefined) {
		sequence = bytes[1] * 2 ** 32 + bytes[2] * 2 ** 24 + bytes[3] * 2 ** 16 + bytes[4] * 2 ** 8 + bytes[5];
	} else {
		sequence++;
	}
	let ts = effectiveTimestamp;
	for (let index = 5; index >= 0; index--) {
		bytes[index] = ts % 256;
		ts = Math.floor(ts / 256);
	}
	const seq = sequence;
	bytes[6] = 0x70 | (Math.floor(seq / 2 ** 37) & 0x0f);
	bytes[7] = Math.floor(seq / 2 ** 29) & 0xff;
	bytes[8] = 0x80 | (Math.floor(seq / 2 ** 23) & 0x3f);
	bytes[9] = Math.floor(seq / 2 ** 15) & 0xff;
	bytes[10] = Math.floor(seq / 2 ** 7) & 0xff;
	bytes[11] = ((seq % 128) << 1) | (bytes[11] & 0x01);
	const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
	return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
}

// ---------------------------------------------------------------------------
// utils/abort.ts
// ---------------------------------------------------------------------------

function abortReason(signal) {
	if (signal.reason !== undefined) return signal.reason;
	const error = new Error("The operation was aborted");
	error.name = "AbortError";
	return error;
}

/** Create an operation-local signal for public APIs whose signal is optional. */
export function operationSignal(signal) {
	return signal ?? new AbortController().signal;
}

/** Stop waiting for an operation when its signal aborts, still observing the abandoned promise. */
export function raceWithAbortSignal(operation, signal) {
	if (signal.aborted) {
		void operation.catch(() => {});
		return Promise.reject(abortReason(signal));
	}
	return new Promise((resolve, reject) => {
		let settled = false;
		const cleanup = () => signal.removeEventListener("abort", onAbort);
		const onAbort = () => {
			if (settled) return;
			settled = true;
			cleanup();
			reject(abortReason(signal));
		};
		signal.addEventListener("abort", onAbort, { once: true });
		void operation.then(
			(value) => {
				if (settled) return;
				settled = true;
				cleanup();
				resolve(value);
			},
			(error) => {
				if (settled) return;
				settled = true;
				cleanup();
				reject(error);
			},
		);
		if (signal.aborted) onAbort();
	});
}

// ---------------------------------------------------------------------------
// utils/diagnostics.ts, utils/models-error.ts
// ---------------------------------------------------------------------------

export function formatThrownValue(value) {
	if (value instanceof Error) return value.message || value.name;
	if (typeof value === "string") return value;
	return String(value);
}

export function extractDiagnosticError(error) {
	if (!(error instanceof Error)) return { name: "ThrownValue", message: formatThrownValue(error) };
	const code = error.code;
	return {
		name: error.name || undefined,
		message: error.message || error.name,
		stack: error.stack,
		code: typeof code === "string" || typeof code === "number" ? code : undefined,
	};
}

export function createAssistantMessageDiagnostic(type, error, details) {
	return { type, timestamp: Date.now(), error: extractDiagnosticError(error), details };
}

export function appendAssistantMessageDiagnostic(message, diagnostic) {
	message.diagnostics = [...(message.diagnostics ?? []), diagnostic];
}

function withCauseDetail(message, cause) {
	if (cause === undefined || cause === null) return message;
	const detail = formatThrownValue(cause).trim();
	if (!detail || message.includes(detail)) return message;
	return `${message}: ${detail}`;
}

export class ModelsError extends Error {
	constructor(code, message, options) {
		super(withCauseDetail(message, options?.cause), options);
		this.name = "ModelsError";
		this.code = code;
	}
}

// ---------------------------------------------------------------------------
// utils/model-operations.ts
// ---------------------------------------------------------------------------

/** The type of a model. Models without `type` are chat models. */
export function getModelType(model) {
	return model.type ?? "chat";
}

/** Runtime-checked model type narrowing, including legacy chat models without `type`. */
export function isModelType(model, type) {
	return getModelType(model) === type;
}

function assertChatModel(model) {
	if (!isModelType(model, "chat")) {
		throw new ModelsError("provider", `Model ${model.provider}/${model.id} is not a chat model`);
	}
}

function assertImageModel(model) {
	if (!isModelType(model, "image")) {
		throw new ModelsError("provider", `Model ${model.provider}/${model.id} is not an image model`);
	}
}

function assertClassifierModel(model) {
	if (!isModelType(model, "classifier")) {
		throw new ModelsError("provider", `Model ${model.provider}/${model.id} is not a classifier model`);
	}
}

function assertClassifierInputSupported(model, context) {
	if (context.images?.length && !model.input.includes("image")) {
		throw new ModelsError("provider", `Model ${model.provider}/${model.id} does not accept image input`);
	}
}

export function imageErrorResult(model, error, aborted = false) {
	return {
		api: model.api,
		provider: model.provider,
		model: model.id,
		output: [],
		stopReason: aborted ? "aborted" : "error",
		errorMessage: error instanceof Error ? error.message : String(error),
		timestamp: Date.now(),
	};
}

export function classifierErrorResult(model, error, aborted = false) {
	return {
		api: model.api,
		provider: model.provider,
		model: model.id,
		answers: {},
		stopReason: aborted ? "aborted" : "error",
		errorMessage: error instanceof Error ? error.message : String(error),
		timestamp: Date.now(),
	};
}

// ---------------------------------------------------------------------------
// utils/text.ts
// ---------------------------------------------------------------------------

/** Extract and join text from message content. */
export function contentText(content, separator = "\n") {
	if (typeof content === "string") return content;
	return content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join(separator);
}

/** Render a system message as a complete prompt: its content followed by its sections. */
export function getSystemMessageText(message) {
	const parts = [contentText(message.content)];
	for (const text of Object.values(message.sections ?? {})) {
		if (text !== null) parts.push(text);
	}
	return parts.filter((part) => part.length > 0).join("\n\n");
}

/** Render a later system message for APIs that accept system messages mid-conversation. */
export function renderSystemMessageUpdate(message) {
	const parts = [];
	const text = contentText(message.content);
	if (text.length > 0) parts.push(text);
	for (const [name, value] of Object.entries(message.sections ?? {})) {
		parts.push(
			value === null
				? `Removed system prompt section "${name}".`
				: `Updated system prompt section "${name}":\n\n${value}`,
		);
	}
	return parts.join("\n\n");
}

// ---------------------------------------------------------------------------
// utils/transcript.ts
// ---------------------------------------------------------------------------

/** Build the leading system message for a prompt and tool set. */
export function createInitialSystemMessage(systemPrompt, tools) {
	const hasSystemPrompt = systemPrompt !== undefined && systemPrompt.length > 0;
	const hasTools = tools !== undefined && tools.length > 0;
	if (!hasSystemPrompt && !hasTools) return undefined;
	return {
		role: "system",
		content: systemPrompt ?? "",
		...(hasTools ? { toolsAdded: tools } : {}),
		timestamp: 0,
	};
}

/** Fold `Context.systemPrompt` and `Context.tools` into a leading system message. */
export function normalizeContext(context) {
	const initialMessage = createInitialSystemMessage(context.systemPrompt, context.tools);
	const messages = initialMessage ? [initialMessage, ...context.messages] : context.messages;
	return { messages };
}

function isSystemMessage(message) {
	return message.role === "system";
}

/** Return the leading system message, if the transcript starts with one. */
export function getInitialSystemMessage(messages) {
	const first = messages[0];
	return first && isSystemMessage(first) ? first : undefined;
}

/** Drop the leading system message for APIs that carry the prompt outside the message list. */
export function withoutInitialSystemMessage(messages) {
	return getInitialSystemMessage(messages) ? messages.slice(1) : messages;
}

/** Resolve the tools available after applying every transcript delta in order. */
export function getCurrentTools(messages) {
	const tools = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsRemoved ?? []) tools.delete(tool.name);
		for (const tool of message.toolsAdded ?? []) tools.set(tool.name, tool);
	}
	return [...tools.values()];
}

/** Replay every system message into one leading system message holding the current prompt and tools. */
export function getCurrentSystemMessage(messages) {
	const content = [];
	const sections = new Map();
	let timestamp;
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		timestamp ??= message.timestamp;
		const text = contentText(message.content);
		if (text.length > 0) content.push(text);
		for (const [name, value] of Object.entries(message.sections ?? {})) {
			if (value === null) sections.delete(name);
			else sections.set(name, value);
		}
	}
	const tools = getCurrentTools(messages);
	if (timestamp === undefined && tools.length === 0) return undefined;
	return {
		role: "system",
		content: content.join("\n\n"),
		...(sections.size > 0 ? { sections: Object.fromEntries(sections) } : {}),
		...(tools.length > 0 ? { toolsAdded: tools } : {}),
		timestamp: timestamp ?? 0,
	};
}

/** Render the current system prompt text after replaying every system message. */
export function getCurrentSystemPrompt(messages) {
	const message = getCurrentSystemMessage(messages);
	return message ? getSystemMessageText(message) : "";
}

/** Rebuild the transcript for APIs without mid-conversation system messages. */
export function collapseSystemMessages(context) {
	const head = getCurrentSystemMessage(context.messages);
	const messages = context.messages.filter((message) => message.role !== "system");
	return { messages: head ? [head, ...messages] : messages };
}

/** Keep later system messages in place when the model accepts them; otherwise collapse them. */
export function resolveTranscript(context, supportsMidConvoSystemMessages) {
	return supportsMidConvoSystemMessages ? context : collapseSystemMessages(context);
}

/** Strip executable and display-only fields from a tool before comparison or persistence. */
export function toToolDeclaration(tool) {
	return {
		name: tool.name,
		description: tool.description,
		parameters: JSON.parse(JSON.stringify(tool.parameters)),
		...(tool.constrainedSampling === undefined ? {} : { constrainedSampling: tool.constrainedSampling }),
	};
}

/** Whether two tools declare the same interface to the model. */
export function declarationsEqual(left, right) {
	return JSON.stringify(toToolDeclaration(left)) === JSON.stringify(toToolDeclaration(right));
}

/** Compare two complete tool states. A changed definition is a removal followed by an addition. */
export function getToolStateChanges(previous, current) {
	const previousTools = new Map(previous.map((tool) => [tool.name, tool]));
	const currentTools = new Map(current.map((tool) => [tool.name, tool]));
	return {
		toolsAdded: current
			.filter((tool) => {
				const previousTool = previousTools.get(tool.name);
				return previousTool === undefined || !declarationsEqual(previousTool, tool);
			})
			.map(toToolDeclaration),
		toolsRemoved: previous
			.filter((tool) => {
				const currentTool = currentTools.get(tool.name);
				return currentTool === undefined || !declarationsEqual(tool, currentTool);
			})
			.map((tool) => ({ name: tool.name })),
	};
}

/** Every definition referenced by transcript tool state, in first-declaration order. */
export function getDeclaredTools(messages) {
	const definitions = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsAdded ?? []) definitions.set(tool.name, tool);
	}
	return [...definitions.values()];
}

/** @deprecated Whether a tool name was declared twice with different definitions. */
export function hasToolRedefinitions(messages) {
	const declared = new Map();
	for (const message of messages) {
		if (!isSystemMessage(message)) continue;
		for (const tool of message.toolsAdded ?? []) {
			const previous = declared.get(tool.name);
			if (previous !== undefined && !declarationsEqual(previous, tool)) return true;
			declared.set(tool.name, tool);
		}
	}
	return false;
}

/** Whether tool history contains a removal or same-name redeclaration. */
export function hasNonAdditiveToolChanges(messages) {
	return findLastNonAdditiveToolChange(messages.filter(isSystemMessage)) >= 0;
}

function findLastNonAdditiveToolChange(systemMessages) {
	const declared = new Set();
	let last = -1;
	for (let index = 0; index < systemMessages.length; index++) {
		const message = systemMessages[index];
		if ((message.toolsRemoved?.length ?? 0) > 0) last = index;
		for (const tool of message.toolsAdded ?? []) {
			if (declared.has(tool.name)) last = index;
			declared.add(tool.name);
		}
	}
	return last;
}

/** Split tool declarations between the top-level request field and in-place changes. */
export function resolveTranscriptTools(messages, support) {
	const systemMessages = messages.filter(isSystemMessage);
	if (support === "none") {
		return { requestTools: getCurrentTools(systemMessages), foldedSystemMessages: systemMessages.length };
	}
	if (support === "changes") return { requestTools: [], foldedSystemMessages: 0 };
	const folded = Math.max(
		getInitialSystemMessage(messages) ? 1 : 0,
		findLastNonAdditiveToolChange(systemMessages) + 1,
	);
	return { requestTools: getCurrentTools(systemMessages.slice(0, folded)), foldedSystemMessages: folded };
}

// ---------------------------------------------------------------------------
// utils/json-parse.ts (`partial-json` parsing is done by the Rust port)
// ---------------------------------------------------------------------------

const VALID_JSON_ESCAPES = new Set(['"', "\\", "/", "b", "f", "n", "r", "t"]);

function isControlCharacter(char) {
	const codePoint = char.codePointAt(0);
	return codePoint !== undefined && codePoint >= 0x00 && codePoint <= 0x1f;
}

function escapeControlCharacter(char) {
	switch (char) {
		case "\b":
			return "\\b";
		case "\f":
			return "\\f";
		case "\n":
			return "\\n";
		case "\r":
			return "\\r";
		case "\t":
			return "\\t";
		default:
			return `\\u${char.codePointAt(0)?.toString(16).padStart(4, "0") ?? "0000"}`;
	}
}

/** Repairs malformed JSON string literals (raw control characters, invalid escapes). */
export function repairJson(json) {
	let repaired = "";
	let inString = false;
	for (let index = 0; index < json.length; index++) {
		const char = json[index];
		if (!inString) {
			repaired += char;
			if (char === '"') inString = true;
			continue;
		}
		if (char === '"') {
			repaired += char;
			inString = false;
			continue;
		}
		if (char === "\\") {
			const nextChar = json[index + 1];
			if (nextChar === undefined) {
				repaired += "\\\\";
				continue;
			}
			if (nextChar === "u") {
				const unicodeDigits = json.slice(index + 2, index + 6);
				if (/^[0-9a-fA-F]{4}$/.test(unicodeDigits)) {
					repaired += `\\u${unicodeDigits}`;
					index += 5;
					continue;
				}
			}
			if (VALID_JSON_ESCAPES.has(nextChar)) {
				repaired += `\\${nextChar}`;
				index += 1;
				continue;
			}
			repaired += "\\\\";
			continue;
		}
		repaired += isControlCharacter(char) ? escapeControlCharacter(char) : char;
	}
	return repaired;
}

export function parseJsonWithRepair(json) {
	try {
		return JSON.parse(json);
	} catch (error) {
		const repairedJson = repairJson(json);
		if (repairedJson !== json) return JSON.parse(repairedJson);
		throw error;
	}
}

/** Parses the complete arguments text of a finished tool call. */
export function parseToolCallArguments(json) {
	if (!json?.trim()) return { arguments: {} };
	try {
		const parsed = parseJsonWithRepair(json);
		if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
			throw new Error("Tool arguments must be a JSON object");
		}
		return { arguments: parsed };
	} catch (error) {
		return {
			arguments: {},
			argumentsError: `${error instanceof Error ? error.message : String(error)}\n\nReceived arguments:\n${json}`,
		};
	}
}

function partialParse(json) {
	const result = call("ai", { fn: "partialParse", args: [json] });
	if (result && typeof result === "object" && "$error" in result) throw new Error(result.$error);
	return result;
}

/** Attempts to parse potentially incomplete JSON during streaming. Always returns an object. */
export function parseStreamingJson(partialJson) {
	if (!partialJson || partialJson.trim() === "") return {};
	try {
		return parseJsonWithRepair(partialJson);
	} catch {
		try {
			return partialParse(partialJson) ?? {};
		} catch {
			try {
				return partialParse(repairJson(partialJson)) ?? {};
			} catch {
				return {};
			}
		}
	}
}

// ---------------------------------------------------------------------------
// utils/overflow.ts
// ---------------------------------------------------------------------------

const OVERFLOW_PATTERNS = [
	/prompt (?:is )?too long/i,
	/prompt exceeds max length/i,
	/request_too_large/i,
	/input is too long for requested model/i,
	/exceeds the context window/i,
	/exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))/i,
	/context window exceeded/i,
	/input token count.*exceeds the maximum/i,
	/maximum prompt length is \d+/i,
	/reduce the length of the messages/i,
	/maximum context length is \d+ tokens/i,
	/exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?/i,
	/input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)/i,
	/exceeds the limit of \d+/i,
	/exceeds the available context size/i,
	/greater than the context length/i,
	/context window exceeds limit/i,
	/exceeded model token limit/i,
	/too large for model with \d+ maximum context length/i,
	/prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?/i,
	/model_context_window_exceeded/i,
	/prompt too long; exceeded (?:max )?context length/i,
	/range of input length should be/i,
	/context[_ ]length[_ ]exceeded/i,
	/too many tokens/i,
	/token limit exceeded/i,
	/^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))/i,
];

const CEREBRAS_BODYLESS_OVERFLOW_PATTERN = /^4(?:00|13)\s*(?:status code)?\s*\(no body\)/i;

const NON_OVERFLOW_PATTERNS = [/^(Throttling error|Service unavailable):/i, /rate limit/i, /too many requests/i];

/** Check if an assistant message represents a context overflow error. */
export function isContextOverflow(message, contextWindow) {
	if (message.stopReason === "error" && message.errorMessage) {
		const isNonOverflow = NON_OVERFLOW_PATTERNS.some((p) => p.test(message.errorMessage));
		if (!isNonOverflow) {
			if (OVERFLOW_PATTERNS.some((p) => p.test(message.errorMessage))) return true;
			if (message.provider === "cerebras" && CEREBRAS_BODYLESS_OVERFLOW_PATTERN.test(message.errorMessage)) {
				return true;
			}
		}
	}
	if (contextWindow && message.stopReason === "stop") {
		const inputTokens = message.usage.input + message.usage.cacheRead;
		if (inputTokens > contextWindow) return true;
	}
	if (contextWindow && message.stopReason === "length" && message.usage.output === 0) {
		const inputTokens = message.usage.input + message.usage.cacheRead;
		if (inputTokens >= contextWindow * 0.99) return true;
	}
	return false;
}

/** Whether a length stop ended below the intended output limit. */
export function isRecoverableLength(message, desiredMaxOutput) {
	return message.stopReason === "length" && desiredMaxOutput > 0 && message.usage.output < desiredMaxOutput;
}

/** Get the overflow patterns for testing purposes. */
export function getOverflowPatterns() {
	return [...OVERFLOW_PATTERNS];
}

// ---------------------------------------------------------------------------
// utils/retry.ts
// ---------------------------------------------------------------------------

function buildProviderErrorPattern(patterns) {
	return new RegExp(patterns.join("|"), "i");
}

const NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN = buildProviderErrorPattern([
	"GoUsageLimitError",
	"FreeUsageLimitError",
	"Monthly usage limit reached",
	"available balance",
	"insufficient_quota",
	"out of budget",
	"quota exceeded",
	"billing",
	"subscription_sharing_usage_limit_exceeded",
	"subscription_sharing_workplace_individual_usage_limit_exceeded",
	'"code"\\s*:\\s*"?1308(?![0-9])',
]);

const RETRYABLE_PROVIDER_ERROR_PATTERN = buildProviderErrorPattern([
	"overloaded",
	"server_busy",
	"servers are currently busy",
	"currently experiencing high demand",
	"model is at capacity",
	"rate.?limit",
	"too many requests",
	"(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])",
	"service.?(?:is.?)?(?:temporarily.?)?unavailable",
	"server.?error",
	"internal.?error",
	"provider.?returned.?(?:an.?)?error",
	"exceeded request buffer limit while retrying upstream",
	"unable to forward upstream",
	"upstream.?request.?failed",
	"upstream_stream_read_error",
	"stream was interrupted",
	"http/?2 stream failed",
	"network.?error",
	"connection.?error",
	"connection.?refused",
	"connection.?lost",
	"other side closed",
	"fetch failed",
	"request to \\S+ failed, reason:",
	"getaddrinfo",
	"ENOTFOUND",
	"EAI_AGAIN",
	"upstream.?connect",
	"reset before headers",
	"socket hang up",
	"socket connection was closed",
	"timed? out",
	"timeout",
	"terminated",
	"websocket.?closed",
	"websocket.?error",
	"ended without",
	"stream ended before message_stop",
	"stream ended before a terminal response event",
	"http2 request did not get a response",
	"stream disconnected before completion",
	"pending stream has been canceled",
	"retry delay",
	"you can retry your request",
	"try your request again",
	"please retry your request",
	"ResourceExhausted",
	"subscription_sharing_usage_unavailable",
	"subscription_sharing_user_unavailable",
]);

export const DEFAULT_MAX_AGENT_RETRY_DELAY_MS = 60_000;

export function retryDelayMs(policy, attempt) {
	const delay = policy.baseDelayMs * 2 ** Math.max(0, attempt - 1);
	const safeDelay = Number.isSafeInteger(delay) ? delay : Number.MAX_SAFE_INTEGER;
	return Math.min(safeDelay, policy.maxAgentDelayMs ?? DEFAULT_MAX_AGENT_RETRY_DELAY_MS);
}

class RetrySleepAbortError extends Error {
	constructor() {
		super("Aborted");
	}
}

function retrySleep(ms, signal) {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(new RetrySleepAbortError());
			return;
		}
		const timeout = setTimeout(resolve, ms);
		signal?.addEventListener(
			"abort",
			() => {
				clearTimeout(timeout);
				reject(new RetrySleepAbortError());
			},
			{ once: true },
		);
	});
}

/** Run a single assistant-producing call with bounded retry on transient errors. */
export async function retryAssistantCall(produce, policy, signal, callbacks) {
	const maxAttempts = policy?.enabled ? policy.maxRetries : 0;
	let attempt = 0;
	let lastRetry;
	for (;;) {
		const response = await produce();
		if (response.stopReason === "aborted") {
			if (lastRetry) await callbacks?.onRetryFinished?.(false, lastRetry.attempt);
			return response;
		}
		if (response.stopReason !== "error") {
			if (lastRetry) await callbacks?.onRetryFinished?.(true, lastRetry.attempt);
			return response;
		}
		if (attempt >= maxAttempts || !isRetryableAssistantError(response)) {
			if (lastRetry) await callbacks?.onRetryFinished?.(false, lastRetry.attempt, response.errorMessage);
			return response;
		}
		attempt++;
		lastRetry = { attempt, errorMessage: response.errorMessage || "Unknown error" };
		const delayMs = retryDelayMs(policy, attempt);
		await callbacks?.onRetryScheduled?.(attempt, maxAttempts, delayMs, lastRetry.errorMessage);
		try {
			await retrySleep(delayMs, signal);
		} catch (error) {
			await callbacks?.onRetryFinished?.(false, attempt, lastRetry.errorMessage);
			if (error instanceof RetrySleepAbortError) {
				const { errorMessage: _errorMessage, ...rest } = response;
				return { ...rest, stopReason: "aborted" };
			}
			throw error;
		}
		await callbacks?.onRetryAttemptStart?.();
	}
}

/** Classifies whether a failed assistant message looks like a transient provider error. */
export function isRetryableAssistantError(message) {
	if (message.stopReason !== "error" || !message.errorMessage) return false;
	const errorMessage = message.errorMessage;
	if (NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN.test(errorMessage)) return false;
	return RETRYABLE_PROVIDER_ERROR_PATTERN.test(errorMessage);
}

// ---------------------------------------------------------------------------
// utils/identity.ts
// ---------------------------------------------------------------------------

/** pi's attribution, used by pi-agent-core, pi-durable, and the coding agent. */
export const PI_ATTRIBUTION = { name: "pi", url: "https://pi.dev", categories: ["cli-agent"] };

const NVIDIA_NIM_HOST = "integrate.api.nvidia.com";
const OPENCODE_HOST = "opencode.ai";

/** Merge identities field by field; later sources win and undefined fields are skipped. */
export function mergeClientIdentity(...sources) {
	let merged;
	for (const source of sources) {
		if (!source) continue;
		merged = { ...merged };
		if (source.userAgent !== undefined) merged.userAgent = source.userAgent;
		if (source.attribution !== undefined) merged.attribution = source.attribution;
		if (source.version !== undefined) merged.version = source.version;
	}
	return merged;
}

/** The version in the detailed `User-Agent`. */
export function getClientVersion(identity) {
	return identity?.version ?? PI_AI_VERSION;
}

/** The `User-Agent` value of an identity (the one pi's Rust providers send). */
export function formatUserAgent(identity) {
	return ai("formatUserAgent", identity ?? null);
}

function matchesHost(baseUrl, expectedHost) {
	try {
		return new URL(baseUrl).hostname === expectedHost;
	} catch {
		return false;
	}
}

function isOpenRouter(target) {
	return target.provider === "openrouter" || target.baseUrl.includes("openrouter.ai");
}

function isNvidiaNim(target) {
	return target.provider === "nvidia" || matchesHost(target.baseUrl, NVIDIA_NIM_HOST);
}

function isOpenCode(target) {
	return (
		target.provider === "opencode" || target.provider === "opencode-go" || matchesHost(target.baseUrl, OPENCODE_HOST)
	);
}

/** Provider attribution headers of an identity for a request target. */
export function getAttributionHeaders(target, identity) {
	const attribution = identity?.attribution;
	if (!attribution) return {};
	const { name, url, categories } = attribution;
	if (isOpenRouter(target)) {
		return {
			"HTTP-Referer": url,
			"X-OpenRouter-Title": name,
			...(categories === undefined || categories.length === 0
				? {}
				: { "X-OpenRouter-Categories": categories.join(",") }),
		};
	}
	if (isNvidiaNim(target)) return { "X-BILLING-INVOKE-ORIGIN": name.charAt(0).toUpperCase() + name.slice(1) };
	if (isOpenCode(target)) return { "x-opencode-client": name };
	return {};
}

/** OpenCode's per-conversation routing header for requests to OpenCode. */
export function getOpenCodeSessionHeaders(target, sessionId) {
	return sessionId && isOpenCode(target) ? { "x-opencode-session": sessionId } : undefined;
}

/** Default headers an identity adds to a request: `User-Agent` and provider attribution. */
export function getIdentityHeaders(target, identity) {
	return { "User-Agent": formatUserAgent(identity), ...getAttributionHeaders(target, identity) };
}

function isMutableModels(models) {
	return "setProvider" in models && "deleteProvider" in models && "clearProviders" in models;
}

/** `models` with `identity` as the default identity of its requests. */
export function withClientIdentity(models, identity) {
	const withIdentity = (options) => ({ ...options, identity: mergeClientIdentity(identity, options?.identity) });
	const wrapped = {
		getProviders: () => models.getProviders(),
		getProvider: (id) => models.getProvider(id),
		getModels: (provider) => models.getModels(provider),
		getModel: (provider, id) => models.getModel(provider, id),
		getModelsOfType: (type, provider) => models.getModelsOfType(type, provider),
		getModelOfType: (type, provider, id) => models.getModelOfType(type, provider, id),
		getAllModels: (provider) => models.getAllModels(provider),
		refresh: (options) => models.refresh(options),
		checkAuth: (providerId, options) => models.checkAuth(providerId, options),
		getAvailable: (providerId, options) => models.getAvailable(providerId, options),
		getAvailableOfType: (type, providerId, options) => models.getAvailableOfType(type, providerId, options),
		getAllAvailable: (providerId, options) => models.getAllAvailable(providerId, options),
		getAuth: models.getAuth.bind(models),
		login: (providerId, type, interaction, options) => models.login(providerId, type, interaction, options),
		logout: (providerId, options) => models.logout(providerId, options),
		stream: (model, context, options) => models.stream(model, context, withIdentity(options)),
		complete: (model, context, options) => models.complete(model, context, withIdentity(options)),
		streamSimple: (model, context, options) => models.streamSimple(model, context, withIdentity(options)),
		completeSimple: (model, context, options) => models.completeSimple(model, context, withIdentity(options)),
		streamDeferred: (model, handle, options) => models.streamDeferred(model, handle, withIdentity(options)),
		fetchDeferred: (model, handle, options) => models.fetchDeferred(model, handle, withIdentity(options)),
		cancelDeferred: (model, handle, options) => models.cancelDeferred(model, handle, withIdentity(options)),
		generateImages: (model, context, options) => models.generateImages(model, context, withIdentity(options)),
		classify: (model, context, options) => models.classify(model, context, withIdentity(options)),
	};
	if (!isMutableModels(models)) return wrapped;
	return {
		...wrapped,
		setProvider: (provider) => models.setProvider(provider),
		deleteProvider: (id) => models.deleteProvider(id),
		clearProviders: () => models.clearProviders(),
	};
}

// ---------------------------------------------------------------------------
// utils/typebox-helpers.ts, utils/validation.ts
// ---------------------------------------------------------------------------

/** String enum schema compatible with providers that don't support anyOf/const patterns. */
export function StringEnum(values, options) {
	return lazyType().Unsafe({
		type: "string",
		enum: values,
		...(options?.description && { description: options.description }),
		...(options?.default && { default: options.default }),
	});
}

/** Validate a tool call against its tool's schema (TypeBox semantics; Rust implementation). */
export function validateToolCall(tools, toolCall) {
	return ai("validateToolCall", tools, toolCall);
}

/** Validate tool call arguments against the tool's schema (Rust implementation). */
export function validateToolArguments(tool, toolCall) {
	return ai("validateToolArguments", tool, toolCall);
}

// ---------------------------------------------------------------------------
// session-resources.ts
// ---------------------------------------------------------------------------

const sessionResourceCleanups = new Set();

export function registerSessionResourceCleanup(cleanup) {
	sessionResourceCleanups.add(cleanup);
	return () => {
		sessionResourceCleanups.delete(cleanup);
	};
}

export function cleanupSessionResources(sessionId) {
	const errors = [];
	for (const cleanup of sessionResourceCleanups) {
		try {
			cleanup(sessionId);
		} catch (error) {
			errors.push(error);
		}
	}
	if (errors.length > 0) throw new AggregateError(errors, "Failed to cleanup session resources");
}

// Rust session teardown runs the JS cleanups too (`cleanupSessionResources()` in pi).
globalThis.__piSessionCleanup = cleanupSessionResources;

// ---------------------------------------------------------------------------
// api/lazy.ts
// ---------------------------------------------------------------------------

function createSetupErrorMessage(model, error, timestamp) {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "error",
		errorMessage: error instanceof Error ? error.message : String(error),
		timestamp,
	};
}

function hasResult(source) {
	return typeof source.result === "function";
}

async function forwardStream(target, source, observer, observeResult) {
	let result;
	for await (const event of source) {
		target.push(event);
		observer?.onEvent?.(event);
		if (event.type === "done") result = event.message;
		else if (event.type === "error") result = event.error;
	}
	if (hasResult(source)) result = await source.result();
	target.end(result);
	observeResult(result);
}

/** Returns a stream synchronously while running async setup behind it. */
export function lazyStream(model, setup, observer) {
	const startedAt = Date.now();
	const outer = new AssistantMessageEventStream();
	let observedResult = false;
	const observeResult = (result) => {
		if (observedResult) return;
		observedResult = true;
		observer?.onResult?.(result);
	};
	setup()
		.then((inner) => forwardStream(outer, inner, observer, observeResult))
		.catch((error) => {
			const message = createSetupErrorMessage(model, error, startedAt);
			const event = { type: "error", reason: "error", error: message };
			outer.push(event);
			observer?.onEvent?.(event);
			outer.end(message);
			observeResult(message);
		});
	return outer;
}

/** Wraps a dynamically loaded API implementation as `ProviderStreams`. */
export function lazyApi(load, capabilities) {
	const api = {
		stream: (model, context, options) => lazyStream(model, async () => (await load()).stream(model, context, options)),
		streamSimple: (model, context, options) =>
			lazyStream(model, async () => (await load()).streamSimple(model, context, options)),
	};
	if (capabilities?.fetchDeferred) {
		api.fetchDeferred = (model, handle, options) =>
			lazyStream(model, async () => {
				const implementation = await load();
				if (!implementation.fetchDeferred) throw new Error("API does not support deferred responses");
				return implementation.fetchDeferred(model, handle, options);
			});
	}
	if (capabilities?.cancelDeferred) {
		api.cancelDeferred = async (model, handle, options) => {
			const implementation = await load();
			if (!implementation.cancelDeferred) throw new Error("API cannot cancel deferred responses");
			await implementation.cancelDeferred(model, handle, options);
		};
	}
	return api;
}

// ---------------------------------------------------------------------------
// telemetry.ts
// ---------------------------------------------------------------------------

const telemetryProvider = { type: "string", description: "Provider identifier.", required: true };
const telemetryModel = { type: "string", description: "Requested model identifier.", required: true };
const telemetryStop = {
	type: "string",
	description: "Normalized stop reason.",
	values: ["pending", "stop", "length", "toolUse", "error", "aborted", "deferred"],
};
const telemetryRequest = {
	description: "One logical model request, including authentication and provider setup.",
	parents: { kind: "any" },
	startAttributes: {
		provider: telemetryProvider,
		api: { type: "string", description: "API identifier.", required: true },
		model: telemetryModel,
		reasoning: { type: "string", description: "Scalar requested reasoning level.", required: false },
	},
	endAttributes: {
		stop: telemetryStop,
		"usage.input": { type: "number", description: "Uncached input tokens." },
		"usage.output": { type: "number", description: "Output tokens." },
		"usage.cache_read": { type: "number", description: "Cache-read tokens." },
		"usage.cache_write": { type: "number", description: "Cache-write tokens." },
		"cost.total": { type: "number", description: "Total request cost in USD." },
		"response.id": { type: "string", description: "Provider response identifier.", cardinality: "high" },
	},
	events: {
		"pi.ai.first_chunk": { description: "First normalized stream event delivered.", attributes: {} },
		"pi.ai.block": {
			description: "A completed content block; content is never recorded.",
			attributes: {
				type: {
					type: "string",
					description: "Block kind.",
					values: ["text", "thinking", "tool_call"],
					required: true,
				},
				index: { type: "number", description: "Content block index.", required: true },
				chars: { type: "number", description: "UTF-16 character count.", required: true },
				tool: { type: "string", description: "Tool name, for tool-call blocks only.", required: false },
			},
		},
	},
	status: {
		default: "ok",
		errorWhen: "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok.",
	},
};

/** Pi-owned request metadata, bounded error status, events, and low-cardinality metrics. */
export const AI_TELEMETRY_SCHEMA = {
	version: 1,
	spans: {
		"pi.ai.request": telemetryRequest,
		"pi.ai.poll": { ...telemetryRequest, description: "One deferred-response poll, not the deferred request's lifetime." },
	},
	metrics: {
		"pi.ai.request.duration": {
			kind: "histogram",
			description: "Logical request or poll duration.",
			unit: "ms",
			attributes: { provider: telemetryProvider, model: telemetryModel, stop: { ...telemetryStop, required: true } },
		},
		"pi.ai.first_chunk.duration": {
			kind: "histogram",
			description: "Time to the first normalized stream event.",
			unit: "ms",
			attributes: { provider: telemetryProvider, model: telemetryModel },
		},
		"pi.ai.tokens": {
			kind: "counter",
			description: "Reported model token usage; pending and deferred polls are excluded.",
			unit: "token",
			attributes: {
				provider: telemetryProvider,
				model: telemetryModel,
				kind: {
					type: "string",
					description: "Token usage category.",
					values: ["input", "output", "cache_read", "cache_write"],
					required: true,
				},
			},
		},
		"pi.ai.cost": {
			kind: "counter",
			description: "Reported model cost; pending and deferred polls are excluded.",
			unit: "USD",
			attributes: { provider: telemetryProvider, model: telemetryModel },
		},
	},
};

// Telemetry parents reach JavaScript as opaque `{ __piTelemetry }` handles in the stream options pi
// passes to extension providers (TS `options.telemetry`). Requests of a JavaScript `Models`
// collection that forward them are observed by pi-ai's Rust telemetry module (`pi.ai.request` /
// `pi.ai.poll` spans, block events and metrics); without one the no-op context observes nothing.
function observeModelRequest(model, options, poll) {
	const telemetry = options?.telemetry;
	if (!telemetry || typeof telemetry !== "object" || telemetry.__piTelemetry === undefined) return undefined;
	const reasoning = options.reasoning;
	const id = ai("telemetry.observe", {
		model,
		poll: !!poll,
		telemetry: { __piTelemetry: telemetry.__piTelemetry },
		reasoning: typeof reasoning === "string" ? reasoning : null,
	});
	if (id === null || id === undefined) return undefined;
	return {
		onEvent(event) {
			ai("telemetry.event", { id, event });
		},
		onResult(message) {
			ai("telemetry.result", { id, message: message ?? null });
		},
	};
}

// ---------------------------------------------------------------------------
// auth/context.ts, auth/credential-store.ts, auth/helpers.ts
// ---------------------------------------------------------------------------

/** Default auth context: env vars from `process.env`, file existence via node:fs. */
export function defaultProviderAuthContext() {
	return {
		async env(name) {
			const value = globalThis.process?.env?.[name];
			return typeof value === "string" && value.trim().length > 0 ? value : undefined;
		},
		async fileExists(path) {
			try {
				const fs = await import("node:fs/promises");
				let resolved = path;
				if (resolved.startsWith("~")) {
					const os = await import("node:os");
					resolved = os.homedir() + resolved.slice(1);
				}
				await fs.access(resolved);
				return true;
			} catch {
				return false;
			}
		},
	};
}

/** Default in-memory credential store; writes are serialized per provider. */
export class InMemoryCredentialStore {
	#credentials = new Map();
	#chains = new Map();

	#enqueue(providerId, task, options) {
		const signal = operationSignal(options?.signal);
		const previous = this.#chains.get(providerId) ?? Promise.resolve();
		const queued = (async () => {
			await previous.catch(() => {});
			signal.throwIfAborted();
			return task();
		})();
		const tail = queued.catch(() => {});
		this.#chains.set(providerId, tail);
		void tail.then(() => {
			if (this.#chains.get(providerId) === tail) this.#chains.delete(providerId);
		});
		return raceWithAbortSignal(queued, signal);
	}

	async read(providerId, options) {
		options?.signal?.throwIfAborted();
		return this.#credentials.get(providerId);
	}

	async list(options) {
		options?.signal?.throwIfAborted();
		return [...this.#credentials].map(([providerId, credential]) => ({ providerId, type: credential.type }));
	}

	modify(providerId, fn, options) {
		return this.#enqueue(
			providerId,
			async () => {
				const current = this.#credentials.get(providerId);
				const next = await fn(current);
				options?.signal?.throwIfAborted();
				if (next !== undefined) this.#credentials.set(providerId, next);
				return next ?? current;
			},
			options,
		);
	}

	delete(providerId, options) {
		return this.#enqueue(
			providerId,
			async () => {
				this.#credentials.delete(providerId);
			},
			options,
		);
	}
}

/** Standard api-key auth: a stored key wins, otherwise the first set env var resolves. */
export function envApiKeyAuth(name, envVars) {
	return {
		name,
		login: async (interaction) => {
			interaction.signal.throwIfAborted();
			const key = await interaction.prompt({ type: "secret", message: `Enter ${name}` });
			interaction.signal.throwIfAborted();
			return { type: "api_key", key };
		},
		resolve: async ({ ctx, credential, signal }) => {
			signal.throwIfAborted();
			if (credential?.key) {
				return { auth: { apiKey: credential.key }, env: credential.env, source: "stored credential" };
			}
			for (const envVar of envVars) {
				const value = await ctx.env(envVar);
				signal.throwIfAborted();
				if (value) return { auth: { apiKey: value }, source: envVar };
			}
			return undefined;
		},
	};
}

/** Wraps a lazily loaded `OAuthAuth`. */
export function lazyOAuth(input) {
	let promise;
	const loaded = () => {
		promise ??= input.load();
		return promise;
	};
	return {
		name: input.name,
		isSubscription: input.isSubscription,
		loginLabel: input.loginLabel,
		login: async (interaction, options) => (await loaded()).login(interaction, options),
		refresh: async (credential, signal) => (await loaded()).refresh(credential, signal),
		toAuth: async (credential) => (await loaded()).toAuth(credential),
	};
}

// ---------------------------------------------------------------------------
// auth/resolve.ts
// ---------------------------------------------------------------------------

/** Auth resolution shared by all operations in a `Models` collection. */
export function resolveProviderAuth(provider, credentials, authContext, overrides) {
	const signal = operationSignal(overrides?.signal);
	return raceWithAbortSignal(
		resolveProviderAuthWithSignal(provider, credentials, authContext, overrides, signal),
		signal,
	);
}

async function resolveProviderAuthWithSignal(provider, credentials, authContext, overrides, signal) {
	signal.throwIfAborted();
	const requestAuthContext = overrides?.env ? overlayEnvAuthContext(authContext, overrides.env) : authContext;
	if (overrides?.apiKey !== undefined && provider.auth.apiKey) {
		return resolveApiKey(
			requestAuthContext,
			provider.auth.apiKey,
			provider.id,
			{ type: "api_key", key: overrides.apiKey, env: overrides.env },
			signal,
		);
	}
	const stored = await readStoredCredential(credentials, provider.id, signal);
	if (stored) {
		if (stored.type === "oauth" && provider.auth.oauth) {
			return resolveStoredOAuth(credentials, provider.id, provider.auth.oauth, stored, signal, overrides?.minOAuthValidityMs);
		}
		if (stored.type === "api_key" && provider.auth.apiKey) {
			const credential = overrides?.env ? { ...stored, env: { ...stored.env, ...overrides.env } } : stored;
			return resolveApiKey(requestAuthContext, provider.auth.apiKey, provider.id, credential, signal);
		}
		return undefined;
	}
	return provider.auth.apiKey
		? resolveApiKey(requestAuthContext, provider.auth.apiKey, provider.id, undefined, signal)
		: undefined;
}

function overlayEnvAuthContext(base, env) {
	return {
		env: async (name) => env[name] || (await base.env(name)),
		fileExists: (path) => base.fileExists(path),
	};
}

const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS = 5 * 60 * 1000;
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS = 15_000;

/** Refresh a stored OAuth credential under the credential-store lock and persist it. */
export async function refreshStoredOAuthCredential(credentials, providerId, oauth, needsRefresh, signal) {
	let post;
	const lockWait = new AbortController();
	const cancelLockWait = () => lockWait.abort(signal.reason);
	signal.addEventListener("abort", cancelLockWait, { once: true });
	if (signal.aborted) cancelLockWait();
	try {
		post = await credentials.modify(
			providerId,
			async (current) => {
				signal.removeEventListener("abort", cancelLockWait);
				signal.throwIfAborted();
				if (current?.type !== "oauth") return undefined;
				if (!needsRefresh(current)) return undefined;
				try {
					return await oauth.refresh(current, AbortSignal.timeout(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS));
				} catch (error) {
					throw new ModelsError("oauth", `OAuth refresh failed for ${providerId}`, { cause: error });
				}
			},
			{ signal: lockWait.signal },
		);
	} catch (error) {
		if (error instanceof ModelsError) throw error;
		signal.throwIfAborted();
		throw new ModelsError("auth", `Credential store modify failed for ${providerId}`, { cause: error });
	} finally {
		signal.removeEventListener("abort", cancelLockWait);
	}
	return post?.type === "oauth" ? post : undefined;
}

async function resolveStoredOAuth(credentials, providerId, oauth, stored, signal, minOAuthValidityMs) {
	const minimumValidityMs = Math.max(DEFAULT_OAUTH_MINIMUM_VALIDITY_MS, minOAuthValidityMs ?? 0);
	const expiresSoon = (credential) => Date.now() + minimumValidityMs >= credential.expires;
	let credential = stored;
	if (expiresSoon(credential)) {
		const post = await refreshStoredOAuthCredential(credentials, providerId, oauth, expiresSoon, signal);
		if (!post) return undefined;
		credential = post;
		if (minOAuthValidityMs !== undefined && expiresSoon(credential)) {
			throw new ModelsError("oauth", `OAuth refresh returned a token that expires too soon for ${providerId}`);
		}
	}
	try {
		return { auth: await oauth.toAuth(credential), source: "OAuth" };
	} catch (error) {
		throw new ModelsError("oauth", `OAuth auth derivation failed for ${providerId}`, { cause: error });
	}
}

async function resolveApiKey(authContext, apiKey, providerId, credential, signal) {
	try {
		return await apiKey.resolve({ ctx: authContext, credential, signal });
	} catch (error) {
		throw new ModelsError("auth", `API key auth failed for provider ${providerId}`, { cause: error });
	}
}

async function readStoredCredential(credentials, providerId, signal) {
	try {
		return await credentials.read(providerId, { signal });
	} catch (error) {
		throw new ModelsError("auth", `Credential store read failed for ${providerId}`, { cause: error });
	}
}

// ---------------------------------------------------------------------------
// models-store.ts
// ---------------------------------------------------------------------------

export class InMemoryModelsStore {
	#entries = new Map();

	async read(providerId, options) {
		options?.signal?.throwIfAborted();
		const entry = this.#entries.get(providerId);
		return entry ? structuredClone(entry) : undefined;
	}

	async write(providerId, entry, options) {
		options?.signal?.throwIfAborted();
		this.#entries.set(providerId, structuredClone(entry));
	}

	async delete(providerId, options) {
		options?.signal?.throwIfAborted();
		this.#entries.delete(providerId);
	}
}

// ---------------------------------------------------------------------------
// models.ts
// ---------------------------------------------------------------------------

const KNOWN_MODEL_TYPES = { chat: true, image: true, classifier: true };

function hasKnownModelType(model) {
	return Object.hasOwn(KNOWN_MODEL_TYPES, getModelType(model));
}

function withKnownModelTypes(entry) {
	return { ...entry, models: entry.models.filter(hasKnownModelType) };
}

function mergeHeaders(base, override) {
	if (!base && !override) return undefined;
	const merged = { ...base };
	for (const [name, value] of Object.entries(override ?? {})) {
		const lowerName = name.toLowerCase();
		for (const existingName of Object.keys(merged)) {
			if (existingName.toLowerCase() === lowerName) delete merged[existingName];
		}
		merged[name] = value;
	}
	return merged;
}

class ModelsImpl {
	#providers = new Map();
	#credentials;
	#modelsStore;
	#authContext;
	#identity;
	#refreshGenerations = new Map();
	#refreshControllers = new Map();
	#publicationChains = new Map();

	constructor(options) {
		this.#credentials = options?.credentials ?? new InMemoryCredentialStore();
		this.#modelsStore = options?.modelsStore ?? new InMemoryModelsStore();
		this.#authContext = options?.authContext ?? defaultProviderAuthContext();
		this.#identity = options?.identity;
	}

	setProvider(provider) {
		this.#supersedeProviderRefresh(provider.id);
		this.#providers.set(provider.id, provider);
	}

	deleteProvider(id) {
		this.#supersedeProviderRefresh(id);
		this.#providers.delete(id);
	}

	clearProviders() {
		for (const id of new Set([...this.#providers.keys(), ...this.#refreshControllers.keys()])) {
			this.#supersedeProviderRefresh(id);
		}
		this.#providers.clear();
	}

	getProviders() {
		return Array.from(this.#providers.values());
	}

	getProvider(id) {
		return this.#providers.get(id);
	}

	getModels(provider) {
		if (provider !== undefined) {
			const entry = this.#providers.get(provider);
			if (!entry) return [];
			try {
				return entry.getModels();
			} catch {
				return [];
			}
		}
		const models = [];
		for (const entry of this.#providers.values()) {
			try {
				models.push(...entry.getModels());
			} catch {}
		}
		return models;
	}

	getAllModels(provider) {
		if (provider !== undefined) {
			const entry = this.#providers.get(provider);
			if (!entry) return [];
			try {
				return entry.getAllModels?.() ?? entry.getModels();
			} catch {
				return [];
			}
		}
		const models = [];
		for (const entry of this.#providers.values()) {
			try {
				models.push(...(entry.getAllModels?.() ?? entry.getModels()));
			} catch {}
		}
		return models;
	}

	getModelsOfType(type, provider) {
		return this.getAllModels(provider).filter((model) => isModelType(model, type));
	}

	getModel(provider, id) {
		return this.getModels(provider).find((model) => model.id === id);
	}

	getModelOfType(type, provider, id) {
		return this.getModelsOfType(type, provider).find((model) => model.id === id);
	}

	#supersedeProviderRefresh(providerId) {
		const generation = (this.#refreshGenerations.get(providerId) ?? 0) + 1;
		this.#refreshGenerations.set(providerId, generation);
		const previous = this.#refreshControllers.get(providerId);
		if (previous) {
			this.#refreshControllers.delete(providerId);
			previous.abort();
		}
		return generation;
	}

	#beginProviderRefresh(providerId) {
		const generation = this.#supersedeProviderRefresh(providerId);
		const controller = new AbortController();
		this.#refreshControllers.set(providerId, controller);
		return { generation, controller };
	}

	#publishProviderModels(providerId, generation, signal, publication) {
		const previous = this.#publicationChains.get(providerId) ?? Promise.resolve();
		const queued = (async () => {
			await previous.catch(() => {});
			if (signal.aborted || this.#refreshGenerations.get(providerId) !== generation) return false;
			if (publication.persist === null) {
				await this.#modelsStore.delete(providerId, { signal });
			} else if (publication.persist !== undefined) {
				await this.#modelsStore.write(providerId, structuredClone(publication.persist), { signal });
			}
			if (signal.aborted || this.#refreshGenerations.get(providerId) !== generation) return false;
			publication.update?.();
			return true;
		})();
		const tail = queued.catch(() => {});
		this.#publicationChains.set(providerId, tail);
		void tail.then(() => {
			if (this.#publicationChains.get(providerId) === tail) this.#publicationChains.delete(providerId);
		});
		return raceWithAbortSignal(queued, signal);
	}

	async #runProviderRefreshPhase(provider, credential, allowNetwork, force, generation, signal) {
		const stored = await this.#modelsStore.read(provider.id, { signal });
		await provider.refreshModels({
			credential,
			stored: stored ? withKnownModelTypes(structuredClone(stored)) : undefined,
			publish: (publication) => this.#publishProviderModels(provider.id, generation, signal, publication),
			allowNetwork,
			force: allowNetwork ? force : undefined,
			signal,
			baseUrl: provider.baseUrl,
		});
	}

	async refresh(options = {}) {
		const allowNetwork = options.allowNetwork ?? true;
		const callerSignal = operationSignal(options.signal);
		const errors = new Map();
		if (callerSignal.aborted) return { aborted: true, errors };
		const selected = options.providers ? new Set(options.providers) : undefined;
		const refreshable = Array.from(this.#providers.values()).filter(
			(provider) => provider.refreshModels !== undefined && (!selected || selected.has(provider.id)),
		);
		const refresh = Promise.all(
			refreshable.map(async (provider) => {
				const { generation, controller } = this.#beginProviderRefresh(provider.id);
				const signal = AbortSignal.any([callerSignal, controller.signal]);
				const operation = (async () => {
					let storedCredential;
					let credentialError;
					try {
						storedCredential = await this.#readCredential(provider.id, signal);
					} catch (error) {
						credentialError = error;
					}
					await this.#runProviderRefreshPhase(provider, storedCredential, false, undefined, generation, signal);
					if (credentialError !== undefined) throw credentialError;
					if (!allowNetwork || signal.aborted) return;
					const credential = await this.#resolveRefreshCredential(provider, storedCredential, signal);
					if (!credential) return;
					await this.#runProviderRefreshPhase(provider, credential, true, options.force, generation, signal);
				})();
				try {
					await raceWithAbortSignal(operation, signal);
				} catch (error) {
					if (!signal.aborted) {
						errors.set(
							provider.id,
							error instanceof Error
								? error
								: new ModelsError("model_source", `Model refresh failed for ${provider.id}`, { cause: error }),
						);
					}
				} finally {
					if (this.#refreshControllers.get(provider.id) === controller) {
						this.#refreshControllers.delete(provider.id);
					}
				}
			}),
		);
		try {
			await raceWithAbortSignal(refresh, callerSignal);
		} catch (error) {
			if (!callerSignal.aborted) throw error;
		}
		return { aborted: callerSignal.aborted, errors: new Map(errors) };
	}

	async #resolveRefreshCredential(provider, stored, signal) {
		if (stored?.type === "oauth") {
			const oauth = provider.auth.oauth;
			if (!oauth) return undefined;
			if (Date.now() < stored.expires) return stored;
			if (signal.aborted) return undefined;
			return refreshStoredOAuthCredential(
				this.#credentials,
				provider.id,
				oauth,
				(current) => Date.now() >= current.expires,
				signal,
			);
		}
		const apiKey = provider.auth.apiKey;
		if (!apiKey) return undefined;
		const credential = stored?.type === "api_key" ? stored : undefined;
		const result = await apiKey.resolve({ ctx: this.#authContext, credential, signal });
		if (!result) return undefined;
		return { type: "api_key", key: result.auth.apiKey, env: result.env };
	}

	async #readCredential(providerId, signal) {
		try {
			return await this.#credentials.read(providerId, { signal });
		} catch (error) {
			throw new ModelsError("auth", `Credential store read failed for ${providerId}`, { cause: error });
		}
	}

	async #checkProviderAuth(provider, credential, signal) {
		if (credential?.type === "oauth") {
			return provider.auth.oauth ? { source: "OAuth", type: "oauth" } : undefined;
		}
		const apiKey = provider.auth.apiKey;
		if (!apiKey) return undefined;
		if (apiKey.check) {
			try {
				return await apiKey.check({
					ctx: this.#authContext,
					credential: credential?.type === "api_key" ? credential : undefined,
					signal,
				});
			} catch (error) {
				throw new ModelsError("auth", `API key auth check failed for provider ${provider.id}`, { cause: error });
			}
		}
		const resolution = await resolveProviderAuth(provider, this.#credentials, this.#authContext, { signal });
		return resolution ? { source: resolution.source, type: "api_key" } : undefined;
	}

	checkAuth(providerId, options) {
		const signal = operationSignal(options?.signal);
		const check = (async () => {
			signal.throwIfAborted();
			const provider = this.#providers.get(providerId);
			if (!provider) return undefined;
			return this.#checkProviderAuth(provider, await this.#readCredential(providerId, signal), signal);
		})();
		return raceWithAbortSignal(check, signal);
	}

	async #getAuthenticatedProviders(providerId, signal) {
		signal.throwIfAborted();
		const providers = providerId
			? [this.#providers.get(providerId)].filter((entry) => entry !== undefined)
			: this.getProviders();
		const checks = await Promise.all(
			providers.map(async (provider) => {
				const credential = await this.#readCredential(provider.id, signal);
				return { provider, credential, auth: await this.#checkProviderAuth(provider, credential, signal) };
			}),
		);
		return checks.filter((entry) => entry.auth !== undefined);
	}

	getAvailable(providerId, options) {
		const signal = operationSignal(options?.signal);
		const available = (async () => {
			const providers = await this.#getAuthenticatedProviders(providerId, signal);
			return providers.flatMap(({ provider, credential }) => {
				const models = provider.getModels();
				return provider.filterModels?.(models, credential) ?? models;
			});
		})();
		return raceWithAbortSignal(available, signal);
	}

	async getAvailableOfType(type, providerId, options) {
		return (await this.getAllAvailable(providerId, options)).filter((model) => isModelType(model, type));
	}

	getAllAvailable(providerId, options) {
		const signal = operationSignal(options?.signal);
		const available = (async () => {
			const providers = await this.#getAuthenticatedProviders(providerId, signal);
			return providers.flatMap(({ provider, credential }) => {
				const models = provider.getAllModels?.() ?? provider.getModels();
				if (provider.filterAllModels) return provider.filterAllModels(models, credential);
				if (!provider.filterModels) return models;
				const availableChatIds = new Set(
					provider.filterModels(provider.getModels(), credential).map((model) => model.id),
				);
				return models.filter((model) => !isModelType(model, "chat") || availableChatIds.has(model.id));
			});
		})();
		return raceWithAbortSignal(available, signal);
	}

	async getAuth(providerOrModel, overrides) {
		const signal = operationSignal(overrides?.signal);
		const providerId = typeof providerOrModel === "string" ? providerOrModel : providerOrModel.provider;
		const provider = this.#providers.get(providerId);
		if (!provider) return undefined;
		const result = await resolveProviderAuth(provider, this.#credentials, this.#authContext, { ...overrides, signal });
		if (!result || typeof providerOrModel === "string" || !providerOrModel.headers) return result;
		return { ...result, auth: { ...result.auth, headers: mergeHeaders(result.auth.headers, providerOrModel.headers) } };
	}

	async login(providerId, type, interaction, options) {
		const signal = operationSignal(interaction.signal);
		signal.throwIfAborted();
		const provider = this.#providers.get(providerId);
		if (!provider) throw new ModelsError("provider", `Unknown provider: ${providerId}`);
		const method = type === "oauth" ? provider.auth.oauth : provider.auth.apiKey;
		if (!method?.login) {
			throw new ModelsError("auth", `${provider.name} does not support ${type} login`);
		}
		const loginOperation = method.login({ ...interaction, signal }, options);
		const credential = await raceWithAbortSignal(loginOperation, signal);
		let mutationStarted = false;
		let markMutationStarted;
		const started = new Promise((resolve) => {
			markMutationStarted = resolve;
		});
		const mutation = this.#credentials.modify(
			providerId,
			async () => {
				mutationStarted = true;
				markMutationStarted?.();
				return credential;
			},
			{ signal },
		);
		void mutation.catch(() => {});
		try {
			await new Promise((resolve, reject) => {
				const onAbort = () => {
					if (!mutationStarted) reject(signal.reason);
				};
				signal.addEventListener("abort", onAbort, { once: true });
				void Promise.race([started, mutation]).then(
					() => {
						signal.removeEventListener("abort", onAbort);
						resolve();
					},
					(error) => {
						signal.removeEventListener("abort", onAbort);
						reject(error);
					},
				);
				if (signal.aborted) onAbort();
			});
			await mutation;
		} catch (error) {
			signal.throwIfAborted();
			throw new ModelsError("auth", `Credential store modify failed for ${providerId}`, { cause: error });
		}
		return credential;
	}

	async logout(providerId, options) {
		const signal = operationSignal(options?.signal);
		signal.throwIfAborted();
		try {
			await this.#credentials.delete(providerId, { signal });
		} catch (error) {
			signal.throwIfAborted();
			throw new ModelsError("auth", `Credential store delete failed for ${providerId}`, { cause: error });
		}
	}

	#requireProvider(model) {
		const provider = this.#providers.get(model.provider);
		if (!provider) throw new ModelsError("provider", `Unknown provider: ${model.provider}`);
		return provider;
	}

	#requireChatProvider(model) {
		assertChatModel(model);
		return this.#requireProvider(model);
	}

	async #applyAuth(model, options) {
		this.#requireProvider(model);
		const resolution = await this.getAuth(model, {
			apiKey: options?.apiKey,
			env: options?.env,
			signal: options?.signal,
		});
		if (!resolution) throw new ModelsError("auth", `Provider is not configured: ${model.provider}`);
		const auth = resolution.auth;
		const apiKey = options?.apiKey ?? auth.apiKey;
		const requestModel = auth.baseUrl ? { ...model, baseUrl: auth.baseUrl } : model;
		const sessionId =
			options && "sessionId" in options && typeof options.sessionId === "string" ? options.sessionId : undefined;
		let headers = mergeHeaders(
			mergeHeaders(getOpenCodeSessionHeaders(requestModel, sessionId), auth.headers),
			options?.headers,
		);
		if (options?.transformHeaders) headers = await options.transformHeaders(headers ?? {});
		const env = resolution.env || options?.env ? { ...(resolution.env ?? {}), ...(options?.env ?? {}) } : undefined;
		const identity = mergeClientIdentity(this.#identity, options?.identity);
		const { transformHeaders: _transformHeaders, ...providerOptions } = options ?? {};
		const requestOptions = {
			...providerOptions,
			apiKey,
			headers,
			env,
			...(identity === undefined ? {} : { identity }),
		};
		return { requestModel, requestOptions };
	}

	stream(model, context, options) {
		const transcript = normalizeContext(context);
		return lazyStream(
			model,
			async () => {
				const provider = this.#requireChatProvider(model);
				const { requestModel, requestOptions } = await this.#applyAuth(model, options);
				return provider.stream(requestModel, transcript, requestOptions);
			},
			observeModelRequest(model, options, false),
		);
	}

	async complete(model, context, options) {
		return this.stream(model, context, options).result();
	}

	streamSimple(model, context, options) {
		const transcript = normalizeContext(context);
		return lazyStream(
			model,
			async () => {
				const provider = this.#requireChatProvider(model);
				const { requestModel, requestOptions } = await this.#applyAuth(model, options);
				return provider.streamSimple(requestModel, transcript, requestOptions);
			},
			observeModelRequest(model, options, false),
		);
	}

	async completeSimple(model, context, options) {
		return this.streamSimple(model, context, options).result();
	}

	streamDeferred(model, handle, options) {
		return lazyStream(
			model,
			async () => {
				const provider = this.#requireChatProvider(model);
				if (!provider.fetchDeferred) {
					throw new ModelsError("provider", `Provider ${model.provider} does not support deferred responses`);
				}
				const { requestModel, requestOptions } = await this.#applyAuth(model, options);
				return provider.fetchDeferred(requestModel, handle, requestOptions);
			},
			observeModelRequest(model, options, true),
		);
	}

	async fetchDeferred(model, handle, options) {
		return this.streamDeferred(model, handle, options).result();
	}

	async cancelDeferred(model, handle, options) {
		const provider = this.#requireChatProvider(model);
		if (!provider.cancelDeferred) {
			throw new ModelsError("provider", `Provider ${model.provider} does not support deferred responses`);
		}
		const { requestModel, requestOptions } = await this.#applyAuth(model, options);
		await provider.cancelDeferred(requestModel, handle, requestOptions);
	}

	async generateImages(model, context, options) {
		try {
			assertImageModel(model);
			const provider = this.#requireProvider(model);
			if (!provider.generateImages) {
				throw new ModelsError("provider", `Provider ${model.provider} does not support image generation`);
			}
			const { requestModel, requestOptions } = await this.#applyAuth(model, options);
			return await provider.generateImages(requestModel, context, requestOptions);
		} catch (error) {
			return imageErrorResult(model, error, options?.signal?.aborted);
		}
	}

	async classify(model, context, options) {
		try {
			assertClassifierModel(model);
			assertClassifierInputSupported(model, context);
			const provider = this.#requireProvider(model);
			if (!provider.classify) {
				throw new ModelsError("provider", `Provider ${model.provider} does not support classification`);
			}
			const { requestModel, requestOptions } = await this.#applyAuth(model, options);
			return await provider.classify(requestModel, context, requestOptions);
		} catch (error) {
			return classifierErrorResult(model, error, options?.signal?.aborted);
		}
	}
}

export function createModels(options) {
	return new ModelsImpl(options);
}

/** Builds a provider from parts. */
export function createProvider(input) {
	const single = input.api && typeof input.api.stream === "function" ? input.api : undefined;
	const byApi = single || !input.api ? undefined : input.api;
	const images = input.images;
	const classifiers = input.classifiers;
	const streams = single ? [single] : Object.values(byApi ?? {}).filter((entry) => entry !== undefined);
	const imageImplementations = Object.values(images ?? {}).filter((entry) => entry !== undefined);
	const classifierImplementations = Object.values(classifiers ?? {}).filter((entry) => entry !== undefined);
	if (streams.length === 0 && imageImplementations.length === 0 && classifierImplementations.length === 0) {
		throw new Error(`Provider ${input.id}: at least one of "api", "images", or "classifiers" is required.`);
	}
	const baselineModels = input.models;
	let dynamicModels = [];
	const fetchModels = input.fetchModels;
	const currentModels = () => {
		const merged = [...baselineModels];
		for (const model of dynamicModels) {
			const index = merged.findIndex(
				(entry) => getModelType(entry) === getModelType(model) && entry.id === model.id,
			);
			if (index >= 0) merged[index] = model;
			else merged.push(model);
		}
		return merged;
	};
	const apiFor = (model) => single ?? byApi?.[model.api];
	const dispatch = (model, run) => {
		const implementation = apiFor(model);
		if (!implementation) {
			return lazyStream(model, async () => {
				throw new ModelsError("stream", `Provider ${input.id} has no API implementation for "${model.api}"`);
			});
		}
		return run(implementation);
	};
	const provider = {
		id: input.id,
		name: input.name ?? input.id,
		baseUrl: input.baseUrl,
		headers: input.headers,
		auth: input.auth,
		getModels: () => currentModels().filter((model) => isModelType(model, "chat")),
		getAllModels: currentModels,
		refreshModels: fetchModels
			? async (context) => {
					if (context.stored) {
						const restored = context.stored.models.filter((model) => model.provider === input.id);
						if (
							!(await context.publish({
								update: () => {
									dynamicModels = restored;
								},
							}))
						) {
							return;
						}
					}
					if (!context.allowNetwork || context.signal.aborted) return;
					const fetched = await fetchModels(context);
					if (context.signal.aborted) return;
					const refreshed = fetched.filter(hasKnownModelType);
					await context.publish({
						persist: { models: refreshed, checkedAt: Date.now() },
						update: () => {
							dynamicModels = refreshed;
						},
					});
				}
			: undefined,
		filterModels: input.filterModels,
		filterAllModels: input.filterAllModels,
		stream: (model, context, options) => dispatch(model, (impl) => impl.stream(model, context, options)),
		streamSimple: (model, context, options) => dispatch(model, (impl) => impl.streamSimple(model, context, options)),
	};
	if (streams.some((entry) => entry.fetchDeferred !== undefined)) {
		provider.fetchDeferred = (model, handle, options) =>
			lazyStream(model, async () => {
				const implementation = apiFor(model);
				if (!implementation?.fetchDeferred) {
					throw new ModelsError(
						"provider",
						`Provider ${input.id} does not support deferred responses for "${model.api}"`,
					);
				}
				return implementation.fetchDeferred(model, handle, options);
			});
	}
	if (streams.some((entry) => entry.cancelDeferred !== undefined)) {
		provider.cancelDeferred = async (model, handle, options) => {
			const implementation = apiFor(model);
			if (!implementation?.cancelDeferred) {
				throw new ModelsError("provider", `Provider ${input.id} cannot cancel deferred responses for "${model.api}"`);
			}
			await implementation.cancelDeferred(model, handle, options);
		};
	}
	if (images && imageImplementations.length > 0) {
		provider.generateImages = async (model, context, options) => {
			const implementation = images[model.api];
			if (!implementation) {
				return imageErrorResult(
					model,
					new ModelsError("provider", `Provider ${input.id} has no image generation implementation for "${model.api}"`),
				);
			}
			return implementation.generateImages(model, context, options);
		};
	}
	if (classifiers && classifierImplementations.length > 0) {
		provider.classify = async (model, context, options) => {
			const implementation = classifiers[model.api];
			if (!implementation) {
				return classifierErrorResult(
					model,
					new ModelsError("provider", `Provider ${input.id} has no classifier implementation for "${model.api}"`),
				);
			}
			return implementation.classify(model, context, options);
		};
	}
	return provider;
}

/** Runtime-checked narrowing for dynamically looked-up models. */
export function hasApi(model, api) {
	return isModelType(model, "chat") && model.api === api;
}

export function calculateCost(model, usage) {
	const inputTokens = usage.input + usage.cacheRead + usage.cacheWrite;
	let rates = model.cost;
	let matchedThreshold = -1;
	for (const tier of model.cost.tiers ?? []) {
		if (inputTokens > tier.inputTokensAbove && tier.inputTokensAbove > matchedThreshold) {
			rates = tier;
			matchedThreshold = tier.inputTokensAbove;
		}
	}
	const longWrite = usage.cacheWrite1h ?? 0;
	const shortWrite = usage.cacheWrite - longWrite;
	usage.cost.input = (rates.input / 1000000) * usage.input;
	usage.cost.output = (rates.output / 1000000) * usage.output;
	usage.cost.cacheRead = (rates.cacheRead / 1000000) * usage.cacheRead;
	usage.cost.cacheWrite = (rates.cacheWrite * shortWrite + rates.input * 2 * longWrite) / 1000000;
	usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cacheRead + usage.cost.cacheWrite;
	return usage.cost;
}

const EXTENDED_THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

export function getSupportedThinkingLevels(model) {
	if (!model.reasoning) return ["off"];
	return EXTENDED_THINKING_LEVELS.filter((level) => {
		const mapped = model.thinkingLevelMap?.[level];
		if (mapped === null) return false;
		if (level === "xhigh" || level === "max") return mapped !== undefined;
		return true;
	});
}

export function clampThinkingLevel(model, level) {
	const availableLevels = getSupportedThinkingLevels(model);
	if (availableLevels.includes(level)) return level;
	const requestedIndex = EXTENDED_THINKING_LEVELS.indexOf(level);
	if (requestedIndex === -1) return availableLevels[0] ?? "off";
	for (let i = requestedIndex; i < EXTENDED_THINKING_LEVELS.length; i++) {
		const candidate = EXTENDED_THINKING_LEVELS[i];
		if (availableLevels.includes(candidate)) return candidate;
	}
	for (let i = requestedIndex - 1; i >= 0; i--) {
		const candidate = EXTENDED_THINKING_LEVELS[i];
		if (availableLevels.includes(candidate)) return candidate;
	}
	return availableLevels[0] ?? "off";
}

/** Check if two models are equal by comparing their type, id, and provider. */
export function modelsAreEqual(a, b) {
	if (!a || !b) return false;
	return getModelType(a) === getModelType(b) && a.id === b.id && a.provider === b.provider;
}

globalThis.__piAi = { AssistantMessageEventStream };
