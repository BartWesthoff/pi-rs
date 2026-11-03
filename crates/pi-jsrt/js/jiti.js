// jiti: the embedded `jiti` module (bare specifier `jiti`, which beats `node_modules`), with the
// API and behavior of jiti 2.7.0 (https://github.com/unjs/jiti, MIT) on pi-jsrt's own loader:
// oxc transpiles TypeScript, QuickJS evaluates ES modules, so jiti's Babel pipeline is never
// loaded. Loading happens in jiti generations (src/loader.rs): a module tree imported through an
// instance resolves its imports the way jiti does (builtins, `virtualModules`, `alias`), files
// jiti would transpile load fresh per import with `moduleCache: false`, files jiti loads natively
// stay shared with the rest of the runtime.
//
// Options jiti implements with Babel or get-tsconfig (`transform`, `transformOptions`, `jsx`,
// `tsconfigPaths`, non-default `extensions`) throw a JitiUnsupportedOptionError; cache options
// (`fsCache`, `rebuildFsCache`, `cacheVersion`, ...) have nothing to configure here.

const path = internal.requireBuiltin("path");
const fs = internal.requireBuiltin("fs");
const { fileURLToPath, pathToFileURL } = internal.requireBuiltin("url");

const DEFAULT_EXTENSIONS = [".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".mtsx", ".ctsx"];

class JitiUnsupportedOptionError extends Error {
  constructor(option, reason) {
    super(`jiti option "${option}" is not supported by the embedded jiti: ${reason}`);
    this.name = "JitiUnsupportedOptionError";
    this.code = "ERR_JITI_UNSUPPORTED_OPTION";
    this.option = option;
  }
}

// --- options (jiti src/options.ts) ----------------------------------------------------------

function jsonEnv(name, defaultValue, rawFallback) {
  const env = globalThis.process.env;
  if (!(name in env)) return defaultValue;
  try {
    return JSON.parse(env[name]);
  } catch {
    return rawFallback ? env[name] : defaultValue;
  }
}

const booleanEnv = (name, defaultValue) => Boolean(jsonEnv(name, defaultValue));

function resolveJitiOptions(userOptions) {
  const defaults = {
    fsCache: booleanEnv("JITI_FS_CACHE", booleanEnv("JITI_CACHE", true)),
    rebuildFsCache: booleanEnv("JITI_REBUILD_FS_CACHE", false),
    moduleCache: booleanEnv("JITI_MODULE_CACHE", booleanEnv("JITI_REQUIRE_CACHE", true)),
    debug: booleanEnv("JITI_DEBUG", false),
    sourceMaps: booleanEnv("JITI_SOURCE_MAPS", false),
    interopDefault: booleanEnv("JITI_INTEROP_DEFAULT", true),
    extensions: jsonEnv("JITI_EXTENSIONS", [...DEFAULT_EXTENSIONS]),
    alias: jsonEnv("JITI_ALIAS", {}),
    nativeModules: jsonEnv("JITI_NATIVE_MODULES", []),
    transformModules: jsonEnv("JITI_TRANSFORM_MODULES", []),
    tryNative: jsonEnv("JITI_TRY_NATIVE", "Bun" in globalThis),
    esmEvalTempFile: booleanEnv("JITI_ESM_EVAL_TEMP_FILE", false),
    jsx: booleanEnv("JITI_JSX", false),
    tsconfigPaths: jsonEnv("JITI_TSCONFIG_PATHS", false, true),
  };
  if (defaults.jsx) defaults.extensions.push(".jsx", ".tsx");
  const deprecated = {};
  if (userOptions.cache !== undefined) deprecated.fsCache = userOptions.cache;
  if (userOptions.requireCache !== undefined) deprecated.moduleCache = userOptions.requireCache;
  return { ...defaults, ...deprecated, ...userOptions };
}

function checkSupported(opts) {
  if (opts.transform !== undefined) {
    throw new JitiUnsupportedOptionError("transform", "modules are transpiled with oxc, not a custom transform");
  }
  if (opts.transformOptions !== undefined) {
    throw new JitiUnsupportedOptionError("transformOptions", "there is no Babel pipeline to configure");
  }
  if (opts.jsx) throw new JitiUnsupportedOptionError("jsx", "the React JSX transform is not available");
  if (opts.tsconfigPaths) {
    throw new JitiUnsupportedOptionError("tsconfigPaths", "tsconfig `paths` resolution is not available");
  }
  const extensions = opts.extensions ?? DEFAULT_EXTENSIONS;
  const same =
    Array.isArray(extensions) &&
    extensions.length === DEFAULT_EXTENSIONS.length &&
    DEFAULT_EXTENSIONS.every((e) => extensions.includes(e));
  if (!same) {
    throw new JitiUnsupportedOptionError("extensions", `only the default set (${DEFAULT_EXTENSIONS.join(", ")}) is resolved`);
  }
}

// --- interopDefault (jiti src/utils.ts) -----------------------------------------------------

function interopDefault(mod) {
  const modType = typeof mod;
  if (mod === null || (modType !== "object" && modType !== "function")) return mod;
  const def = mod.default;
  const defType = typeof def;
  const defIsNil = def === null || def === undefined;
  const defIsObj = defType === "object" || defType === "function";
  if (defIsNil && mod instanceof Promise) return mod;
  const needsApplyTrap = defType === "function" && modType !== "function";
  const needsDefaultFallback = defIsObj && !(def instanceof Promise);
  const cache = new Map();
  return new Proxy(mod, {
    get(target, prop) {
      if (cache.has(prop)) return cache.get(prop);
      let value;
      if (prop === "__esModule") {
        value = true;
      } else if (prop === "default") {
        if (defIsNil) value = mod;
        else if (typeof def?.default === "function" && mod.__esModule) value = def.default;
        else value = def;
      } else if (prop in target) {
        value = target[prop];
      } else if (needsDefaultFallback) {
        value = def[prop];
        if (typeof value === "function") value = value.bind(def);
      }
      cache.set(prop, value);
      return value;
    },
    apply: needsApplyTrap ? (_target, thisArg, args) => Reflect.apply(def, thisArg, args) : undefined,
  });
}

internal.jitiInteropDefault = interopDefault;

// jiti evaluates the modules it transpiles as CommonJS (Babel output): their exports are a plain
// object with a non-enumerable `__esModule`, not a module namespace. Getters keep live bindings.
function jitiExports(ns) {
  if (Object.prototype.toString.call(ns) !== "[object Module]") return ns;
  const exports = {};
  Object.defineProperty(exports, "__esModule", { value: true });
  for (const key of Object.keys(ns)) {
    Object.defineProperty(exports, key, { enumerable: true, configurable: true, get: () => ns[key] });
  }
  return exports;
}
internal.jitiExports = jitiExports;

// What Node's `import` of a CommonJS module gives: `default` is `module.exports`, next to its
// named exports (`__esModule` included when the module marks itself).
function nodeNamespace(exports) {
  const ns = Object.create(null);
  const names = new Set();
  if (exports !== null && (typeof exports === "object" || typeof exports === "function")) {
    for (const k of Object.keys(exports)) names.add(k);
    if (exports.__esModule) names.add("__esModule");
  }
  names.add("default");
  for (const k of [...names].sort()) ns[k] = k === "default" ? exports : exports[k];
  Object.defineProperty(ns, Symbol.toStringTag, { value: "Module" });
  return Object.freeze(ns);
}
internal.jitiNodeNamespace = nodeNamespace;

// Resolution failures read like jiti's (Node's `require.resolve`).
function requireNotFound(err) {
  if (err && err.code === "ERR_MODULE_NOT_FOUND") err.code = "MODULE_NOT_FOUND";
  return err;
}

// The exports a transpiled module sees when it imports a virtual module: what jiti's Babel
// output reads from the (interop) value: named properties and `default`.
internal.jitiVirtualRecord = (mod, interop) => {
  const record = Object.create(null);
  const isObject = mod !== null && (typeof mod === "object" || typeof mod === "function");
  const view = interop ? interopDefault(mod) : mod;
  if (isObject) {
    const names = new Set(Object.keys(mod));
    const def = mod.default;
    if (interop && def !== null && (typeof def === "object" || typeof def === "function") && !(def instanceof Promise)) {
      for (const k of Object.keys(def)) names.add(k);
    }
    names.delete("default");
    for (const k of names) record[k] = view[k];
  }
  record.default = isObject ? view.default : mod;
  Object.defineProperty(record, "__esModule", { value: true });
  return record;
};

// --- alias (pathe/utils normalizeAliases + resolveAlias) ------------------------------------

const pathSeparators = new Set(["/", "\\", undefined]);
const hasTrailingSlash = (p = "/") => p[p.length - 1] === "/" || p[p.length - 1] === "\\";

function normalizeAliases(input) {
  const aliases = Object.fromEntries(
    Object.entries(input).sort(([a], [b]) => b.split("/").length - a.split("/").length),
  );
  for (const key in aliases) {
    for (const alias in aliases) {
      if (alias === key || key.startsWith(alias)) continue;
      if (aliases[key]?.startsWith(alias) && pathSeparators.has(aliases[key][alias.length])) {
        aliases[key] = aliases[alias] + aliases[key].slice(alias.length);
      }
    }
  }
  return aliases;
}

function resolveAlias(id, aliases) {
  const p = id.replace(/\\/g, "/");
  for (const [alias, to] of Object.entries(aliases)) {
    if (!p.startsWith(alias)) continue;
    const trimmed = hasTrailingSlash(alias) ? alias.slice(0, -1) : alias;
    if (hasTrailingSlash(p[trimmed.length])) return path.join(to, p.slice(alias.length));
  }
  return p;
}

const aliasTables = [];
internal.jitiAlias = (scope, id) => {
  const aliases = aliasTables[scope];
  return aliases ? resolveAlias(id, aliases) : id;
};

// --- createJiti (jiti src/jiti.ts, src/require.ts, src/eval.ts) -----------------------------

function isDir(p) {
  if (typeof p !== "string" || p.startsWith("file://")) return false;
  try {
    return fs.lstatSync(p).isDirectory();
  } catch {
    return false;
  }
}

// A parent URL or path, as the filename resolution is relative to.
function parentPath(parent) {
  let p = parent instanceof internal.URL || String(parent).startsWith("file:") ? fileURLToPath(parent) : String(parent);
  if (isDir(p)) p = path.join(p, "_index.js");
  return p;
}

function createJiti(id, userOptions = {}) {
  const opts = resolveJitiOptions(userOptions);
  checkSupported(opts);

  let filename = id;
  if (typeof filename === "string" && filename.startsWith("file://")) filename = fileURLToPath(filename);
  const alias = opts.alias && Object.keys(opts.alias).length > 0 ? normalizeAliases(opts.alias) : undefined;
  if (!filename) filename = globalThis.process.cwd();
  if (isDir(filename)) filename = path.join(filename, "_index.js");
  const url = pathToFileURL(filename);
  const nativeRequire = internal.makeRequire(filename);

  const virtualModules = opts.virtualModules;
  const hasVirtual = virtualModules !== null && (typeof virtualModules === "object" || typeof virtualModules === "function");
  const scope = ops.jitiCreate(
    hasVirtual ? virtualModules : undefined,
    JSON.stringify({
      hasAlias: alias !== undefined,
      interopDefault: Boolean(opts.interopDefault),
      nativeModules: ["typescript", "jiti", ...(opts.nativeModules || [])],
      transformModules: [...(opts.transformModules || [])],
    }),
  );
  aliasTables[scope] = alias;

  const interop = (mod) => (opts.interopDefault ? interopDefault(mod) : mod);
  const generation = (isImport) => ops.jitiGeneration(scope, isImport, Boolean(opts.moduleCache));

  function resolveId(request, options = {}) {
    if (options.conditions !== undefined) {
      throw new JitiUnsupportedOptionError("conditions", "resolution uses Node's import/require conditions");
    }
    if (options.paths !== undefined) {
      throw new JitiUnsupportedOptionError("paths", "resolution starts from the parent module");
    }
    const parent = options.parentURL ? parentPath(options.parentURL) : filename;
    try {
      return ops.jitiResolve(scope, String(request), parent, Boolean(options.async));
    } catch (err) {
      if (options.try) return undefined;
      throw requireNotFound(err);
    }
  }

  // jiti's `jitiRequire`: `jiti(id)` (sync) and `jiti.import(id)` (async).
  function load(request, options, isAsync) {
    let spec = String(request);
    if (spec.startsWith("file:")) {
      spec = fileURLToPath(spec);
    } else if (spec.startsWith("data:")) {
      if (!isAsync) {
        throw new Error("`data:` URLs are only supported in ESM context. Use `import` or `jiti.import` instead.");
      }
      return import(spec).then(interop);
    }
    const parent = options?.parentURL ? parentPath(options.parentURL) : filename;
    const gen = generation(isAsync);
    if (options?.try) {
      try {
        ops.jitiTarget(gen, spec, parent);
      } catch {
        return undefined;
      }
    }
    let target;
    try {
      target = ops.jitiTarget(gen, spec, parent);
    } catch (err) {
      throw requireNotFound(err);
    }
    if (target.virtual) {
      const mod = interop(virtualModules[spec]);
      return isAsync ? Promise.resolve(mod) : mod;
    }
    if (target.cjs !== undefined && isAsync) {
      return new Promise((resolve) => resolve(interop(nodeNamespace(nativeRequire(target.cjs)))));
    }
    // `tryNative`: the runtime's own loader first (shared instances), jiti's when that fails.
    const native = opts.tryNative ? resolveId(spec, { parentURL: parent, async: isAsync, try: true }) : undefined;
    if (!isAsync) {
      if (native !== undefined) {
        try {
          return interop(nativeRequire(native));
        } catch {
          // Fall back to jiti's loading, like jiti.
        }
      }
      return ops.jitiRequire(gen, spec, parent);
    }
    const jitiImport = () => {
      const loaded = import(target.name);
      if (target.transpiled) return loaded.then(jitiExports);
      return target.fallback ? loaded.catch(() => import(target.fallback).then(jitiExports)) : loaded;
    };
    const loaded = native !== undefined ? import(native).catch(jitiImport) : jitiImport();
    return loaded.then(interop);
  }

  function evalModule(source, evalOptions = {}) {
    const evalId =
      evalOptions.id ||
      (evalOptions.filename ? path.basename(evalOptions.filename) : `_jitiEval.${evalOptions.ext || (evalOptions.async ? "mjs" : "js")}`);
    const evalFilename = evalOptions.filename || resolveId(evalId, { async: evalOptions.async });
    const result = ops.jitiEval(generation(Boolean(evalOptions.async)), String(source), evalFilename);
    if ("exports" in result) {
      const value = interop(result.exports);
      return evalOptions.async ? Promise.resolve(value) : value;
    }
    if (evalOptions.async) return import(result.name).then((ns) => interop(jitiExports(ns)));
    return interop(jitiExports(ops.jitiRequireModule(result.name)));
  }

  const jiti = Object.assign(
    function jiti(request) {
      return load(request, undefined, false);
    },
    {
      cache: opts.moduleCache ? nativeRequire.cache : Object.create(null),
      extensions: nativeRequire.extensions,
      main: nativeRequire.main,
      options: opts,
      resolve: Object.assign(
        function resolve(request, options) {
          return resolveId(request, { ...options, async: false });
        },
        { paths: nativeRequire.resolve.paths },
      ),
      transform() {
        throw new JitiUnsupportedOptionError("transform", "jiti.transform() produces Babel output, which the embedded jiti does not have");
      },
      evalModule(source, options) {
        return evalModule(source, options);
      },
      async import(request, options) {
        const mod = await load(request, options, true);
        return options?.default ? (mod?.default ?? mod) : mod;
      },
      esmResolve(request, options) {
        if (typeof options === "string") options = { parentURL: options };
        const resolved = resolveId(request, { parentURL: url, ...options, async: true });
        if (!resolved || typeof resolved !== "string" || resolved.startsWith("file://")) return resolved;
        return pathToFileURL(resolved).href;
      },
    },
  );
  return jiti;
}

// `require("jiti")` is the function with a `createJiti` property (lib/jiti.cjs); `import` gets it
// as the default export plus the named export `createJiti` (lib/jiti.mjs).
createJiti.createJiti = createJiti;
internal.JitiUnsupportedOptionError = JitiUnsupportedOptionError;
internal.defineBuiltin("jsrt:jiti", () => createJiti);
