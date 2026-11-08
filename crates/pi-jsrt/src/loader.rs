//! Module resolution and loading: ES modules, CommonJS, JSON, builtins and
//! virtual modules, with Node-style interop between them.
//!
//! Resolution order for a specifier:
//! 1. embedder aliases (exact, or `alias/` prefix),
//! 2. embedder virtual modules (exact),
//! 3. builtins (`node:fs`, `fs`, `jsrt:host`, ...),
//! 4. `file:` URLs, absolute and relative paths (with extension / index probing,
//!    including `./x.js` -> `./x.ts` like TypeScript's resolver),
//! 5. packages in `node_modules` (walking up), honoring `package.json` `exports`
//!    (with `import`/`require`/`node`/`default` conditions and `*` patterns),
//!    `module` (for `import`) and `main`.
//!
//! File format: `.mjs/.mts` ESM, `.cjs/.cts` CommonJS, `.json` JSON; `.js` follows the
//! nearest `package.json` `type` and otherwise ES module syntax detection; `.ts`
//! uses syntax detection (like jiti). TypeScript is transpiled with oxc.
//! ES modules outside `node_modules` get jiti-compatible `__filename`, `__dirname`
//! and `require` bindings unless they declare those names themselves.
//!
//! The embedded `jiti` module (`js/jiti.js`, specifier `jiti`, which beats `node_modules`) loads
//! module trees in *jiti generations*: a cache generation tied to one `createJiti` instance. A
//! module loaded in a jiti generation resolves imports the way jiti 2.x does: builtins, then the
//! instance's `virtualModules`, then its `alias` and the regular resolution. Files jiti would
//! transpile (TypeScript, ES modules it cannot import natively) load in the generation, so a
//! `moduleCache: false` import gets fresh instances; files it loads natively (CommonJS, JSON,
//! ES modules whose graph links natively) are shared with the rest of the runtime.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::js::ext;
use crate::js::rquickjs::qjs;
use crate::js::{
    Context, Engine, JsArgs, JsArray, JsError, JsNativeError, JsObject, JsPromise, JsResult,
    JsString, JsValue, Source, js_string,
};
use serde_json::Value;

use crate::engine::{self, arg_opt_string, arg_string, internal, js_str, state};
use crate::error::{self, Error};
use crate::runtime::{Shared, VirtualModule};
use crate::transpile::{self, SourceLang};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Resolved {
    Builtin(String),
    Virtual(String),
    File(PathBuf),
    /// An entry of a jiti instance's `virtualModules` (instance index, specifier).
    JitiVirtual(usize, String),
    /// A CommonJS file imported by a module jiti loads (instance index, file): jiti imports it
    /// natively, so the importer sees Node's namespace of it (`default` is `module.exports`)
    /// through jiti's interop. The file itself runs once, shared with the runtime.
    JitiCjs(usize, PathBuf),
}

impl Resolved {
    /// The module name (QuickJS caches modules by name): files carry the cache generation
    /// (see [`Loader::clear_files`]).
    fn key_at(&self, generation: u32) -> String {
        match self {
            Resolved::Builtin(n) => format!("builtin:{n}"),
            Resolved::Virtual(s) => format!("virtual:{s}"),
            Resolved::File(p) if generation == 0 => p.display().to_string(),
            Resolved::File(p) => format!("{}{RELOAD_MARK}{generation}", p.display()),
            // jiti hands out a new interop view per load: one module per generation.
            Resolved::JitiVirtual(scope, s) => format!("jiti-virtual:{scope}:{generation}:{s}"),
            Resolved::JitiCjs(scope, p) => format!("jiti-cjs:{scope}:{generation}:{}", p.display()),
        }
    }
}

/// Suffix of file module names after a cache clear, and of repeated `eval_module` names.
const RELOAD_MARK: &str = "?reload=";
const EVAL_MARK: &str = "?eval=";

/// The file path behind a module name (without the reload / eval suffix).
pub(crate) fn module_path(name: &str) -> &str {
    let end = [RELOAD_MARK, EVAL_MARK]
        .iter()
        .filter_map(|m| name.find(m))
        .min()
        .unwrap_or(name.len());
    &name[..end]
}

/// A module to evaluate (see [`evaluate_module`]): its QuickJS module name.
pub(crate) struct Module(String);

/// Exports of a module made from a JS value (builtins, CommonJS, JSON), set when QuickJS
/// initializes the module.
struct Synthetic {
    value: JsValue,
    default: JsValue,
    names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Import,
    Require,
}

impl Kind {
    /// The package.json `exports` / `imports` conditions Node matches for this kind of load.
    /// Bundler-only conditions such as `module` are not among them.
    fn conditions(self) -> &'static [&'static str] {
        match self {
            Kind::Import => &["import", "node", "default"],
            Kind::Require => &["require", "node", "default"],
        }
    }
}

/// A `createJiti` instance, as far as module loading is concerned.
struct JitiScope {
    /// The `virtualModules` option (looked up with `in`, like jiti does).
    virtual_modules: Option<JsObject>,
    /// Whether the instance has `alias` entries (applied by `internal.jitiAlias`).
    has_alias: bool,
    /// Whether loaded modules go through jiti's `interopDefault` proxy.
    interop_default: bool,
    /// `nativeModules` (always `typescript` and `jiti` too) and `transformModules` package names.
    native_modules: Vec<String>,
    transform_modules: Vec<String>,
    /// The generation reused while `moduleCache` is on, per kind of load.
    cached: [Option<u32>; 2],
}

/// A jiti generation: the instance it belongs to and how its static imports load (an import
/// tree uses `jiti.import` semantics, a require tree `jiti()` semantics).
#[derive(Debug, Clone, Copy)]
struct JitiGeneration {
    scope: usize,
    kind: Kind,
}

/// How jiti loads a resolved module (see [`Loader::resolve_jiti`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JitiLoad {
    /// Natively: shared with the rest of the runtime.
    Native,
    /// Transpiled: in the jiti generation.
    Transpile,
    /// A native ES module under `import`: jiti tries a native import first and transpiles when
    /// that fails.
    TryNative,
}

/// Extensions jiti handles itself (others load natively).
const JITI_EXTENSIONS: &[&str] = &[
    "js", "mjs", "cjs", "ts", "tsx", "mts", "cts", "mtsx", "ctsx",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Esm,
    Cjs,
    Json,
    /// A native addon (`.node`): loaded with `dlopen`, never compiled as source.
    Native,
}

struct Prepared {
    code: String,
    format: Format,
}

/// The module loader installed in the QuickJS runtime.
pub(crate) struct Loader {
    shared: Arc<Shared>,
    /// Module name -> what it loads (filled when names are handed to QuickJS).
    names: RefCell<HashMap<String, Resolved>>,
    /// ES module source text for names created by [`module_from_source`].
    sources: RefCell<HashMap<String, String>>,
    /// Module names QuickJS has loaded.
    loaded: RefCell<std::collections::HashSet<String>>,
    synthetic: RefCell<HashMap<String, Synthetic>>,
    cjs: RefCell<HashMap<String, JsObject>>,
    values: RefCell<HashMap<String, JsValue>>,
    pkg_json: RefCell<HashMap<PathBuf, Option<Rc<Value>>>>,
    /// Addons loaded by this loader; their cleanup hooks run when the runtime stops.
    native: RefCell<Vec<crate::native::Loaded>>,
    generation: Cell<u32>,
    /// Last generation number handed out (cache clears and jiti generations share the space).
    last_generation: Cell<u32>,
    evals: Cell<u32>,
    jiti_scopes: RefCell<Vec<JitiScope>>,
    jiti_generations: RefCell<HashMap<u32, JitiGeneration>>,
    /// Whether a file's static import graph resolves natively, per jiti generation.
    native_links: RefCell<HashMap<(u32, PathBuf), bool>>,
}

