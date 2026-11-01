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
    write(root, "esm.mjs", "export default 'esm'; export const extra = '!';\n");
    write(
        root,
        "node_modules/pkg-esm/package.json",
        r#"{ "name": "pkg-esm", "type": "module", "exports": { ".": { "types": "./x.d.ts", "import": "./esm/index.js", "require": "./cjs/index.cjs" }, "./sub": "./esm/sub.js" } }"#,
    );
    write(root, "node_modules/pkg-esm/esm/index.js", "export default 'pkg-esm:import';\n");
    write(root, "node_modules/pkg-esm/esm/sub.js", "export default 'pkg-esm:sub';\n");
    write(root, "node_modules/pkg-esm/cjs/index.cjs", "module.exports = 'pkg-esm:require';\n");
    write(root, "node_modules/pkg-cjs/package.json", r#"{ "name": "pkg-cjs", "main": "index.js" }"#);
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
    write(root, "dir/index.ts", "const v: string = 'dir-index'; export default v;\n");
    write(root, "renamed.ts", "export const fromTs = 'ts via .js specifier';\n");
    write(root, "types-only.ts", "export type Unused = { a: number }; export interface I {}; export default 'types-only';\n");
    write(root, "real-thing.ts", "export const value = 'aliased';\n");
    write(root, "lazy.js", "export const lazyValue = 'lazy';\n");
    write(root, "modpkg/package.json", r#"{ "type": "module" }"#);
    write(root, "modpkg/x.js", "export default typeof require === 'function' ? 'esm-with-require' : 'esm';\n");
    write(root, "cycle-a.ts", "import { cycleB } from './cycle-b'; export const cycleA = 'a+' + cycleB(); \n");
    write(root, "cycle-b.ts", "import { cycleA } from './cycle-a'; export function cycleB() { return 'b'; } export function late() { return cycleA; }\n");
}

fn options(root: &Path) -> RuntimeOptions {
    let mut virtual_modules = HashMap::new();
    virtual_modules.insert(
        "virtual:tools".to_string(),
        VirtualModule::Esm("export function tool(): string { return 'virtual tool'; }".into()),
    );
    let mut aliases = HashMap::new();
    aliases.insert("@alias/thing".to_string(), root.join("real-thing.ts").display().to_string());
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
    write(root, "state.ts", "export const state = { n: 0 }; export function bump() { return ++state.n; }\n");
    write(root, "a.ts", "import { bump } from './state'; export const a = bump();\n");
    write(root, "b.ts", "import { bump } from './state.ts'; export const b = bump();\n");
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
    assert_eq!(rt.import("./a.ts").await.unwrap().get("a").await.unwrap(), json!(1));
    rt.clear_module_cache().await.unwrap();
    assert_eq!(rt.import("./a.ts").await.unwrap().get("a").await.unwrap(), json!("changed"));
}

#[tokio::test]
async fn module_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "bad-import.ts", "import { x } from './nope';\nexport default x;\n");
    write(root, "bad-syntax.ts", "export const = ;\n");
    write(root, "throws.ts", "\n\nthrow new Error('at load');\n");
    write(root, "missing-export.mjs", "import { notThere } from './esm.mjs'; export default notThere;\n");
    write(root, "esm.mjs", "export const there = 1;\n");
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(root.to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    let e = rt.import("./bad-import.ts").await.unwrap_err().to_string();
    assert!(e.contains("Cannot find module './nope'"), "{e}");
    let e = rt.import("./bad-syntax.ts").await.unwrap_err().to_string();
    assert!(e.contains("SyntaxError") && e.contains("bad-syntax.ts:1:"), "{e}");
    let e = rt.import("./throws.ts").await.unwrap_err();
    let js = e.as_js().unwrap();
    assert_eq!(js.message, "at load");
    assert!(js.stack.as_deref().unwrap_or("").contains("throws.ts:3:"), "{:?}", js.stack);
    let e = rt.import("./missing-export.mjs").await.unwrap_err().to_string();
    assert!(e.contains("notThere"), "{e}");
    let e = rt.import("left-pad").await.unwrap_err().to_string();
    assert!(e.contains("Cannot find module 'left-pad'"), "{e}");
    let e = rt.eval("require('./nope-cjs')").await.unwrap_err().to_string();
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
    rt.set_virtual_module("v-json", VirtualModule::Json(json!({ "a": 1, "b": [true] })));
    rt.set_virtual_module("v-cjs", VirtualModule::CommonJs("exports.hello = (n) => 'hi ' + n;".into()));
    rt.set_virtual_module("v-expr", VirtualModule::Expr("{ Type: { String: () => ({ type: 'string' }) }, version: 3 }".into()));
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
    write(root, "node_modules/dual/lib/main.cjs.js", "exports.which = 'main';\n");
    write(
        root,
        "node_modules/dual/lib/mod.es.js",
        "export const which = 'module'; export const dep = require('./dep.cjs');\n",
    );
    write(root, "node_modules/dual/lib/dep.cjs", "module.exports = 1;\n");
    write(
        root,
        "node_modules/cond/package.json",
        r##"{ "name": "cond", "exports": { ".": { "module": "./mod.mjs", "default": "./main.cjs" } }, "imports": { "#target": { "module": "./mod.mjs", "default": "./main.cjs" } } }"##,
    );
    write(root, "node_modules/cond/main.cjs", "exports.which = 'default';\n");
    write(root, "node_modules/cond/mod.mjs", "export const which = 'module-condition';\n");
    write(root, "node_modules/cond/self.mjs", "export { which } from '#target';\n");
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
    write(root, "reader.mjs", "import { state } from './shared.mjs';\nexport const read = () => state.n;\n");
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
    assert_eq!(first.call_method("lazyRead", vec![]).await.unwrap(), json!(2));
    assert_eq!(second.call_method("lazyRead", vec![]).await.unwrap(), json!(1));
}
