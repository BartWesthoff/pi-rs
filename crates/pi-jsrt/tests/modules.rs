//! Module loading: ESM graphs, CommonJS interop, packages, JSON, virtual modules, aliases.

use std::collections::HashMap;
use std::path::Path;

use pi_jsrt::{JsRuntime, RuntimeOptions, VirtualModule};
use serde_json::json;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn fixture(root: &Path) {
    write(
        root,
        "main.ts",
        r#"
import { greet, Mode, Counter } from "./lib";
import data from "./data.json";
import cjsDefault, { named as cjsNamed } from "./dep.cjs";
import * as esm from "./esm.mjs";
import pkgEsm from "pkg-esm";
import pkgSub from "pkg-esm/sub";
import pkgCjs, { helper } from "pkg-cjs";
import fromDir from "./dir";
import { fromTs } from "./renamed.js";
import typeOnly, { type Unused } from "./types-only";
import { value as aliased } from "@alias/thing";
import { tool } from "virtual:tools";
import { ref } from "jsrt:host";
import modJs from "./modpkg/x.js";
import { cycleA } from "./cycle-a";

const lazy = await import("./lazy.js");
await new Promise((r) => setTimeout(r, 1));

export const result = {
  greet: greet("pi"),
  mode: Mode.Fast,
  counter: new Counter(2).inc().value,
  data,
  cjsDefault: cjsDefault.kind,
  cjsNamed,
  esm: esm.default + esm.extra,
  pkgEsm,
  pkgSub,
  pkgCjs: pkgCjs(),
  helper: helper(),
  fromDir,
  fromTs,
  typeOnly,
  aliased,
  tool: tool(),
  ref: typeof ref,
  lazy: lazy.lazyValue,
  meta: import.meta.url.startsWith("file://") && import.meta.url.endsWith("/main.ts"),
  dirname: __dirname.endsWith("proj") || __dirname.length > 0,
  filename: __filename.endsWith("main.ts"),
  req: require("./dep.cjs").named,
  reqJson: require("./data.json").name,
  reqEsm: require("./esm.mjs").extra,
  reqResolve: require.resolve("pkg-cjs").endsWith("index.js"),
  metaResolve: import.meta.resolve("./lib").endsWith("/lib.ts"),
  modJs,
  cycleA,
};
"#,
    );
    write(
        root,
        "lib.ts",
        r#"
export enum Mode { Slow = 1, Fast }
export class Counter {
  constructor(public value: number, private readonly step: number = 1) {}
  inc(): this { this.value += this.step; return this; }
}
export function greet(name: string): string { return `hello ${name}`; }
"#,
    );
    write(root, "data.json", r#"{ "name": "data", "list": [1, 2] }"#);
    write(
        root,
        "dep.cjs",
        r#"
const path = require("path");
const again = require("./dep2.cjs");
module.exports = { kind: "cjs:" + path.basename(__filename), named: again.n + 1 };
"#,
    );
    write(root, "dep2.cjs", "exports.n = 41;\n");
    write(
        root,
        "esm.mjs",
        "export default 'esm'; export const extra = '!';\n",
    );
    write(
        root,
        "node_modules/pkg-esm/package.json",
        r#"{ "name": "pkg-esm", "type": "module", "exports": { ".": { "types": "./x.d.ts", "import": "./esm/index.js", "require": "./cjs/index.cjs" }, "./sub": "./esm/sub.js" } }"#,
    );
    write(
        root,
        "node_modules/pkg-esm/esm/index.js",
        "export default 'pkg-esm:import';\n",
    );
    write(
        root,
        "node_modules/pkg-esm/esm/sub.js",
        "export default 'pkg-esm:sub';\n",
    );
    write(
        root,
        "node_modules/pkg-esm/cjs/index.cjs",
        "module.exports = 'pkg-esm:require';\n",
    );
    write(
        root,
        "node_modules/pkg-cjs/package.json",
        r#"{ "name": "pkg-cjs", "main": "index.js" }"#,
    );
    write(
        root,
        "node_modules/pkg-cjs/index.js",
        r#"
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.default = function () { return "pkg-cjs default"; };
exports.helper = function () { return "helper:" + require("pkg-esm"); };
"#,
    );
    write(
        root,
        "dir/index.ts",
        "const v: string = 'dir-index'; export default v;\n",
    );
    write(
        root,
        "renamed.ts",
        "export const fromTs = 'ts via .js specifier';\n",
    );
    write(
        root,
        "types-only.ts",
        "export type Unused = { a: number }; export interface I {}; export default 'types-only';\n",
    );
    write(root, "real-thing.ts", "export const value = 'aliased';\n");
    write(root, "lazy.js", "export const lazyValue = 'lazy';\n");
    write(root, "modpkg/package.json", r#"{ "type": "module" }"#);
    write(
        root,
        "modpkg/x.js",
        "export default typeof require === 'function' ? 'esm-with-require' : 'esm';\n",
    );
    write(
        root,
        "cycle-a.ts",
        "import { cycleB } from './cycle-b'; export const cycleA = 'a+' + cycleB(); \n",
    );
    write(
        root,
        "cycle-b.ts",
        "import { cycleA } from './cycle-a'; export function cycleB() { return 'b'; } export function late() { return cycleA; }\n",
    );
}

fn options(root: &Path) -> RuntimeOptions {
    let mut virtual_modules = HashMap::new();
    virtual_modules.insert(
        "virtual:tools".to_string(),
        VirtualModule::Esm("export function tool(): string { return 'virtual tool'; }".into()),
    );
    let mut aliases = HashMap::new();
    aliases.insert(
        "@alias/thing".to_string(),
        root.join("real-thing.ts").display().to_string(),
    );
    RuntimeOptions {
        cwd: Some(root.to_path_buf()),
        virtual_modules,
        aliases,
        ..Default::default()
    }
}

#[tokio::test]
async fn esm_graph_with_interop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    fixture(&root);
    let rt = JsRuntime::new(options(&root)).unwrap();
    let ns = match rt.import("./main.ts").await {
        Ok(ns) => ns,
        Err(e) => panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())),
    };
    let r = ns.get("result").await.unwrap();
    assert_eq!(
        r,
        json!({
            "greet": "hello pi", "mode": 2, "counter": 3,
            "data": { "name": "data", "list": [1, 2] },
            "cjsDefault": "cjs:dep.cjs", "cjsNamed": 42, "esm": "esm!",
            "pkgEsm": "pkg-esm:import", "pkgSub": "pkg-esm:sub",
            "pkgCjs": "pkg-cjs default", "helper": "helper:pkg-esm:require",
            "fromDir": "dir-index", "fromTs": "ts via .js specifier", "typeOnly": "types-only",
            "aliased": "aliased", "tool": "virtual tool", "ref": "function", "lazy": "lazy",
            "meta": true, "dirname": true, "filename": true,
            "req": 42, "reqJson": "data", "reqEsm": "!", "reqResolve": true, "metaResolve": true,
            "modJs": "esm-with-require", "cycleA": "a+b",
        })
    );
}