impl Loader {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Loader {
            shared,
            names: RefCell::default(),
            sources: RefCell::default(),
            loaded: RefCell::default(),
            synthetic: RefCell::default(),
            cjs: RefCell::default(),
            values: RefCell::default(),
            pkg_json: RefCell::default(),
            native: RefCell::default(),
            generation: Cell::new(0),
            last_generation: Cell::new(0),
            evals: Cell::new(0),
            jiti_scopes: RefCell::default(),
            jiti_generations: RefCell::default(),
            native_links: RefCell::default(),
        }
    }

    /// The module name for `resolved`, registered for the QuickJS loader callback.
    fn module_name(&self, resolved: &Resolved) -> String {
        self.module_name_at(resolved, self.generation.get())
    }

    /// The module name for `resolved` in cache `generation`.
    fn module_name_at(&self, resolved: &Resolved, generation: u32) -> String {
        let name = resolved.key_at(generation);
        self.names
            .borrow_mut()
            .entry(name.clone())
            .or_insert_with(|| resolved.clone());
        name
    }

    /// The cache generation of the module named `name`. Imports from a module resolve in its
    /// generation, so a module loaded before a cache clear keeps sharing the instances it
    /// imported (like jiti's per-load module cache); other names use the current generation.
    fn generation_of(&self, name: &str) -> u32 {
        if let Some(i) = name.rfind(RELOAD_MARK)
            && let Ok(generation) = name[i + RELOAD_MARK.len()..].parse()
        {
            return generation;
        }
        match self.names.borrow().get(name) {
            Some(Resolved::File(_)) => 0,
            _ => self.generation.get(),
        }
    }

    /// jiti-style `__filename`/`__dirname`/`require` for ES modules, unless disabled
    /// ([`crate::RuntimeOptions::esm_cjs_globals`]).
    /// In a jiti generation, the injected `require` resolves like the jiti instance's.
    fn cjs_globals(&self, code: &str, declared: &[&str], jiti: Option<u32>) -> String {
        if self.shared.options.esm_cjs_globals {
            inject_cjs_globals(code, declared, jiti)
        } else {
            code.to_string()
        }
    }

    pub(crate) fn clear(&self) {
        for mut addon in self.native.borrow_mut().drain(..) {
            addon.destroy();
        }
        self.synthetic.borrow_mut().clear();
        self.cjs.borrow_mut().clear();
        self.values.borrow_mut().clear();
        self.jiti_scopes.borrow_mut().clear();
        self.jiti_generations.borrow_mut().clear();
    }

    /// A generation number not used before.
    fn new_generation(&self) -> u32 {
        let next = self.last_generation.get() + 1;
        self.last_generation.set(next);
        next
    }

    /// Forget loaded files: QuickJS cannot unload modules, so later loads use new module names
    /// (a new generation), which re-reads and re-evaluates the files.
    fn clear_files(&self) {
        self.generation.set(self.new_generation());
        self.native_links.borrow_mut().clear();
        self.cjs
            .borrow_mut()
            .retain(|k, _| k.starts_with("builtin:") || k.starts_with("virtual:"));
        self.values
            .borrow_mut()
            .retain(|k, _| k.starts_with("builtin:") || k.starts_with("virtual:"));
        self.pkg_json.borrow_mut().clear();
    }

    // -----------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------

    fn apply_alias(&self, spec: &str) -> String {
        let aliases = self.shared.aliases.read();
        if aliases.is_empty() {
            return spec.to_string();
        }
        let mut current = spec.to_string();
        for _ in 0..8 {
            if let Some(t) = aliases.get(&current) {
                current = t.clone();
                continue;
            }
            let mut best: Option<(&String, &String)> = None;
            for (from, to) in aliases.iter() {
                if current.len() > from.len()
                    && current.starts_with(from.as_str())
                    && current.as_bytes()[from.len()] == b'/'
                    && best.is_none_or(|(b, _)| from.len() > b.len())
                {
                    best = Some((from, to));
                }
            }
            match best {
                Some((from, to)) => current = format!("{to}{}", &current[from.len()..]),
                None => break,
            }
        }
        current
    }

    pub(crate) fn resolve(
        &self,
        ctx: &mut Context,
        spec: &str,
        base_dir: &Path,
        kind: Kind,
    ) -> Result<Resolved, Error> {
        let resolved = self.resolve_spec(ctx, spec, base_dir, kind)?;
        // An alias keyed by an absolute file path redirects every import resolving to that file.
        if let Resolved::File(path) = &resolved {
            let target = self
                .shared
                .aliases
                .read()
                .get(&*path.to_string_lossy())
                .cloned();
            if let Some(target) = target {
                return self.resolve_spec(ctx, &target, base_dir, kind);
            }
        }
        Ok(resolved)
    }

    fn resolve_spec(
        &self,
        ctx: &mut Context,
        spec: &str,
        base_dir: &Path,
        kind: Kind,
    ) -> Result<Resolved, Error> {
        let spec = self.apply_alias(spec);
        if self.shared.virtual_modules.read().contains_key(&spec) {
            return Ok(Resolved::Virtual(spec));
        }
        if let Some(name) = builtin_name(ctx, &spec) {
            return Ok(Resolved::Builtin(name));
        }
        let not_found = || Error::ModuleNotFound {
            specifier: spec.clone(),
            referrer: base_dir.display().to_string(),
        };
        if let Some(rest) = spec.strip_prefix("file://") {
            let path = url::Url::parse(&format!("file://{rest}"))
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .unwrap_or_else(|| PathBuf::from(rest));
            return self
                .resolve_path(&path)
                .map(Resolved::File)
                .ok_or_else(not_found);
        }
        if spec.starts_with('/') {
            return self
                .resolve_path(&normalize(Path::new(&spec)))
                .map(Resolved::File)
                .ok_or_else(not_found);
        }
        if spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../") {
            let p = normalize(&base_dir.join(&spec));
            return self
                .resolve_path(&p)
                .map(Resolved::File)
                .ok_or_else(not_found);
        }
        if spec.starts_with('#') {
            return self
                .resolve_imports(&spec, base_dir, kind)
                .map(Resolved::File)
                .ok_or_else(not_found);
        }
        self.resolve_package(&spec, base_dir, kind)
            .map(Resolved::File)
            .ok_or_else(not_found)
    }

    /// package.json `imports` (`#internal` specifiers) of the nearest package.
    fn resolve_imports(&self, spec: &str, base_dir: &Path, kind: Kind) -> Option<PathBuf> {
        let mut dir = Some(base_dir);
        while let Some(d) = dir {
            if let Some(pkg) = self.package_json(d) {
                let imports = pkg.get("imports")?;
                let target = resolve_exports(imports, spec, kind.conditions())?;
                if target.starts_with("./") || target.starts_with("../") {
                    return self.resolve_file_or_index(&normalize(&d.join(target)));
                }
                // A bare target refers to a dependency.
                return self.resolve_package(&target, d, kind);
            }
            dir = d.parent();
        }
        None
    }

    fn resolve_path(&self, p: &Path) -> Option<PathBuf> {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        let s = p.to_string_lossy();
        // TypeScript-style: "./x.js" may refer to "./x.ts".
        for (from, tos) in [
            (".js", &[".ts", ".tsx"][..]),
            (".mjs", &[".mts"][..]),
            (".cjs", &[".cts"][..]),
            (".jsx", &[".tsx"][..]),
        ] {
            if let Some(stem) = s.strip_suffix(from) {
                for to in tos {
                    let c = PathBuf::from(format!("{stem}{to}"));
                    if c.is_file() {
                        return Some(c);
                    }
                }
            }
        }
        for ext in EXTENSIONS {
            let c = PathBuf::from(format!("{s}{ext}"));
            if c.is_file() {
                return Some(c);
            }
        }
        if p.is_dir() {
            if let Some(pkg) = self.package_json(p) {
                // Node (and jiti) use `main` for `import` too; `module` is a bundler field.
                let main = pkg.get("main").and_then(|v| v.as_str());
                if let Some(main) = main
                    && let Some(r) = self.resolve_file_or_index(&normalize(&p.join(main)))
                {
                    return Some(r);
                }
            }
            for ext in EXTENSIONS {
                let c = p.join(format!("index{ext}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        None
    }

    fn resolve_file_or_index(&self, p: &Path) -> Option<PathBuf> {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        let s = p.to_string_lossy();
        for ext in EXTENSIONS {
            let c = PathBuf::from(format!("{s}{ext}"));
            if c.is_file() {
                return Some(c);
            }
        }
        for ext in EXTENSIONS {
            let c = p.join(format!("index{ext}"));
            if c.is_file() {
                return Some(c);
            }
        }
        None
    }

    fn package_json(&self, dir: &Path) -> Option<Rc<Value>> {
        if let Some(v) = self.pkg_json.borrow().get(dir) {
            return v.clone();
        }
        let v = std::fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(Rc::new);
        self.pkg_json
            .borrow_mut()
            .insert(dir.to_path_buf(), v.clone());
        v
    }

    /// Nearest package.json `type` for a file.
    fn package_type(&self, file: &Path) -> Option<String> {
        let mut dir = file.parent();
        while let Some(d) = dir {
            if let Some(pkg) = self.package_json(d) {
                return pkg.get("type").and_then(|t| t.as_str()).map(String::from);
            }
            if d.file_name().is_some_and(|n| n == "node_modules") {
                return None;
            }
            dir = d.parent();
        }
        None
    }

    fn resolve_package(&self, spec: &str, base_dir: &Path, kind: Kind) -> Option<PathBuf> {
        let (name, subpath) = split_package(spec)?;
        let mut dir = Some(base_dir.to_path_buf());
        while let Some(d) = dir {
            let candidate = d.join("node_modules").join(name);
            if candidate.is_dir() {
                return self.resolve_in_package(&candidate, subpath, kind);
            }
            // Self-reference: a package importing itself by name.
            if let Some(pkg) = self.package_json(&d)
                && pkg.get("name").and_then(|n| n.as_str()) == Some(name)
                && pkg.get("exports").is_some()
            {
                return self.resolve_in_package(&d, subpath, kind);
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        None
    }

    fn resolve_in_package(&self, pkg_dir: &Path, subpath: &str, kind: Kind) -> Option<PathBuf> {
        let pkg = self.package_json(pkg_dir);
        if let Some(exports) = pkg.as_ref().and_then(|p| p.get("exports")) {
            let sub = if subpath.is_empty() {
                ".".to_string()
            } else {
                format!("./{subpath}")
            };
            if let Some(target) = resolve_exports(exports, &sub, kind.conditions()) {
                let p = normalize(&pkg_dir.join(target));
                if p.is_file() {
                    return Some(p);
                }
                if let Some(p) = self.resolve_file_or_index(&p) {
                    return Some(p);
                }
            }
            // Lenient fallback (Node would throw ERR_PACKAGE_PATH_NOT_EXPORTED).
        }
        if subpath.is_empty() {
            self.resolve_path(pkg_dir)
        } else {
            self.resolve_path(&normalize(&pkg_dir.join(subpath)))
        }
    }

    // -----------------------------------------------------------------------
    // jiti (see `js/jiti.js`)
    // -----------------------------------------------------------------------

    /// The jiti generation numbered `generation`, if it is one.
    fn jiti_generation(&self, generation: u32) -> Option<JitiGeneration> {
        self.jiti_generations.borrow().get(&generation).copied()
    }

    /// The jiti generation the module named `name` belongs to, if any.
    fn jiti_generation_of(&self, name: &str) -> Option<u32> {
        let generation = self.generation_of(name);
        self.jiti_generation(generation).map(|_| generation)
    }

    /// Resolve `spec`, loaded (`kind`) from a module of jiti generation `jg`, like jiti's
    /// `jitiRequire`: builtins first, then the instance's virtual modules, then its aliases and
    /// the regular resolution (with the other kind's conditions as a fallback).
    fn resolve_jiti(
        &self,
        ctx: &mut Context,
        spec: &str,
        base_dir: &Path,
        jg: JitiGeneration,
        kind: Kind,
    ) -> Result<(Resolved, JitiLoad), Error> {
        if let Some(name) = builtin_name(ctx, spec) {
            return Ok((Resolved::Builtin(name), JitiLoad::Native));
        }
        // Copied out so no borrow is held while JS runs (`in` may hit a proxy trap).
        let (virtual_modules, has_alias) = {
            let scopes = self.jiti_scopes.borrow();
            let scope = scopes
                .get(jg.scope)
                .ok_or_else(|| Error::host("the jiti instance was released"))?;
            (scope.virtual_modules.clone(), scope.has_alias)
        };
        if let Some(vm) = virtual_modules
            && vm
                .has_property(spec, ctx)
                .map_err(|e| error::from_js(e, ctx))?
        {
            return Ok((
                Resolved::JitiVirtual(jg.scope, spec.to_string()),
                JitiLoad::Native,
            ));
        }
        let spec = if has_alias {
            jiti_alias(ctx, jg.scope, spec).map_err(|e| error::from_js(e, ctx))?
        } else {
            spec.to_string()
        };
        let resolved = self.resolve_either(ctx, &spec, base_dir, kind)?;
        let load = match &resolved {
            Resolved::File(path) => self.jiti_load(path, jg.scope, kind),
            _ => JitiLoad::Native,
        };
        if let (Resolved::File(path), JitiLoad::Native, Kind::Import) = (&resolved, load, kind)
            && path.extension().is_none_or(|e| e != "json")
            && self
                .prepare_file(ctx, path, None)
                .is_ok_and(|p| p.format == Format::Cjs)
        {
            return Ok((Resolved::JitiCjs(jg.scope, path.clone()), JitiLoad::Native));
        }
        Ok((resolved, load))
    }

    /// Resolve with `kind`'s conditions, then the other kind's (as jiti tries both).
    fn resolve_either(
        &self,
        ctx: &mut Context,
        spec: &str,
        base_dir: &Path,
        kind: Kind,
    ) -> Result<Resolved, Error> {
        let other = match kind {
            Kind::Import => Kind::Require,
            Kind::Require => Kind::Import,
        };
        self.resolve(ctx, spec, base_dir, kind)
            .or_else(|e| self.resolve(ctx, spec, base_dir, other).map_err(|_| e))
    }

    /// How jiti loads the file `path` (jiti's `evalModule` `needsTranspile`).
    fn jiti_load(&self, path: &Path, scope: usize, kind: Kind) -> JitiLoad {
        let (native, transform) = {
            let scopes = self.jiti_scopes.borrow();
            let Some(s) = scopes.get(scope) else {
                return JitiLoad::Native;
            };
            (
                in_packages(path, &s.native_modules),
                in_packages(path, &s.transform_modules),
            )
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if native
            || ext == "json"
            || ext == "cjs"
            || !(ext.is_empty() || JITI_EXTENSIONS.contains(&ext))
        {
            return JitiLoad::Native;
        }
        let typescript = matches!(ext, "ts" | "tsx" | "mts" | "cts" | "mtsx" | "ctsx");
        let esm =
            ext == "mjs" || (ext == "js" && self.package_type(path).as_deref() == Some("module"));
        if esm && kind == Kind::Import {
            return JitiLoad::TryNative;
        }
        if typescript || esm || transform || self.has_module_syntax(path) {
            JitiLoad::Transpile
        } else {
            JitiLoad::Native
        }
    }

    fn read_source(&self, path: &Path) -> Option<String> {
        let source = self.shared.file_sources.read().get(path).cloned();
        source.or_else(|| std::fs::read_to_string(path).ok())
    }

    fn has_module_syntax(&self, path: &Path) -> bool {
        self.read_source(path).is_some_and(|text| {
            transpile::transpile(&text, path, SourceLang::JavaScript)
                .is_ok_and(|out| out.has_module_syntax)
        })
    }

    /// Whether a native import of `path` would link: every static import in its graph resolves
    /// and parses (jiti's native-first `import` falls back to transpiling when it does not).
    fn links_natively(
        &self,
        ctx: &mut Context,
        path: &Path,
        generation: u32,
        visiting: &mut std::collections::HashSet<PathBuf>,
    ) -> bool {
        let memo = (generation, path.to_path_buf());
        if let Some(&links) = self.native_links.borrow().get(&memo) {
            return links;
        }
        if !visiting.insert(path.to_path_buf()) {
            return true;
        }
        let links = (|| {
            if path.extension().is_some_and(|e| e == "json") {
                return true;
            }
            let Some(text) = self.read_source(path) else {
                return false;
            };
            let imports = match transpile::static_imports(&text, path, SourceLang::from_path(path))
            {
                Ok(Some(imports)) => imports,
                Ok(None) => return true,
                Err(_) => return false,
            };
            let dir = path.parent().unwrap_or(Path::new("/"));
            imports
                .iter()
                .all(|spec| match self.resolve(ctx, spec, dir, Kind::Import) {
                    Ok(Resolved::File(dep)) => self.links_natively(ctx, &dep, generation, visiting),
                    Ok(_) => true,
                    Err(_) => false,
                })
        })();
        self.native_links.borrow_mut().insert(memo, links);
        links
    }

    /// The module name for `resolved` loaded as `load` from jiti generation `generation`.
    fn jiti_module_name(
        &self,
        ctx: &mut Context,
        resolved: &Resolved,
        load: JitiLoad,
        generation: u32,
    ) -> String {
        let native = match (load, resolved) {
            (_, Resolved::JitiVirtual(..) | Resolved::JitiCjs(..)) => false,
            (JitiLoad::Native, _) => true,
            (JitiLoad::Transpile, _) => false,
            (JitiLoad::TryNative, Resolved::File(path)) => {
                self.links_natively(ctx, path, generation, &mut Default::default())
            }
            (JitiLoad::TryNative, _) => true,
        };
        let at = if native {
            self.generation.get()
        } else {
            generation
        };
        self.module_name_at(resolved, at)
    }

    /// `require(spec)` from `parent` in jiti generation `generation` (jiti's synchronous
    /// `jitiRequire`), with the instance's `interopDefault`.
    fn jiti_require(
        self: &Rc<Self>,
        ctx: &mut Context,
        generation: u32,
        spec: &str,
        parent: Option<&Path>,
    ) -> JsResult<JsValue> {
        let jg = self
            .jiti_generation(generation)
            .ok_or_else(|| engine::type_error(format!("unknown jiti generation {generation}")))?;
        let base = referrer_dir(ctx, parent);
        let (resolved, load) = self
            .resolve_jiti(ctx, spec, &base, jg, Kind::Require)
            .map_err(|e| error::to_js(&e, ctx))?;
        let value = if load == JitiLoad::Native {
            self.require_resolved_at(ctx, &resolved, self.generation.get(), parent)?
        } else {
            let ns = self.require_resolved_at(ctx, &resolved, generation, parent)?;
            call_internal(ctx, "jitiExports", &[ns])?
        };
        self.jiti_interop(ctx, jg.scope, value)
    }

    /// jiti's `interopDefault` proxy, when the instance enables it.
    fn jiti_interop(&self, ctx: &mut Context, scope: usize, value: JsValue) -> JsResult<JsValue> {
        let interop = self
            .jiti_scopes
            .borrow()
            .get(scope)
            .is_some_and(|s| s.interop_default);
        if !interop {
            return Ok(value);
        }
        call_internal(ctx, "jitiInteropDefault", &[value])
    }

    /// The value of virtual module `spec` of jiti instance `scope`.
    fn jiti_virtual_value(&self, ctx: &mut Context, scope: usize, spec: &str) -> JsResult<JsValue> {
        let vm = self
            .jiti_scopes
            .borrow()
            .get(scope)
            .and_then(|s| s.virtual_modules.clone());
        match vm {
            Some(vm) => vm.get(spec, ctx),
            None => Err(engine::type_error(format!(
                "unknown jiti virtual module '{spec}'"
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------------

    /// Read and classify a file; `jiti` is the jiti generation it loads in, if any.
    fn prepare_file(
        &self,
        ctx: &mut Context,
        path: &Path,
        jiti: Option<u32>,
    ) -> Result<Prepared, Error> {
        // A native addon is a shared library, not source: `require_resolved` loads it.
        if crate::native::is_addon(path) {
            return Ok(Prepared {
                code: String::new(),
                format: Format::Native,
            });
        }
        let source = self.shared.file_sources.read().get(path).cloned();
        let text = match source {
            Some(text) => text,
            None => std::fs::read_to_string(path)
                .map_err(|e| Error::host(format!("cannot read {}: {e}", path.display())))?,
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let in_node_modules = path.components().any(|c| c.as_os_str() == "node_modules");
        match ext {
            "json" => {
                return Ok(Prepared {
                    code: text,
                    format: Format::Json,
                });
            }
            "cjs" => {
                return Ok(Prepared {
                    code: strip_shebang(text),
                    format: Format::Cjs,
                });
            }
            "ts" | "mts" | "cts" | "tsx" => {
                let out = transpile::transpile(&text, path, SourceLang::from_path(path))?;
                if let Some(map) = out.source_map {
                    state(ctx)
                        .source_maps
                        .borrow_mut()
                        .insert(path.display().to_string(), map);
                }
                let format = match ext {
                    "mts" => Format::Esm,
                    "cts" => Format::Cjs,
                    _ if out.has_module_syntax => Format::Esm,
                    _ => Format::Cjs,
                };
                let code = if format == Format::Esm && !in_node_modules {
                    self.cjs_globals(&out.code, &out.declared_cjs_names, jiti)
                } else {
                    out.code
                };
                return Ok(Prepared { code, format });
            }
            _ => {}
        }
        let text = strip_shebang(text);
        let pkg_type = self.package_type(path);
        if ext == "mjs" || pkg_type.as_deref() == Some("module") {
            if in_node_modules {
                return Ok(Prepared {
                    code: text,
                    format: Format::Esm,
                });
            }
            let out = transpile::transpile(&text, path, SourceLang::JavaScript)?;
            return Ok(Prepared {
                code: self.cjs_globals(&text, &out.declared_cjs_names, jiti),
                format: Format::Esm,
            });
        }
        if in_node_modules && !transpile::may_have_module_syntax(&text) {
            return Ok(Prepared {
                code: text,
                format: Format::Cjs,
            });
        }
        let out = transpile::transpile(&text, path, SourceLang::JavaScript)?;
        if out.has_module_syntax {
            let code = if in_node_modules {
                text
            } else {
                self.cjs_globals(&text, &out.declared_cjs_names, jiti)
            };
            Ok(Prepared {
                code,
                format: Format::Esm,
            })
        } else {
            Ok(Prepared {
                code: text,
                format: Format::Cjs,
            })
        }
    }

    /// Whether `path` belongs to a package named `undici`: its `fetch` is a re-implementation of
    /// the global, not an extension patch, so a global set to it must not divert pi's requests.
    fn in_undici_package(&self, path: &Path) -> bool {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if let Some(pkg) = self.package_json(d) {
                return pkg.get("name").and_then(|n| n.as_str()) == Some("undici");
            }
            dir = d.parent();
        }
        false
    }

    /// Mark undici's own `fetch` as the native implementation (`globalThis.fetch = undici.fetch`
    /// is not a patch). The module body runs first, so a `fetch` binding it exports or re-exports
    /// is in scope here.
    fn tag_undici(&self, path: &Path, prepared: &mut Prepared) {
        if prepared.format == Format::Json || !self.in_undici_package(path) {
            return;
        }
        prepared.code.push_str(
            "\n;try { if (typeof fetch === \"function\") \
             Object.defineProperty(fetch, \"__piNativeFetch\", { value: true, configurable: true }); } catch (e) {}\n",
        );
    }

    /// Create the QuickJS module named `name` (called by QuickJS's loader hook).
    fn load_module(
        self: &Rc<Self>,
        ctx: &mut Context,
        name: &str,
    ) -> Result<*mut qjs::JSModuleDef, Error> {
        tracing::trace!(target: "pi_jsrt", "load module {name}");
        let source = self.sources.borrow_mut().remove(name);
        if let Some(code) = source {
            return compile_module(ctx, &code, name);
        }
        let resolved =
            self.names
                .borrow()
                .get(name)
                .cloned()
                .ok_or_else(|| Error::ModuleNotFound {
                    specifier: name.to_string(),
                    referrer: "<loader>".into(),
                })?;
        let key = name.to_string();
        let module = match &resolved {
            Resolved::Builtin(builtin) => {
                let value = require_builtin(ctx, builtin).map_err(|e| error::from_js(e, ctx))?;
                self.synthetic_from_value(ctx, value, name, false)
                    .map_err(|e| error::from_js(e, ctx))?
            }
            Resolved::Virtual(spec) => {
                let vm = self.shared.virtual_modules.read().get(spec).cloned();
                let vm = vm.ok_or_else(|| Error::ModuleNotFound {
                    specifier: spec.clone(),
                    referrer: "<virtual>".into(),
                })?;
                match vm {
                    VirtualModule::Esm(src) => {
                        let out = transpile::transpile(
                            &src,
                            Path::new(&format!("{spec}.ts")),
                            SourceLang::TypeScript,
                        )?;
                        if let Some(map) = out.source_map {
                            state(ctx)
                                .source_maps
                                .borrow_mut()
                                .insert(name.to_string(), map);
                        }
                        compile_module(ctx, &out.code, name)?
                    }
                    VirtualModule::EsmJs(src) => compile_module(ctx, &src, name)?,
                    _ => {
                        let value = self
                            .require_resolved(ctx, &resolved, None)
                            .map_err(|e| error::from_js(e, ctx))?;
                        self.synthetic_from_value(ctx, value, name, true)
                            .map_err(|e| error::from_js(e, ctx))?
                    }
                }
            }
            Resolved::JitiVirtual(scope, spec) => {
                // jiti hands the value out through its interop proxy; named imports read it.
                let record = (|| {
                    let value = self.jiti_virtual_value(ctx, *scope, spec)?;
                    let interop = self
                        .jiti_scopes
                        .borrow()
                        .get(*scope)
                        .is_some_and(|s| s.interop_default);
                    call_internal(ctx, "jitiVirtualRecord", &[value, JsValue::from(interop)])
                })()
                .map_err(|e| error::from_js(e, ctx))?;
                self.synthetic_from_value(ctx, record, name, true)
                    .map_err(|e| error::from_js(e, ctx))?
            }
            Resolved::JitiCjs(scope, path) => {
                let record = (|| {
                    let file = Resolved::File(path.clone());
                    let exports =
                        self.require_resolved_at(ctx, &file, self.generation.get(), None)?;
                    let ns = call_internal(ctx, "jitiNodeNamespace", &[exports])?;
                    let interop = self
                        .jiti_scopes
                        .borrow()
                        .get(*scope)
                        .is_some_and(|s| s.interop_default);
                    call_internal(ctx, "jitiVirtualRecord", &[ns, JsValue::from(interop)])
                })()
                .map_err(|e| error::from_js(e, ctx))?;
                self.synthetic_from_value(ctx, record, name, true)
                    .map_err(|e| error::from_js(e, ctx))?
            }
            Resolved::File(path) => {
                let jiti = self.jiti_generation_of(name);
                let mut prepared = self.prepare_file(ctx, path, jiti)?;
                self.tag_undici(path, &mut prepared);
                match prepared.format {
                    Format::Esm => {
                        compile_module(ctx, &prepared.code, name).map_err(|e| with_file(e, path))?
                    }
                    Format::Native => {
                        let v = crate::native::require(ctx, path).map_err(|e| error::from_js(e, ctx))?;
                        self.synthetic_from_value(ctx, v, name, false).map_err(|e| error::from_js(e, ctx))?
                    }
                    Format::Json => {
                        let v = parse_json_text(ctx, &prepared.code, path)?;
                        self.synthetic_from_value(ctx, v, name, false)
                            .map_err(|e| error::from_js(e, ctx))?
                    }
                    Format::Cjs => {
                        let v = self
                            .run_cjs(ctx, &key, path, &prepared.code, jiti)
                            .map_err(|e| error::from_js(e, ctx))?;
                        self.synthetic_from_value(ctx, v, name, true)
                            .map_err(|e| error::from_js(e, ctx))?
                    }
                }
            }
        };
        Ok(module)
    }

    /// A module exposing `value` as `default` and its own enumerable properties as named
    /// exports. With `cjs_interop`, `__esModule` objects use their `default` property as the
    /// default export.
    fn synthetic_from_value(
        &self,
        ctx: &mut Context,
        value: JsValue,
        name: &str,
        cjs_interop: bool,
    ) -> JsResult<*mut qjs::JSModuleDef> {
        let mut names = vec!["default".to_string()];
        let mut default = value.clone();
        if let Some(obj) = value.as_object() {
            // Like Node's CJS export detection, element indices of arrays and typed arrays (a
            // module exporting a 50 KB Buffer) are not named exports.
            let indexed =
                obj.is_array() || crate::js::JsTypedArray::from_object(obj.clone()).is_ok();
            for k in object_keys(ctx, &obj)? {
                let k = k.to_std_string_lossy();
                if k == "default" || k.contains('\0') {
                    continue;
                }
                if indexed && k.bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                names.push(k);
            }
            if cjs_interop
                && obj.get(js_string!("__esModule"), ctx)?.to_boolean()
                && obj.has_property(js_string!("default"), ctx)?
            {
                default = obj.get(js_string!("default"), ctx)?;
            }
        }
        let cname = CString::new(name).map_err(|_| engine::type_error("invalid module name"))?;
        let raw = ctx.as_raw();
        let m = unsafe { qjs::JS_NewCModule(raw, cname.as_ptr(), Some(synthetic_init)) };
        if m.is_null() {
            return Err(crate::js::take_exception(&Context::from_raw(raw)));
        }
        for export in &names {
            let c = CString::new(export.as_str()).unwrap_or_default();
            if unsafe { qjs::JS_AddModuleExport(raw, m, c.as_ptr()) } < 0 {
                return Err(crate::js::take_exception(&Context::from_raw(raw)));
            }
        }
        self.synthetic.borrow_mut().insert(
            name.to_string(),
            Synthetic {
                value,
                default,
                names,
            },
        );
        Ok(m)
    }

    /// `require()` a resolved target from `parent` (a filename, for error messages).
    fn require_resolved(
        self: &Rc<Self>,
        ctx: &mut Context,
        resolved: &Resolved,
        parent: Option<&Path>,
    ) -> JsResult<JsValue> {
        self.require_resolved_at(ctx, resolved, self.generation.get(), parent)
    }

    /// [`Self::require_resolved`] in cache generation `generation`.
    fn require_resolved_at(
        self: &Rc<Self>,
        ctx: &mut Context,
        resolved: &Resolved,
        generation: u32,
        parent: Option<&Path>,
    ) -> JsResult<JsValue> {
        let key = resolved.key_at(generation);
        if let Some(m) = self.cjs.borrow().get(&key).cloned() {
            return m.get(js_string!("exports"), ctx);
        }
        if let Some(v) = self.values.borrow().get(&key).cloned() {
            return Ok(v);
        }
        match resolved {
            Resolved::Builtin(name) => require_builtin(ctx, name),
            Resolved::Virtual(spec) => {
                let vm = self.shared.virtual_modules.read().get(spec).cloned();
                let Some(vm) = vm else {
                    return Err(JsNativeError::error()
                        .with_message(format!("Cannot find module '{spec}'"))
                        .into());
                };
                match vm {
                    VirtualModule::Esm(_) | VirtualModule::EsmJs(_) => {
                        let name = self.module_name(resolved);
                        self.require_esm(ctx, &name, parent)
                    }
                    VirtualModule::CommonJs(src) => {
                        let path = PathBuf::from(format!("virtual:{spec}"));
                        self.run_cjs(ctx, &key, &path, &src, None)
                    }
                    VirtualModule::Json(v) => {
                        let v = JsValue::from_json(&v, ctx)?;
                        self.values.borrow_mut().insert(key, v.clone());
                        Ok(v)
                    }
                    VirtualModule::Expr(expr) => {
                        let path = PathBuf::from(format!("virtual:{spec}"));
                        let code = format!("({expr}\n)");
                        let v = ctx.eval(Source::from_bytes(code.as_bytes()).with_path(&path))?;
                        self.values.borrow_mut().insert(key, v.clone());
                        Ok(v)
                    }
                    VirtualModule::Object(handle) => {
                        let v = state(ctx).get_handle(handle.id()).ok_or_else(|| {
                            engine::type_error(format!(
                                "virtual module '{spec}' refers to a released handle"
                            ))
                        })?;
                        self.values.borrow_mut().insert(key, v.clone());
                        Ok(v)
                    }
                }
            }
            Resolved::JitiVirtual(scope, spec) => self.jiti_virtual_value(ctx, *scope, spec),
            Resolved::JitiCjs(_, path) => {
                let file = Resolved::File(path.clone());
                self.require_resolved_at(ctx, &file, self.generation.get(), parent)
            }
            Resolved::File(path) => {
                let jiti = self.jiti_generation(generation).map(|_| generation);
                let mut prepared = self
                    .prepare_file(ctx, path, jiti)
                    .map_err(|e| error::to_js(&e, ctx))?;
                self.tag_undici(path, &mut prepared);
                match prepared.format {
                    Format::Native => {
                        let v = crate::native::require(ctx, path)?;
                        self.values.borrow_mut().insert(key, v.clone());
                        Ok(v)
                    }
                    Format::Json => {
                        let v = parse_json_text(ctx, &prepared.code, path)
                            .map_err(|e| error::to_js(&e, ctx))?;
                        self.values.borrow_mut().insert(key, v.clone());
                        Ok(v)
                    }
                    Format::Cjs => self.run_cjs(ctx, &key, path, &prepared.code, jiti),
                    Format::Esm => {
                        let name = self.module_name_at(resolved, generation);
                        self.require_esm(ctx, &name, parent)
                    }
                }
            }
        }
    }

    /// Evaluate an ES module for `require()` and return its namespace, as Node's `require(esm)`
    /// does: synchronously and without running queued promise jobs (running them here would
    /// re-enter a module graph that is still being evaluated).
    fn require_esm(
        self: &Rc<Self>,
        ctx: &mut Context,
        name: &str,
        parent: Option<&Path>,
    ) -> JsResult<JsValue> {
        let c = CString::new(name).map_err(|_| engine::type_error("invalid module name"))?;
        let raw = ctx.as_raw();
        let mut status = ext::JS_REQUIRE_MODULE_ERROR;
        // SAFETY: `raw` is this thread's live context and both names are NUL-terminated
        // strings that outlive the call; `status` is a valid out-pointer.
        let v = unsafe { ext::JS_RequireModule(raw, c"".as_ptr(), c.as_ptr(), &mut status) };
        let parent = parent.map(|p| p.display().to_string());
        let path = module_path(name);
        let (code, message) = match status {
            ext::JS_REQUIRE_MODULE_ASYNC => {
                let mut message = "require() cannot be used on an ESM graph with top-level await. Use import() instead. \
                     To see where the top-level await comes from, use --experimental-print-required-tla."
                    .to_string();
                if let Some(parent) = &parent {
                    message.push_str(&format!("\n  From {parent} "));
                }
                message.push_str(&format!("\n  Requiring {path} "));
                ("ERR_REQUIRE_ASYNC_MODULE", message)
            }
            ext::JS_REQUIRE_MODULE_CYCLE => {
                let mut message = format!("Cannot require() ES Module {path} in a cycle.");
                if let Some(parent) = &parent {
                    message.push_str(&format!(" (from {parent})"));
                }
                message.push_str(
                    " A cycle involving require(esm) is not allowed to maintain invariants mandated by the ECMAScript \
                     specification. Try making at least part of the dependency in the graph lazily loaded.",
                );
                ("ERR_REQUIRE_CYCLE_MODULE", message)
            }
            _ => return crate::js::check_value(&Context::from_raw(raw), v),
        };
        let err = Error::Js(error::JsException {
            name: "Error".to_string(),
            message,
            stack: None,
            code: Some(code.to_string()),
        });
        Err(error::to_js(&err, ctx))
    }

    /// Run CommonJS `code`; in jiti generation `jiti`, its `require` resolves like jiti's.
    fn run_cjs(
        self: &Rc<Self>,
        ctx: &mut Context,
        key: &str,
        path: &Path,
        code: &str,
        jiti: Option<u32>,
    ) -> JsResult<JsValue> {
        let filename = path.display().to_string();
        let create = internal(ctx).get(js_string!("createCjsModule"), ctx)?;
        let create = create
            .as_function()
            .ok_or_else(|| engine::type_error("createCjsModule missing"))?;
        let generation = jiti.map_or_else(JsValue::undefined, JsValue::from);
        let module_obj = create
            .call(&JsValue::undefined(), &[js_str(&filename), generation], ctx)?
            .as_object()
            .ok_or_else(|| engine::type_error("createCjsModule returned a non-object"))?;
        self.cjs
            .borrow_mut()
            .insert(key.to_string(), module_obj.clone());
        let wrapped =
            format!("(function (exports, require, module, __filename, __dirname) {{{code}\n}})");
        let result = (|| {
            let f = ctx.eval(Source::from_bytes(wrapped.as_bytes()).with_path(path))?;
            let f = f
                .as_function()
                .ok_or_else(|| engine::type_error("CommonJS wrapper is not a function"))?;
            let exports = module_obj.get(js_string!("exports"), ctx)?;
            let require = module_obj.get(js_string!("require"), ctx)?;
            let dirname = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            f.call(
                &exports,
                &[
                    exports.clone(),
                    require,
                    module_obj.clone().into(),
                    js_str(&filename),
                    js_str(&dirname),
                ],
                ctx,
            )?;
            module_obj.set(js_string!("loaded"), true, false, ctx)?;
            module_obj.get(js_string!("exports"), ctx)
        })();
        if result.is_err() {
            self.cjs.borrow_mut().remove(key);
        }
        result
    }
}

const EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".cjs", ".json", ".node",
];

fn strip_shebang(text: String) -> String {
    match text.strip_prefix("#!") {
        Some(rest) => format!("//{rest}"),
        None => text,
    }
}

/// Prefix ES module code with jiti-compatible CommonJS bindings (on line 1, so
/// line numbers are unchanged).
/// In jiti generation `jiti`, `require` resolves like the jiti instance's.
fn inject_cjs_globals(code: &str, declared: &[&str], jiti: Option<u32>) -> String {
    // A hashbang must stay first; turn it into a comment so the prefix can go in front.
    let code = match code.strip_prefix("#!") {
        Some(rest) => std::borrow::Cow::Owned(format!("//{rest}")),
        None => std::borrow::Cow::Borrowed(code),
    };
    let code = code.as_ref();
    let mut prefix = String::new();
    if !declared.contains(&"__filename") {
        prefix.push_str("const __filename = import.meta.filename; ");
    }
    if !declared.contains(&"__dirname") {
        prefix.push_str("const __dirname = import.meta.dirname; ");
    }
    if !declared.contains(&"require") {
        let generation = jiti.map(|g| format!(", {g}")).unwrap_or_default();
        prefix.push_str(&format!(
            "const require = globalThis[Symbol.for(\"pi-jsrt.createRequire\")](import.meta.filename{generation}); "
        ));
    }
    if prefix.is_empty() {
        return code.to_string();
    }
    format!("{prefix}{code}")
}

fn split_package(spec: &str) -> Option<(&str, &str)> {
    if spec.is_empty() {
        return None;
    }
    let idx = if spec.starts_with('@') {
        let first = spec.find('/')?;
        spec[first + 1..].find('/').map(|i| first + 1 + i)
    } else {
        spec.find('/')
    };
    Some(match idx {
        Some(i) => (&spec[..i], &spec[i + 1..]),
        None => (spec, ""),
    })
}

/// Resolve a package.json `exports` entry for `subpath` ("." or "./x").
pub(crate) fn resolve_exports(
    exports: &Value,
    subpath: &str,
    conditions: &[&str],
) -> Option<String> {
    let is_subpath_map = exports.as_object().is_some_and(|m| {
        m.keys()
            .next()
            .is_some_and(|k| k.starts_with('.') || k.starts_with('#'))
    });
    if !is_subpath_map {
        return if subpath == "." {
            resolve_target(exports, None, conditions)
        } else {
            None
        };
    }
    let map = exports.as_object()?;
    if let Some(t) = map.get(subpath) {
        return resolve_target(t, None, conditions);
    }
    // Pattern keys: "./features/*.js"
    let mut best: Option<(&str, &Value, String)> = None;
    for (key, target) in map {
        let Some(star) = key.find('*') else { continue };
        let (prefix, suffix) = (&key[..star], &key[star + 1..]);
        if subpath.len() >= prefix.len() + suffix.len()
            && subpath.starts_with(prefix)
            && subpath.ends_with(suffix)
        {
            let matched = &subpath[prefix.len()..subpath.len() - suffix.len()];
            if best.as_ref().is_none_or(|(k, _, _)| key.len() > k.len()) {
                best = Some((key, target, matched.to_string()));
            }
        }
    }
    let (_, target, matched) = best?;
    resolve_target(target, Some(&matched), conditions)
}

fn resolve_target(target: &Value, matched: Option<&str>, conditions: &[&str]) -> Option<String> {
    match target {
        Value::String(s) => Some(match matched {
            Some(m) => s.replace('*', m),
            None => s.clone(),
        }),
        Value::Array(items) => items
            .iter()
            .find_map(|t| resolve_target(t, matched, conditions)),
        Value::Object(map) => {
            for (cond, t) in map {
                if (cond == "default" || conditions.contains(&cond.as_str()))
                    && let Some(r) = resolve_target(t, matched, conditions)
                {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

/// Lexically normalize a path (resolve `.` and `..`).
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Builtins that, as in Node, exist only under the `node:` scheme (`sqlite` is an npm package).
const PREFIX_ONLY_BUILTINS: &[&str] = &["sqlite"];

/// The builtin behind the bare specifier `jiti` (not a Node builtin: `node:jiti` does not exist).
const JITI_BUILTIN: &str = "jsrt:jiti";

fn builtin_name(ctx: &mut Context, spec: &str) -> Option<String> {
    if spec == "jiti" {
        return Some(JITI_BUILTIN.to_string());
    }
    let name = match spec.strip_prefix("node:") {
        Some(name) => name,
        None if PREFIX_ONLY_BUILTINS.contains(&spec) => return None,
        None => spec,
    };
    let has = internal(ctx)
        .get(js_string!("hasBuiltin"), ctx)
        .ok()?
        .as_function()?;
    let r = has.call(&JsValue::undefined(), &[js_str(name)], ctx).ok()?;
    if r.to_boolean() {
        Some(name.to_string())
    } else {
        None
    }
}

fn require_builtin(ctx: &mut Context, name: &str) -> JsResult<JsValue> {
    let f = internal(ctx).get(js_string!("requireBuiltin"), ctx)?;
    let f = f
        .as_function()
        .ok_or_else(|| engine::type_error("requireBuiltin missing"))?;
    f.call(&JsValue::undefined(), &[js_str(name)], ctx)
}

fn parse_json_text(ctx: &mut Context, text: &str, path: &Path) -> Result<JsValue, Error> {
    let v: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| Error::Syntax {
            file: path.display().to_string(),
            message: format!("{}: {e}", path.display()),
        })?;
    JsValue::from_json(&v, ctx).map_err(|e| error::from_js(e, ctx))
}

fn object_keys(ctx: &mut Context, obj: &JsObject) -> JsResult<Vec<JsString>> {
    let object_ctor = ctx.intrinsics().constructors().object().constructor();
    let keys = object_ctor.get(js_string!("keys"), ctx)?;
    let keys = keys
        .as_function()
        .ok_or_else(|| engine::type_error("Object.keys missing"))?;
    let arr = keys.call(&JsValue::undefined(), &[obj.clone().into()], ctx)?;
    let arr = JsArray::from_object(arr.as_object().ok_or_else(|| engine::type_error("keys"))?)?;
    let len = arr.length(ctx)?;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        out.push(arr.get(i, ctx)?.to_string(ctx)?);
    }
    Ok(out)
}

/// Name the file in parse errors.
fn with_file(err: Error, path: &Path) -> Error {
    match err {
        Error::Js(mut e)
            if e.name == "SyntaxError" && !e.message.contains(&*path.to_string_lossy()) =>
        {
            e.message = format!("{} ({})", e.message, path.display());
            Error::Js(e)
        }
        other => other,
    }
}

fn referrer_dir(ctx: &Context, path: Option<&Path>) -> PathBuf {
    match path {
        Some(p) if p.is_absolute() => p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/")),
        _ => state(ctx).cwd.borrow().clone(),
    }
}

/// The directory imports of module `name` resolve against.
fn base_dir_of(ctx: &Context, name: &str) -> PathBuf {
    let path = module_path(name);
    let path = path.strip_prefix("file://").unwrap_or(path);
    referrer_dir(ctx, (!path.is_empty()).then_some(Path::new(path)))
}

fn loader(ctx: &Context) -> Rc<Loader> {
    ctx.get_data::<Rc<Loader>>()
        .cloned()
        .expect("pi-jsrt loader not installed")
}

/// Install the loader in the engine (QuickJS module hooks).
pub(crate) fn install(engine: &Engine, loader: Rc<Loader>) {
    engine.context().insert_data(loader);
    unsafe {
        qjs::JS_SetModuleLoaderFunc2(
            engine.raw_runtime(),
            Some(normalize_hook),
            Some(loader_hook),
            Some(check_attributes_hook),
            std::ptr::null_mut(),
        );
    }
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// QuickJS `module_normalize`: resolve `name` imported by module `base` to a module name.
unsafe extern "C" fn normalize_hook(
    raw: *mut qjs::JSContext,
    base: *const c_char,
    name: *const c_char,
    _opaque: *mut c_void,
) -> *mut c_char {
    let base = cstr(base);
    let spec = cstr(name);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ctx = Context::from_raw(raw);
        let l = loader(&ctx);
        if l.names.borrow().contains_key(&spec) || l.sources.borrow().contains_key(&spec) {
            return Ok(spec.clone());
        }
        let dir = base_dir_of(&ctx, &base);
        let generation = l.generation_of(&base);
        if let Some(jg) = l.jiti_generation(generation) {
            let (resolved, load) = l
                .resolve_jiti(&mut ctx, &spec, &dir, jg, jg.kind)
                .map_err(|e| error::to_js(&e, &mut ctx))?;
            return Ok(l.jiti_module_name(&mut ctx, &resolved, load, generation));
        }
        let resolved = l
            .resolve(&mut ctx, &spec, &dir, Kind::Import)
            .map_err(|e| error::to_js(&e, &mut ctx))?;
        Ok::<_, JsError>(l.module_name_at(&resolved, generation))
    }));
    match r {
        Ok(Ok(resolved)) => {
            let c = CString::new(resolved).unwrap_or_default();
            unsafe { qjs::js_strdup(raw, c.as_ptr()) }
        }
        Ok(Err(e)) => {
            crate::js::throw_error(&Context::from_raw(raw), e);
            std::ptr::null_mut()
        }
        Err(_) => {
            crate::js::throw_error(
                &Context::from_raw(raw),
                engine::type_error(format!("panic while resolving '{spec}'")),
            );
            std::ptr::null_mut()
        }
    }
}

/// QuickJS `module_loader`: create the module named `name`.
unsafe extern "C" fn loader_hook(
    raw: *mut qjs::JSContext,
    name: *const c_char,
    _opaque: *mut c_void,
    _attributes: qjs::JSValue,
) -> *mut qjs::JSModuleDef {
    let name = cstr(name);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ctx = Context::from_raw(raw);
        let l = loader(&ctx);
        l.loaded.borrow_mut().insert(name.clone());
        l.load_module(&mut ctx, &name)
            .map_err(|e| error::to_js(&e, &mut ctx))
    }));
    match r {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            crate::js::throw_error(&Context::from_raw(raw), e);
            std::ptr::null_mut()
        }
        Err(_) => {
            crate::js::throw_error(
                &Context::from_raw(raw),
                engine::type_error(format!("panic while loading '{name}'")),
            );
            std::ptr::null_mut()
        }
    }
}

/// Import attributes are accepted as is (JSON modules are recognized by extension).
unsafe extern "C" fn check_attributes_hook(
    _ctx: *mut qjs::JSContext,
    _opaque: *mut c_void,
    _attributes: qjs::JSValue,
) -> c_int {
    0
}

/// Initialize a module made by [`Loader::synthetic_from_value`]: set its exports.
unsafe extern "C" fn synthetic_init(raw: *mut qjs::JSContext, m: *mut qjs::JSModuleDef) -> c_int {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> JsResult<()> {
        let mut ctx = Context::from_raw(raw);
        let atom = unsafe { qjs::JS_GetModuleName(raw, m) };
        let name_val = unsafe { JsValue::from_raw(qjs::JS_AtomToValue(raw, atom)) };
        unsafe { qjs::JS_FreeAtom(raw, atom) };
        let name = name_val.to_string(&mut ctx)?.to_std_string_lossy();
        let syn = loader(&ctx).synthetic.borrow_mut().remove(&name);
        let Some(syn) = syn else { return Ok(()) };
        let obj = syn.value.as_object();
        for export in &syn.names {
            let v = if export == "default" {
                syn.default.clone()
            } else if let Some(obj) = &obj {
                obj.get(export.as_str(), &mut ctx)?
            } else {
                JsValue::undefined()
            };
            let c = CString::new(export.as_str()).unwrap_or_default();
            if unsafe { qjs::JS_SetModuleExport(raw, m, c.as_ptr(), v.into_raw()) } < 0 {
                return Err(crate::js::take_exception(&Context::from_raw(raw)));
            }
        }
        Ok(())
    }));
    match r {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            crate::js::throw_error(&Context::from_raw(raw), e);
            -1
        }
        Err(_) => -1,
    }
}

/// Compile ES module source named `name` and fill its `import.meta`.
fn compile_module(
    ctx: &mut Context,
    code: &str,
    name: &str,
) -> Result<*mut qjs::JSModuleDef, Error> {
    let flags = (qjs::JS_EVAL_TYPE_MODULE | qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32;
    let value = ctx
        .eval_with_flags(code.as_bytes(), name, flags)
        .map_err(|e| error::from_js(e, ctx))?;
    let m = unsafe { qjs::JS_VALUE_GET_PTR(value.as_raw()) } as *mut qjs::JSModuleDef;
    let meta = unsafe { JsValue::from_raw(qjs::JS_GetImportMeta(ctx.as_raw(), m)) };
    if let Some(meta) = meta.as_object() {
        init_import_meta(ctx, &meta, name);
    }
    drop(value);
    Ok(m)
}

fn init_import_meta(ctx: &mut Context, import_meta: &JsObject, name: &str) {
    let path = Path::new(module_path(name));
    let path_str = path.display().to_string();
    let url = if path.is_absolute() {
        url::Url::from_file_path(path)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| path_str.clone())
    } else {
        path_str.clone()
    };
    let _ = import_meta.set(js_string!("url"), js_str(&url), false, ctx);
    if path.is_absolute() {
        let dir = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let _ = import_meta.set(js_string!("filename"), js_str(&path_str), false, ctx);
        let _ = import_meta.set(js_string!("dirname"), js_str(&dir), false, ctx);
    }
    if let Ok(f) = internal(ctx).get(js_string!("makeImportMetaResolve"), ctx)
        && let Some(f) = f.as_function()
        && let Ok(resolve) = f.call(&JsValue::undefined(), &[js_str(&path_str)], ctx)
    {
        let _ = import_meta.set(js_string!("resolve"), resolve, false, ctx);
    }
}

/// Load, link and evaluate the module named `name` (like `import()`): a promise of its namespace.
fn load_by_name(ctx: &mut Context, name: &str) -> JsResult<JsPromise> {
    let c = CString::new(name).map_err(|_| engine::type_error("invalid module name"))?;
    let raw = ctx.as_raw();
    let v = crate::js::check_value(&Context::from_raw(raw), unsafe {
        qjs::JS_LoadModule(raw, c"".as_ptr(), c.as_ptr())
    })?;
    v.as_promise()
        .ok_or_else(|| engine::type_error("JS_LoadModule did not return a promise"))
}

// ---------------------------------------------------------------------------
// Entry points used by the runtime API and ops
// ---------------------------------------------------------------------------

/// Evaluate a classic script.
pub(crate) fn eval_script(ctx: &mut Context, code: &str, filename: &str) -> JsResult<JsValue> {
    let cwd = state(ctx).cwd.borrow().clone();
    let path = if filename.starts_with('<') || filename.starts_with('[') {
        PathBuf::from(filename)
    } else {
        cwd.join(filename)
    };
    let code =
        if filename.ends_with(".ts") || filename.ends_with(".mts") || filename.ends_with(".cts") {
            let out = transpile::transpile(code, &path, SourceLang::TypeScript)
                .map_err(|e| error::to_js(&e, ctx))?;
            if let Some(map) = out.source_map {
                state(ctx)
                    .source_maps
                    .borrow_mut()
                    .insert(path.display().to_string(), map);
            }
            out.code
        } else {
            code.to_string()
        };
    ctx.eval(Source::from_bytes(code.as_bytes()).with_path(&path))
}

/// An ES module from source text (evaluated by [`evaluate_module`]).
pub(crate) fn module_from_source(
    ctx: &mut Context,
    source: &str,
    filename: &str,
) -> Result<Module, Error> {
    let cwd = state(ctx).cwd.borrow().clone();
    let path = normalize(&cwd.join(filename));
    let lang = SourceLang::from_path(&path);
    let out = transpile::transpile(source, &path, lang)?;
    if let Some(map) = out.source_map {
        state(ctx)
            .source_maps
            .borrow_mut()
            .insert(path.display().to_string(), map);
    }
    let l = loader(ctx);
    let code = l.cjs_globals(&out.code, &out.declared_cjs_names, None);
    // A name QuickJS already loaded would return that module: use a fresh one.
    let mut name = path.display().to_string();
    if l.loaded.borrow().contains(&name) || l.sources.borrow().contains_key(&name) {
        l.evals.set(l.evals.get() + 1);
        name = format!("{name}{EVAL_MARK}{}", l.evals.get());
    }
    l.sources.borrow_mut().insert(name.clone(), code);
    Ok(Module(name))
}

/// Resolve the module for `specifier` (relative to `referrer` or the cwd).
pub(crate) fn module_for_specifier(
    ctx: &mut Context,
    specifier: &str,
    referrer: Option<&Path>,
) -> Result<Module, Error> {
    let l = loader(ctx);
    let base = referrer_dir(ctx, referrer);
    let resolved = l.resolve(ctx, specifier, &base, Kind::Import)?;
    Ok(Module(l.module_name(&resolved)))
}

/// Load, link and evaluate `module`; `done` receives the namespace or the error.
pub(crate) fn evaluate_module(
    ctx: &mut Context,
    module: Module,
    done: impl FnOnce(&mut Context, Result<JsValue, JsValue>) + 'static,
) {
    let promise = load_by_name(ctx, &module.0).map(JsValue::from);
    engine::when_settled(ctx, promise, done);
}

/// `require(specifier)` from `parent` (a filename) or the cwd.
pub(crate) fn require(
    ctx: &mut Context,
    specifier: &str,
    parent: Option<&Path>,
) -> JsResult<JsValue> {
    let l = loader(ctx);
    let base = referrer_dir(ctx, parent);
    let resolved = l
        .resolve(ctx, specifier, &base, Kind::Require)
        .map_err(|e| error::to_js(&e, ctx))?;
    l.require_resolved(ctx, &resolved, parent)
}

pub(crate) fn clear_file_cache(ctx: &mut Context) {
    loader(ctx).clear_files();
}

/// `ops.require(specifier, parent, jitiGeneration?)`.
fn op_require(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let spec = arg_string(args, 0, ctx)?;
    let parent = arg_opt_string(args, 1, ctx)?;
    let parent = parent.as_deref().map(Path::new);
    match arg_generation(args, 2) {
        Some(generation) => loader(ctx).jiti_require(ctx, generation, &spec, parent),
        None => require(ctx, &spec, parent),
    }
}

/// An optional generation number argument.
fn arg_generation(args: &[JsValue], i: usize) -> Option<u32> {
    let n = args.get_or_undefined(i).as_number()?;
    (n >= 0.0 && n <= f64::from(u32::MAX) && n.fract() == 0.0).then_some(n as u32)
}

/// Call `internal[name](...args)`.
fn call_internal(ctx: &mut Context, name: &str, args: &[JsValue]) -> JsResult<JsValue> {
    let f = internal(ctx).get(name, ctx)?;
    let f = f
        .as_function()
        .ok_or_else(|| engine::type_error(format!("internal.{name} missing")))?;
    f.call(&JsValue::undefined(), args, ctx)
}

/// jiti's `resolveAlias` for instance `scope` (pathe's alias rules live in `js/jiti.js`).
fn jiti_alias(ctx: &mut Context, scope: usize, spec: &str) -> JsResult<String> {
    let scope = u32::try_from(scope).map_err(|_| engine::type_error("jiti instance index"))?;
    let v = call_internal(ctx, "jitiAlias", &[JsValue::from(scope), js_str(spec)])?;
    Ok(v.to_string(ctx)?.to_std_string_lossy())
}

/// Whether `path` lies in one of the `node_modules` packages `names` (jiti's `isNativeRe` /
/// `isTransformRe`).
fn in_packages(path: &Path, names: &[String]) -> bool {
    let path = path.to_string_lossy();
    names
        .iter()
        .any(|name| path.contains(&format!("node_modules/{name}/")))
}

/// `ops.jitiCreate(virtualModules, config)`: register a `createJiti` instance; returns its index.
/// `config` is JSON: `{ hasAlias, interopDefault, nativeModules, transformModules }`.
fn op_jiti_create(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let virtual_modules = args.get_or_undefined(0).as_object();
    let config = arg_string(args, 1, ctx)?;
    let config: Value = serde_json::from_str(&config)
        .map_err(|e| engine::type_error(format!("invalid jiti config: {e}")))?;
    let flag = |k: &str| config.get(k).and_then(Value::as_bool).unwrap_or(false);
    let names = |k: &str| -> Vec<String> {
        config
            .get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let scope = JitiScope {
        virtual_modules,
        has_alias: flag("hasAlias"),
        interop_default: flag("interopDefault"),
        native_modules: names("nativeModules"),
        transform_modules: names("transformModules"),
        cached: [None, None],
    };
    let l = loader(ctx);
    let mut scopes = l.jiti_scopes.borrow_mut();
    scopes.push(scope);
    let index = u32::try_from(scopes.len() - 1)
        .map_err(|_| engine::type_error("too many jiti instances"))?;
    Ok(JsValue::from(index))
}

/// `ops.jitiGeneration(scope, isImport, moduleCache)`: the generation one `jiti.import` (or
/// `jiti()`) loads in: a new one per call, or the instance's own while `moduleCache` is on.
fn op_jiti_generation(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let scope = args.get_or_undefined(0).to_u32(ctx)? as usize;
    let kind = if args.get_or_undefined(1).to_boolean() {
        Kind::Import
    } else {
        Kind::Require
    };
    let reuse = args.get_or_undefined(2).to_boolean();
    let l = loader(ctx);
    let slot = usize::from(kind == Kind::Import);
    let cached = l
        .jiti_scopes
        .borrow()
        .get(scope)
        .ok_or_else(|| engine::type_error(format!("unknown jiti instance {scope}")))?
        .cached[slot];
    if reuse && let Some(generation) = cached {
        return Ok(JsValue::from(generation));
    }
    let generation = l.new_generation();
    l.jiti_generations
        .borrow_mut()
        .insert(generation, JitiGeneration { scope, kind });
    if reuse && let Some(s) = l.jiti_scopes.borrow_mut().get_mut(scope) {
        s.cached[slot] = Some(generation);
    }
    Ok(JsValue::from(generation))
}

/// `ops.jitiRequire(generation, specifier, parent)`: jiti's synchronous load.
fn op_jiti_require(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let generation =
        arg_generation(args, 0).ok_or_else(|| engine::type_error("jiti generation"))?;
    let spec = arg_string(args, 1, ctx)?;
    let parent = arg_opt_string(args, 2, ctx)?;
    loader(ctx).jiti_require(ctx, generation, &spec, parent.as_deref().map(Path::new))
}

/// `ops.jitiTarget(generation, specifier, parent)`: what `jiti.import` loads: `{ virtual: true }`
/// for a virtual module, `{ cjs: path }` for a CommonJS file (imported natively), else
/// `{ name, transpiled?, fallback? }`, module names for `import()`;
/// `fallback` is the transpiled load to use when the native import of `name` fails.
fn op_jiti_target(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let generation =
        arg_generation(args, 0).ok_or_else(|| engine::type_error("jiti generation"))?;
    let spec = arg_string(args, 1, ctx)?;
    let parent = arg_opt_string(args, 2, ctx)?;
    let l = loader(ctx);
    let jg = l
        .jiti_generation(generation)
        .ok_or_else(|| engine::type_error(format!("unknown jiti generation {generation}")))?;
    let base = referrer_dir(ctx, parent.as_deref().map(Path::new));
    let (resolved, load) = l
        .resolve_jiti(ctx, &spec, &base, jg, Kind::Import)
        .map_err(|e| error::to_js(&e, ctx))?;
    let out = JsObject::with_null_proto();
    match &resolved {
        Resolved::JitiVirtual(..) => {
            out.set("virtual", true, true, ctx)?;
            return Ok(out.into());
        }
        Resolved::JitiCjs(_, path) => {
            out.set("cjs", js_str(&path.display().to_string()), true, ctx)?;
            return Ok(out.into());
        }
        _ => {}
    }
    let current = l.generation.get();
    let (name, fallback) = match load {
        JitiLoad::Native => (l.module_name_at(&resolved, current), None),
        JitiLoad::Transpile => {
            out.set("transpiled", true, true, ctx)?;
            (l.module_name_at(&resolved, generation), None)
        }
        JitiLoad::TryNative => (
            l.module_name_at(&resolved, current),
            Some(l.module_name_at(&resolved, generation)),
        ),
    };
    out.set("name", js_str(&name), true, ctx)?;
    if let Some(fallback) = fallback {
        out.set("fallback", js_str(&fallback), true, ctx)?;
    }
    Ok(out.into())
}

/// `ops.jitiResolve(scope, specifier, parent, isImport)`: jiti's `jitiResolve` (aliases, then
/// the regular resolution; virtual modules are not consulted, as in jiti).
fn op_jiti_resolve(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let scope = args.get_or_undefined(0).to_u32(ctx)? as usize;
    let spec = arg_string(args, 1, ctx)?;
    let parent = arg_opt_string(args, 2, ctx)?;
    let kind = if args.get_or_undefined(3).to_boolean() {
        Kind::Import
    } else {
        Kind::Require
    };
    let l = loader(ctx);
    let has_alias = l
        .jiti_scopes
        .borrow()
        .get(scope)
        .is_some_and(|s| s.has_alias);
    let spec = if has_alias {
        jiti_alias(ctx, scope, &spec)?
    } else {
        spec
    };
    let base = referrer_dir(ctx, parent.as_deref().map(Path::new));
    let resolved = l
        .resolve_either(ctx, &spec, &base, kind)
        .map_err(|e| error::to_js(&e, ctx))?;
    Ok(js_str(&resolved_specifier(&resolved)))
}

/// `ops.jitiEval(generation, source, filename)`: jiti's `evalModule`. ES module source returns
/// `{ name }` (load it with `import()` or `ops.jitiRequireModule`), CommonJS `{ exports }`.
fn op_jiti_eval(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let generation =
        arg_generation(args, 0).ok_or_else(|| engine::type_error("jiti generation"))?;
    let source = arg_string(args, 1, ctx)?;
    let filename = arg_string(args, 2, ctx)?;
    let l = loader(ctx);
    if l.jiti_generation(generation).is_none() {
        return Err(engine::type_error(format!(
            "unknown jiti generation {generation}"
        )));
    }
    let path = normalize(&state(ctx).cwd.borrow().join(&filename));
    let out = transpile::transpile(&source, &path, SourceLang::from_path(&path))
        .map_err(|e| error::to_js(&e, ctx))?;
    if let Some(map) = out.source_map {
        state(ctx)
            .source_maps
            .borrow_mut()
            .insert(path.display().to_string(), map);
    }
    l.evals.set(l.evals.get() + 1);
    let name = format!(
        "{}{EVAL_MARK}{}{RELOAD_MARK}{generation}",
        path.display(),
        l.evals.get()
    );
    let esm = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "mjs" | "mts" => true,
        "cjs" | "cts" => false,
        _ => out.has_module_syntax,
    };
    let result = JsObject::with_null_proto();
    if esm {
        let code = l.cjs_globals(&out.code, &out.declared_cjs_names, Some(generation));
        l.sources.borrow_mut().insert(name.clone(), code);
        result.set("name", js_str(&name), true, ctx)?;
    } else {
        let exports = l.run_cjs(ctx, &name, &path, &out.code, Some(generation))?;
        result.set("exports", exports, true, ctx)?;
    }
    Ok(result.into())
}

/// `ops.jitiRequireModule(name)`: evaluate the ES module named `name` synchronously (`require`).
fn op_jiti_require_module(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = arg_string(args, 0, ctx)?;
    loader(ctx).require_esm(ctx, &name, None)
}

fn op_require_resolve(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let spec = arg_string(args, 0, ctx)?;
    let parent = arg_opt_string(args, 1, ctx)?;
    let kind = if args.get_or_undefined(2).to_boolean() {
        Kind::Import
    } else {
        Kind::Require
    };
    let l = loader(ctx);
    let base = referrer_dir(ctx, parent.as_deref().map(Path::new));
    let resolved = l
        .resolve(ctx, &spec, &base, kind)
        .map_err(|e| error::to_js(&e, ctx))?;
    Ok(js_str(&resolved_specifier(&resolved)))
}

/// What `require.resolve` reports for a resolved module.
fn resolved_specifier(resolved: &Resolved) -> String {
    match resolved {
        Resolved::Builtin(n) if n == JITI_BUILTIN => "jiti".to_string(),
        Resolved::Builtin(n) if n.starts_with("jsrt:") => n.clone(),
        Resolved::Builtin(n) => format!("node:{n}"),
        Resolved::Virtual(s) | Resolved::JitiVirtual(_, s) => s.clone(),
        Resolved::File(p) | Resolved::JitiCjs(_, p) => p.display().to_string(),
    }
}

fn op_set_cwd(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let dir = arg_string(args, 0, ctx)?;
    let st = state(ctx);
    let new = normalize(&st.cwd.borrow().join(&dir));
    if !new.is_dir() {
        let err = Error::Js(crate::error::JsException {
            name: "Error".into(),
            message: format!("ENOENT: no such file or directory, chdir '{dir}'"),
            stack: None,
            code: Some("ENOENT".into()),
        });
        return Err(error::to_js(&err, ctx));
    }
    *st.cwd.borrow_mut() = new;
    Ok(JsValue::undefined())
}

fn op_get_cwd(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    Ok(js_str(&state(ctx).cwd.borrow().display().to_string()))
}

fn op_transpile(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let code = arg_string(args, 0, ctx)?;
    let name = arg_opt_string(args, 1, ctx)?.unwrap_or_else(|| "input.ts".into());
    let out = transpile::transpile(
        &code,
        Path::new(&name),
        SourceLang::from_path(Path::new(&name)),
    )
    .map_err(|e| error::to_js(&e, ctx))?;
    Ok(js_str(&out.code))
}

/// `ops.isCommonJs(path) -> bool`: whether a file loads as CommonJS (Node mode runs a CommonJS
/// main module synchronously, like Node).
fn op_is_common_js(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let path = arg_string(args, 0, ctx)?;
    let l = loader(ctx);
    let Some(resolved) = l.resolve_path(Path::new(&path)) else {
        return Ok(JsValue::from(false));
    };
    let cjs = l
        .prepare_file(ctx, &resolved, None)
        .is_ok_and(|p| p.format == Format::Cjs);
    Ok(JsValue::from(cjs))
}

/// `ops.evalScript(code, filename)`: evaluate a classic script (`node -e`, `vm.runInThisContext`).
fn op_eval_script(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let code = arg_string(args, 0, ctx)?;
    let name = arg_opt_string(args, 1, ctx)?.unwrap_or_else(|| "<eval>".into());
    eval_script(ctx, &code, &name)
}

/// `ops.dlopen(filename)`: load a native addon and return its exports (`process.dlopen`).
fn op_dlopen(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let filename = arg_string(args, 0, ctx)?;
    crate::native::require(ctx, Path::new(&filename))
}

/// Keep a loaded addon alive with its loader (its cleanup hooks run at teardown).
pub(crate) fn add_native(ctx: &Context, addon: crate::native::Loaded) {
    loader(ctx).native.borrow_mut().push(addon);
}

pub(crate) const LOADER_OPS: &[(&str, usize, engine::OpFn)] = &[
    ("evalScript", 2, op_eval_script),
    ("dlopen", 1, op_dlopen),
    ("isCommonJs", 1, op_is_common_js),
    ("require", 2, op_require),
    ("requireResolve", 3, op_require_resolve),
    ("chdir", 1, op_set_cwd),
    ("cwd", 0, op_get_cwd),
    ("transpile", 2, op_transpile),
    ("jitiCreate", 2, op_jiti_create),
    ("jitiGeneration", 3, op_jiti_generation),
    ("jitiRequire", 3, op_jiti_require),
    ("jitiTarget", 3, op_jiti_target),
    ("jitiResolve", 4, op_jiti_resolve),
    ("jitiEval", 3, op_jiti_eval),
    ("jitiRequireModule", 1, op_jiti_require_module),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exports_resolution() {
        let exports = json!({
            ".": { "types": "./d.ts", "import": "./esm/index.js", "require": "./cjs/index.js" },
            "./compile": { "import": "./esm/compile.js", "default": "./cjs/compile.js" },
            "./features/*": "./src/features/*.js",
            "./package.json": "./package.json"
        });
        let imp = &["import", "node", "default"];
        let req = &["require", "node", "default"];
        assert_eq!(
            resolve_exports(&exports, ".", imp).as_deref(),
            Some("./esm/index.js")
        );
        assert_eq!(
            resolve_exports(&exports, ".", req).as_deref(),
            Some("./cjs/index.js")
        );
        assert_eq!(
            resolve_exports(&exports, "./compile", req).as_deref(),
            Some("./cjs/compile.js")
        );
        assert_eq!(
            resolve_exports(&exports, "./features/a/b", imp).as_deref(),
            Some("./src/features/a/b.js")
        );
        assert_eq!(
            resolve_exports(&json!("./main.js"), ".", imp).as_deref(),
            Some("./main.js")
        );
        assert_eq!(
            resolve_exports(
                &json!({"import": "./m.mjs", "default": "./m.cjs"}),
                ".",
                req
            )
            .as_deref(),
            Some("./m.cjs")
        );
        assert_eq!(resolve_exports(&exports, "./missing", imp), None);
    }

    #[test]
    fn package_split() {
        assert_eq!(split_package("ms"), Some(("ms", "")));
        assert_eq!(split_package("typebox/value"), Some(("typebox", "value")));
        assert_eq!(split_package("@scope/pkg"), Some(("@scope/pkg", "")));
        assert_eq!(split_package("@scope/pkg/a/b"), Some(("@scope/pkg", "a/b")));
    }

    #[test]
    fn injection_skips_declared() {
        let out = inject_cjs_globals("x", &["require"], None);
        assert!(out.contains("__filename"));
        assert!(!out.contains("const require"));
    }
}
