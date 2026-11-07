// Count the JavaScript source bytes a Node host hands to its compiler (PKGP-02).
//
// Preloaded with `node --require`: wraps CommonJS `Module._compile`, the ESM `module.registerHooks`
// chain (jiti replaces the chain instead of chaining onto it, so `registerHooks` itself is
// wrapped) and the `vm` compilers jiti/Babel fall back to. One sink file per process AND thread,
// because extensions spawn child node processes that inherit this environment and worker threads
// share it but compile separately; a shared file would lose the main thread's count.
//
//   PI_JS_COMPILE_SINK=/path/to/js node --require js-compile-counter.cjs <cli> ...
//   -> /path/to/js.<pid>.<thread> holds {"total": <bytes>, "modules": <count>}
const Module = require('node:module');
const vm = require('node:vm');
const fs = require('node:fs');
const { threadId } = require('node:worker_threads');

const base = process.env.PI_JS_COMPILE_SINK;
const sink = base ? `${base}.${process.pid}.${threadId}` : undefined;
const bytes = { total: 0, modules: 0 };

// Synchronous: the benchmark stops the host with SIGKILL, which runs no exit handlers.
function flush() {
	if (!sink) return;
	try {
		fs.writeFileSync(sink, JSON.stringify(bytes));
	} catch {}
}

function size(source) {
	if (typeof source === 'string') return Buffer.byteLength(source);
	if (source && typeof source.byteLength === 'number') return source.byteLength;
	return 0;
}

function add(source) {
	bytes.modules++;
	bytes.total += size(source);
	flush();
}

const origCompile = Module.prototype._compile;
Module.prototype._compile = function (content, filename, ...rest) {
	add(content);
	// CJS modules loaded through jiti are compiled from this wrapper too.
	return origCompile.call(this, content, filename, ...rest);
};

const origRegister = Module.registerHooks;
if (origRegister) {
	Module.registerHooks = function (hooks) {
		const load = hooks && hooks.load;
		return origRegister.call(this, {
			...hooks,
			load(url, context, nextLoad) {
				const result = load ? load(url, context, nextLoad) : nextLoad(url, context);
				if (result && result.source != null) add(result.source);
				return result;
			},
		});
	};
	// Our own pass-through hook: builtins report no source and are skipped by `add` callers.
	origRegister.call(Module, {
		load(url, context, nextLoad) {
			const result = nextLoad(url, context);
			if (result && result.source != null) add(result.source);
			return result;
		},
	});
}

const origRunInThisContext = vm.runInThisContext;
vm.runInThisContext = function (code, ...rest) {
	add(code);
	return origRunInThisContext.call(this, code, ...rest);
};

const OrigScript = vm.Script;
class Script extends OrigScript {
	constructor(code, ...rest) {
		add(code);
		super(code, ...rest);
	}
}
vm.Script = Script;

process.on('exit', flush);