#[tokio::test]
async fn module_identity_and_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "state.ts",
        "export const state = { n: 0 }; export function bump() { return ++state.n; }\n",
    );
    write(
        root,
        "a.ts",
        "import { bump } from './state'; export const a = bump();\n",
    );
    write(
        root,
        "b.ts",
        "import { bump } from './state.ts'; export const b = bump();\n",
    );
    write(root, "c.cjs", "module.exports = require('./state.ts');\n");
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(root.to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let a = rt.import("./a.ts").await.unwrap();
    let b = rt.import("./b.ts").await.unwrap();
    assert_eq!(a.get("a").await.unwrap(), json!(1));
    assert_eq!(b.get("b").await.unwrap(), json!(2));
    let c = rt.require("./c.cjs").await.unwrap();
    assert_eq!(c.call_method("bump", vec![]).await.unwrap(), json!(3));

    // Re-import after clearing the cache re-evaluates the file.
    write(root, "a.ts", "export const a = 'changed';\n");
    assert_eq!(
        rt.import("./a.ts").await.unwrap().get("a").await.unwrap(),
        json!(1)
    );
    rt.clear_module_cache().await.unwrap();
    assert_eq!(
        rt.import("./a.ts").await.unwrap().get("a").await.unwrap(),
        json!("changed")
    );
}

