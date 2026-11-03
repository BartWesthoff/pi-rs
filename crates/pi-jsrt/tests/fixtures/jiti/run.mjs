// Differential driver for the embedded `jiti` (crates/pi-jsrt/js/jiti.js): runs the six fixture
// modules (ESM, CJS, TS, default interop, alias, virtual) through `createJiti` and prints a
// normalized JSON summary. `jiti-differential.sh` runs it under Node with npm jiti 2.7.0 and under
// pi-jsrt; tests/jiti.rs compares pi-jsrt's output with the recorded Node output (expected.json).
import { createJiti } from "jiti";
import defaultCreateJiti from "jiti";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import * as path from "node:path";

const root = path.dirname(fileURLToPath(import.meta.url));
const rel = (v) => (typeof v === "string" ? v.split(root).join("<root>") : v);

function summarize(v, depth = 0) {
  if (typeof v === "function") return `[function ${v.name}]`;
  if (v === null || typeof v !== "object") return v ?? null;
  if (depth > 2) return "[object]";
  const out = {};
  for (const k of Object.keys(v)) out[k] = summarize(v[k], depth + 1);
  return out;
}

function describe(mod) {
  if (mod === null || (typeof mod !== "object" && typeof mod !== "function")) return { primitive: mod ?? null };
  // Node adds a "module.exports" export to namespaces of CommonJS modules; jiti does not care.
  const keys = Object.keys(mod).filter((k) => k !== "module.exports");
  const values = {};
  for (const k of keys) values[k] = summarize(mod[k]);
  return {
    type: typeof mod,
    keys,
    esModule: "__esModule" in mod,
    defaultType: typeof mod.default,
    values,
  };
}

const sdk = {
  createAgentSession() {
    return "session";
  },
  VERSION: "1.4.0",
};
globalThis.__jitiFixtureSdk = sdk;

const base = { tryNative: false, moduleCache: false };
const jiti = createJiti(import.meta.url, base);
const results = {};

// 1. ESM
results.esm = {
  import: describe(await jiti.import("./esm.mjs")),
  require: describe(jiti("./esm.mjs")),
  importDefault: summarize(await jiti.import("./esm.mjs", { default: true })),
};

// 2. CJS
{
  const imported = await jiti.import("./cjs.cjs");
  const required = jiti("./cjs.cjs");
  imported.bump();
  results.cjs = { import: describe(imported), require: describe(required), sharedCount: required.count };
}

// 3. TS
{
  const first = await jiti.import("./ts-module.ts");
  const second = await jiti.import("./ts-module.ts");
  results.ts = {
    import: describe(first),
    require: describe(jiti("./ts-module.ts")),
    freshPerImport: first.state !== second.state,
    extensionless: describe(await jiti.import("./ts-module")).keys,
  };
}

// 4. default interop
{
  const mod = await jiti.import("./default-interop.ts");
  const plain = await createJiti(import.meta.url, { ...base, interopDefault: false }).import("./default-interop.ts");
  results.defaultInterop = {
    import: describe(mod),
    label: mod.label ?? null,
    shout: mod.shout ? mod.shout("loud") : null,
    defaultOption: summarize(await jiti.import("./default-interop.ts", { default: true })),
    noInterop: describe(plain),
    noInteropLabel: plain.label ?? null,
    legacy: describe(await jiti.import("./legacy.cjs")),
    legacyRequire: describe(jiti("./legacy.cjs")),
  };
}

// 5. alias
{
  const aliasJiti = createJiti(import.meta.url, { ...base, alias: { "@fixture/aliased": path.join(root, "aliased") } });
  results.alias = {
    import: describe(await aliasJiti.import("./alias.ts")),
    resolve: rel(aliasJiti.resolve("@fixture/aliased")),
    esmResolve: rel(aliasJiti.esmResolve("@fixture/aliased/deep")),
  };
}

// 6. virtual
{
  const virtualJiti = createJiti(import.meta.url, {
    ...base,
    virtualModules: { "@earendil-works/pi-coding-agent": sdk },
  });
  const mod = await virtualJiti.import("./virtual.ts");
  const direct = await virtualJiti.import("@earendil-works/pi-coding-agent");
  results.virtual = {
    import: describe(mod),
    identity: mod.identity,
    requireIdentity: virtualJiti("./virtual.ts").identity,
    direct: { same: direct.createAgentSession === sdk.createAgentSession, wrapped: direct !== sdk },
    esmResolveVirtual: (() => {
      try {
        return virtualJiti.esmResolve("@earendil-works/pi-coding-agent");
      } catch (err) {
        return err.code;
      }
    })(),
  };
}

// Module shape and resolution.
{
  const require = createRequire(import.meta.url);
  const cjsJiti = require("jiti");
  let missing;
  try {
    await jiti.import("./missing");
  } catch (err) {
    missing = err.code;
  }
  results.shape = {
    defaultExport: defaultCreateJiti === createJiti,
    requireType: typeof cjsJiti,
    requireCreateJiti: typeof cjsJiti.createJiti,
    instanceKeys: Object.keys(jiti).sort(),
    instanceType: typeof jiti,
    cacheType: typeof jiti.cache,
    resolveTs: rel(jiti.resolve("./ts-module")),
    esmResolveTs: rel(jiti.esmResolve("./ts-module.ts")),
    resolveBuiltin: jiti.resolve("fs"),
    resolveNodeBuiltin: jiti.resolve("node:path"),
    tryMissing: jiti.esmResolve("./missing", { try: true }) ?? null,
    missingImport: missing,
    builtinImport: typeof (await jiti.import("node:path")).join,
  };
}

console.log(JSON.stringify(results, null, 2));