#[tokio::test]
async fn module_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "bad-import.ts",
        "import { x } from './nope';\nexport default x;\n",
    );
    write(root, "bad-syntax.ts", "export const = ;\n");
    write(root, "throws.ts", "\n\nthrow new Error('at load');\n");
    write(
        root,
        "missing-export.mjs",
        "import { notThere } from './esm.mjs'; export default notThere;\n",
    );
    write(root, "esm.mjs", "export const there = 1;\n");
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(root.to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let e = rt.import("./bad-import.ts").await.unwrap_err().to_string();
    assert!(e.contains("Cannot find module './nope'"), "{e}");
    let e = rt.import("./bad-syntax.ts").await.unwrap_err().to_string();
    assert!(
        e.contains("SyntaxError") && e.contains("bad-syntax.ts:1:"),
        "{e}"
    );
    let e = rt.import("./throws.ts").await.unwrap_err();
    let js = e.as_js().unwrap();
    assert_eq!(js.message, "at load");
    assert!(
        js.stack.as_deref().unwrap_or("").contains("throws.ts:3:"),
        "{:?}",
        js.stack
    );
    let e = rt
        .import("./missing-export.mjs")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("notThere"), "{e}");
    let e = rt.import("left-pad").await.unwrap_err().to_string();
    assert!(e.contains("Cannot find module 'left-pad'"), "{e}");
    let e = rt
        .eval("require('./nope-cjs')")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Cannot find module './nope-cjs'"), "{e}");
}

#[tokio::test]
async fn virtual_module_kinds_and_runtime_registration() {
    let dir = tempfile::tempdir().unwrap();
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    rt.set_virtual_module(
        "v-json",
        VirtualModule::Json(json!({ "a": 1, "b": [true] })),
    );
    rt.set_virtual_module(
        "v-cjs",
        VirtualModule::CommonJs("exports.hello = (n) => 'hi ' + n;".into()),
    );
    rt.set_virtual_module(
        "v-expr",
        VirtualModule::Expr("{ Type: { String: () => ({ type: 'string' }) }, version: 3 }".into()),
    );
    rt.set_alias("@sinclair/typebox", "v-expr");
    rt.set_virtual_module(
        "v-esm",
        VirtualModule::Esm("import { version } from 'v-expr'; export const v = version * 2; export default 'esm-default';".into()),
    );
    let ns = rt
        .eval_module(
            r#"
            import j from "v-json";
            import { hello } from "v-cjs";
            import { Type } from "@sinclair/typebox";
            import esmDefault, { v } from "v-esm";
            const req = require("v-esm");
            export default { j, hi: hello("x"), t: Type.String(), v, esmDefault, reqV: req.v, reqDefault: req.default, reqExpr: require("@sinclair/typebox").version };
            "#,
            "virtual-test.ts",
        )
        .await
        .unwrap();
    let v = ns.get("default").await.unwrap();
    assert_eq!(v["j"], json!({ "a": 1, "b": [true] }));
    assert_eq!(v["hi"], json!("hi x"));
    assert_eq!(v["t"], json!({ "type": "string" }));
    assert_eq!(v["v"], json!(6));
    assert_eq!(v["esmDefault"], json!("esm-default"));
    assert_eq!(v["reqV"], json!(6));
    assert_eq!(v["reqDefault"], json!("esm-default"));
}

/// Real npm packages installed in `PI_JSRT_NPM_DIR` (skipped when unset), e.g. after
/// `npm install ms chalk typebox yaml zod diff minimatch uuid lodash-es nanoid strip-ansi @sinclair/typebox`.
#[tokio::test]
async fn real_npm_packages() {
    let Ok(dir) = std::env::var("PI_JSRT_NPM_DIR") else {
        eprintln!("skipping: PI_JSRT_NPM_DIR not set");
        return;
    };
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.into()),
        ..Default::default()
    })
    .unwrap();
    let ns = rt
        .eval_module(
            r#"
            import ms from "ms";
            import chalk from "chalk";
            import { Type } from "typebox";
            import { Value } from "typebox/value";
            import * as Sinclair from "@sinclair/typebox";
            import YAML from "yaml";
            import { z } from "zod";
            import { diffLines } from "diff";
            import { minimatch } from "minimatch";
            import { v4 as uuidv4, validate } from "uuid";
            import { chunk } from "lodash-es";
            import { nanoid } from "nanoid";
            import stripAnsi from "strip-ansi";
            const schema = Type.Object({ name: Type.String(), n: Type.Optional(Type.Number()) });
            const cjsMs = require("ms");
            export default {
                ms: ms("2 days"), cjsMs: cjsMs(1000),
                chalk: stripAnsi(chalk.red("red")),
                tb: Value.Check(schema, { name: "x" }) && !Value.Check(schema, { name: 1 }),
                sinclair: typeof Sinclair.Type.Object,
                yaml: YAML.parse("a: 1\nb: [x, y]\n"),
                zod: z.object({ a: z.number() }).safeParse({ a: "no" }).success,
                diff: diffLines("a\nb\n", "a\nc\n").length,
                mm: minimatch("src/a.ts", "src/**/*.ts"),
                uuid: validate(uuidv4()),
                chunk: chunk([1, 2, 3], 2),
                nanoid: nanoid().length,
            };
            "#,
            "npm-test.mjs",
        )
        .await
        .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
    let v = ns.get("default").await.unwrap();
    assert_eq!(
        v,
        json!({
            "ms": 172800000, "cjsMs": "1s", "chalk": "red", "tb": true, "sinclair": "function",
            "yaml": { "a": 1, "b": ["x", "y"] }, "zod": false, "diff": 3, "mm": true, "uuid": true,
            "chunk": [[1, 2], [3]], "nanoid": 21
        })
    );
}

/// `import` resolves a package like Node (and jiti): `main` without `exports`, and only Node's
/// conditions inside `exports` / `imports`. The bundler-only `module` field and condition used to
/// win, which loaded ESM builds that call `require()` (turndown's `lib/turndown.es.js`).
#[tokio::test]
async fn import_resolves_package_entries_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "node_modules/dual/package.json",
        r#"{ "name": "dual", "main": "lib/main.cjs.js", "module": "lib/mod.es.js" }"#,
    );
    write(
        root,
        "node_modules/dual/lib/main.cjs.js",
        "exports.which = 'main';\n",
    );
    write(
        root,
        "node_modules/dual/lib/mod.es.js",
        "export const which = 'module'; export const dep = require('./dep.cjs');\n",
    );
    write(
        root,
        "node_modules/dual/lib/dep.cjs",
        "module.exports = 1;\n",
    );
    write(
        root,
        "node_modules/cond/package.json",
        r##"{ "name": "cond", "exports": { ".": { "module": "./mod.mjs", "default": "./main.cjs" } }, "imports": { "#target": { "module": "./mod.mjs", "default": "./main.cjs" } } }"##,
    );
    write(
        root,
        "node_modules/cond/main.cjs",
        "exports.which = 'default';\n",
    );
    write(
        root,
        "node_modules/cond/mod.mjs",
        "export const which = 'module-condition';\n",
    );
    write(
        root,
        "node_modules/cond/self.mjs",
        "export { which } from '#target';\n",
    );
    write(
        root,
        "entry.mjs",
        r#"
import { which as dual } from "dual";
import { which as cond } from "cond";
import { which as condImports } from "./node_modules/cond/self.mjs";
export default { dual, cond, condImports };
"#,
    );
    let rt = JsRuntime::new(options(root)).unwrap();
    let ns = rt.import("./entry.mjs").await.unwrap();
    assert_eq!(
        ns.get("default").await.unwrap(),
        json!({ "dual": "main", "cond": "default", "condImports": "default" })
    );
}

/// After a module cache clear (each extension load, jiti `moduleCache: false`), a module that was
/// loaded before the clear still shares its own graph: its later `import()` must reuse the
/// instances it imported, not re-evaluate them (an extension's lazily imported UI module read an
/// empty copy of the extension's state module).
#[tokio::test]
async fn lazy_import_after_cache_clear_reuses_the_importers_modules() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "shared.mjs", "export const state = { n: 0 };\n");
    write(
        root,
        "reader.mjs",
        "import { state } from './shared.mjs';\nexport const read = () => state.n;\n",
    );
    write(
        root,
        "ext.mjs",
        r#"import { state } from "./shared.mjs";
export function bump() { state.n += 1; return state.n; }
export async function lazyRead() { const m = await import("./reader.mjs"); return m.read(); }
"#,
    );
    let rt = JsRuntime::new(options(root)).unwrap();
    let first = rt.import("./ext.mjs").await.unwrap();
    first.call_method("bump", vec![]).await.unwrap();
    assert_eq!(first.call_method("bump", vec![]).await.unwrap(), json!(2));
    // A later load (another extension) clears the cache: it gets fresh module instances...
    rt.clear_module_cache().await.unwrap();
    let second = rt.import("./ext.mjs").await.unwrap();
    assert_eq!(second.call_method("bump", vec![]).await.unwrap(), json!(1));
    // ...while each earlier module keeps resolving into its own instances.
    assert_eq!(
        first.call_method("lazyRead", vec![]).await.unwrap(),
        json!(2)
    );
    assert_eq!(
        second.call_method("lazyRead", vec![]).await.unwrap(),
        json!(1)
    );
}

// ---------------------------------------------------------------------------
// Embedded `jiti` (js/jiti.js)
// ---------------------------------------------------------------------------

const NODE_BIN: &str = env!("CARGO_BIN_EXE_pi-jsrt-node");

const SDK_SPEC: &str = "@earendil-works/pi-coding-agent";

/// The pi-subagents runner bootstrap pattern: `createJiti(import.meta.url, { tryNative: false,
/// moduleCache: false, virtualModules: { sdk } }).import("./x.ts")` gives the TS module (and the
/// modules it imports) the host's SDK object itself, whether or not the runtime knows the SDK
/// specifier.
#[tokio::test]
async fn jiti_virtual_modules_keep_sdk_identity() {
    for host_registers_sdk in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "x.ts",
            r#"
import * as ns from "@earendil-works/pi-coding-agent";
import sdkDefault, { createAgentSession, type AgentSession } from "@earendil-works/pi-coding-agent";
import { helper } from "./helper.ts";
let session: AgentSession | undefined = undefined;
export const seen = { createAgentSession, sdkDefault, viaNamespace: ns.createAgentSession, helper, session };
"#,
        );
        write(
            root,
            "helper.ts",
            "import { createAgentSession } from '@earendil-works/pi-coding-agent';\nexport const helper = createAgentSession;\n",
        );
        write(
            root,
            "sdk.mjs",
            "export function createAgentSession() { return 'session'; }\nexport const VERSION = '1.4.0';\n",
        );
        let sdk_import = if host_registers_sdk {
            SDK_SPEC
        } else {
            "./sdk.mjs"
        };
        write(
            root,
            "bootstrap.mjs",
            &format!(
                r#"
import {{ createJiti }} from "jiti";
import * as sdk from "{sdk_import}";
const jiti = createJiti(import.meta.url, {{
  tryNative: false,
  moduleCache: false,
  virtualModules: {{ "{SDK_SPEC}": sdk }},
}});
const a = await jiti.import("./x.ts");
const b = await jiti.import("./x.ts");
export default {{
  named: a.seen.createAgentSession === sdk.createAgentSession,
  namespaceProperty: a.seen.viaNamespace === sdk.createAgentSession,
  nested: a.seen.helper === sdk.createAgentSession,
  defaultIsSdk: a.seen.sdkDefault === sdk,
  freshPerImport: a.seen !== b.seen,
  call: a.seen.createAgentSession(),
}};
"#
            ),
        );
        let mut virtual_modules = HashMap::new();
        if host_registers_sdk {
            virtual_modules.insert(
                SDK_SPEC.to_string(),
                VirtualModule::EsmJs(std::fs::read_to_string(root.join("sdk.mjs")).unwrap()),
            );
        }
        let rt = JsRuntime::new(RuntimeOptions {
            cwd: Some(root.to_path_buf()),
            virtual_modules,
            ..Default::default()
        })
        .unwrap();
        let ns = rt
            .import("./bootstrap.mjs")
            .await
            .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
        assert_eq!(
            ns.get("default").await.unwrap(),
            json!({
                "named": true, "namespaceProperty": true, "nested": true, "defaultIsSdk": true,
                "freshPerImport": true, "call": "session",
            }),
            "host registers the SDK: {host_registers_sdk}"
        );
    }
}

/// `jiti` is a builtin that beats `node_modules`: an installed npm jiti (and its Babel bundle) is
/// never read, for `import`, `require` and nested loads alike. `node:jiti` is not a builtin.
#[tokio::test]
async fn jiti_builtin_never_loads_npm_jiti_or_babel() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let poison = "globalThis.__npmJitiLoaded = (globalThis.__npmJitiLoaded || []).concat(__filename); throw new Error('npm jiti loaded');\n";
    write(
        root,
        "node_modules/jiti/package.json",
        r#"{ "name": "jiti", "version": "2.7.0", "main": "./lib/jiti.cjs",
             "exports": { ".": { "import": "./lib/jiti.mjs", "require": "./lib/jiti.cjs" } } }"#,
    );
    write(root, "node_modules/jiti/lib/jiti.cjs", poison);
    write(
        root,
        "node_modules/jiti/lib/jiti.mjs",
        "throw new Error('npm jiti loaded');\n",
    );
    write(root, "node_modules/jiti/dist/babel.cjs", poison);
    write(root, "node_modules/jiti/dist/jiti.cjs", poison);
    write(
        root,
        "a.ts",
        "import { createJiti } from 'jiti';\nexport const a: string = typeof createJiti;\n",
    );
    write(
        root,
        "main.mjs",
        r#"
import { createJiti } from "jiti";
import createDefault from "jiti";
import { builtinModules } from "node:module";
const required = require("jiti");
const jiti = createJiti(import.meta.url);
const imported = await jiti.import("./a.ts");
const sync = jiti("./a.ts");
let nodePrefixed;
try {
  await import("node:jiti");
  nodePrefixed = "resolved";
} catch (err) {
  nodePrefixed = "not found";
}
export default {
  npm: globalThis.__npmJitiLoaded ?? null,
  babelInCache: Object.keys(require.cache).filter((k) => k.includes("babel") || k.includes("node_modules/jiti")),
  shape: [typeof createJiti, createDefault === createJiti, typeof required, required.createJiti === createJiti],
  imported: imported.a,
  sync: sync.a,
  nodePrefixed,
  listedAsNodeBuiltin: builtinModules.includes("jiti"),
  resolve: require.resolve("jiti"),
};
"#,
    );
    let rt = JsRuntime::new(options(root)).unwrap();
    let ns = rt
        .import("./main.mjs")
        .await
        .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
    assert_eq!(
        ns.get("default").await.unwrap(),
        json!({
            "npm": null, "babelInCache": [], "shape": ["function", true, "function", true],
            "imported": "function", "sync": "function", "nodePrefixed": "not found",
            "listedAsNodeBuiltin": false, "resolve": "jiti",
        })
    );
}

/// `moduleCache`, `tryNative`, `evalModule`, `alias` on `require`, `try`, and options that need
/// jiti's Babel pipeline (a named error instead of a silently different result).
#[tokio::test]
async fn jiti_options_and_unsupported_options() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "counter.ts",
        "export const state: { n: number } = { n: 0 };\nstate.n += 1;\n",
    );
    write(
        root,
        "uses-virtual.ts",
        "import { tag } from 'virtual-only';\nexport const tag2: string = tag + '!';\n",
    );
    write(root, "lib/value.ts", "export const value = 'aliased';\n");
    write(
        root,
        "cjs-ts.ts",
        "const { value } = require('#lib/value');\nmodule.exports = { value, kind: typeof require };\n",
    );
    write(
        root,
        "main.mjs",
        r##"
import { createJiti } from "jiti";
import * as path from "node:path";
const dir = path.dirname(import.meta.filename);
const out = {};
for (const [option, value] of [
  ["transform", () => ({ code: "" })],
  ["transformOptions", {}],
  ["jsx", true],
  ["tsconfigPaths", true],
  ["extensions", [".ts"]],
]) {
  try {
    createJiti(import.meta.url, { [option]: value });
    out[option] = "accepted";
  } catch (err) {
    out[option] = `${err.name} ${err.code} ${err.option}`;
  }
}
const cached = createJiti(import.meta.url, { moduleCache: true });
out.cachedShares = (await cached.import("./counter.ts")).state === (await cached.import("./counter.ts")).state;
const fresh = createJiti(import.meta.url, { moduleCache: false });
const first = await fresh.import("./counter.ts");
out.freshDiffers = first.state !== (await fresh.import("./counter.ts")).state;
out.freshCount = first.state.n;
const native = await import("./counter.ts");
const tryNative = createJiti(import.meta.url, { tryNative: true, moduleCache: false, virtualModules: { "virtual-only": { tag: "v" } } });
out.tryNativeShares = (await tryNative.import("./counter.ts")).state === native.state;
out.tryNativeFallback = (await tryNative.import("./uses-virtual.ts")).tag2;
out.evalCjs = fresh.evalModule("module.exports = { v: 1 + 1 };", { filename: path.join(dir, "e.cjs") }).v;
out.evalEsm = fresh.evalModule("import { state } from './counter.ts'; export const v: number = state.n + 2;", { filename: path.join(dir, "e.ts") }).v;
out.evalAsync = (await fresh.evalModule("export default 4;", { filename: path.join(dir, "e2.mjs"), async: true })).default;
try {
  fresh.transform({ source: "" });
  out.transform = "returned";
} catch (err) {
  out.transform = err.name;
}
const aliased = createJiti(import.meta.url, { alias: { "#lib": path.join(dir, "lib") } });
out.requireAlias = aliased("./cjs-ts.ts");
out.tryMissing = (await fresh.import("./missing", { try: true })) ?? null;
export default out;
"##,
    );
    let rt = JsRuntime::new(options(root)).unwrap();
    let ns = rt
        .import("./main.mjs")
        .await
        .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
    let unsupported =
        |option: &str| format!("JitiUnsupportedOptionError ERR_JITI_UNSUPPORTED_OPTION {option}");
    assert_eq!(
        ns.get("default").await.unwrap(),
        json!({
            "transform": unsupported("transform"),
            "transformOptions": unsupported("transformOptions"),
            "jsx": unsupported("jsx"),
            "tsconfigPaths": unsupported("tsconfigPaths"),
            "extensions": unsupported("extensions"),
            "cachedShares": true, "freshDiffers": true, "freshCount": 1,
            "tryNativeShares": true, "tryNativeFallback": "v!",
            "evalCjs": 2, "evalEsm": 3, "evalAsync": 4, "transform": "JitiUnsupportedOptionError",
            "requireAlias": { "value": "aliased", "kind": "function" },
            "tryMissing": null,
        })
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The differential fixtures (ESM, CJS, TS, default interop, alias, virtual) give exactly what npm
/// jiti 2.7.0 gives under Node (`tests/fixtures/jiti/expected.json`, recorded with
/// `tests/fixtures/jiti/jiti-differential.sh`).
#[test]
fn jiti_differential_fixtures_match_npm_jiti() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jiti");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixtures, dir.path());
    let out = std::process::Command::new(NODE_BIN)
        .arg("run.mjs")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "run.mjs failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    let want: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures.join("expected.json")).unwrap())
            .unwrap();
    assert_eq!(got, want);
}
