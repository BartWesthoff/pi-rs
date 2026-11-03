//! TypeScript / JavaScript source preparation with oxc.
//!
//! TypeScript is stripped with `oxc_transformer` (type annotations, interfaces,
//! type-only imports, enums, namespaces and parameter properties are all
//! handled by oxc). JavaScript sources are parsed only to detect whether they
//! use ES module syntax and which top-level bindings they declare; their text
//! is passed through unchanged so that stack traces keep exact positions.
//!
//! For transpiled TypeScript a compact line/column map is kept so that stack
//! traces can be mapped back to the original file (see [`SourceMapTable`]).

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

use crate::error::Error;

/// Source language of a file handed to [`transpile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLang {
    /// `.ts`, `.mts`, `.cts`
    TypeScript,
    /// `.tsx` (JSX is parsed but not transformed specially; React runtime is assumed)
    Tsx,
    /// `.js`, `.mjs`, `.cjs`
    JavaScript,
}

impl SourceLang {
    /// Guess the language from a path extension.
    pub fn from_path(path: &Path) -> SourceLang {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "ts" | "mts" | "cts" => SourceLang::TypeScript,
            "tsx" => SourceLang::Tsx,
            _ => SourceLang::JavaScript,
        }
    }
}

/// Result of [`transpile`].
#[derive(Debug, Clone)]
pub struct TranspileOutput {
    /// JavaScript code.
    pub code: String,
    /// Whether the source contains `import`/`export` statements (or `import.meta`/top-level await).
    pub has_module_syntax: bool,
    /// Which of the CommonJS-style names (`require`, `module`, `exports`,
    /// `__filename`, `__dirname`) are declared at the top level of the module.
    pub declared_cjs_names: Vec<&'static str>,
    /// Position map from generated code back to the source (only for transpiled TypeScript).
    pub source_map: Option<SourceMapTable>,
}

/// Names that the loader may inject into ES modules (jiti compatibility).
pub const CJS_NAMES: [&str; 5] = ["require", "module", "exports", "__filename", "__dirname"];

/// A compact generated→original position table.
#[derive(Debug, Clone, Default)]
pub struct SourceMapTable {
    /// Sorted by (dst_line, dst_col). All values are 0-based.
    tokens: Vec<(u32, u32, u32, u32)>,
}

impl SourceMapTable {
    /// Map a 1-based generated (line, column) to a 1-based original (line, column).
    pub fn lookup(&self, line: u32, col: u32) -> Option<(u32, u32)> {
        if line == 0 {
            return None;
        }
        let key = (line - 1, col.saturating_sub(1));
        let idx = match self.tokens.binary_search_by(|t| (t.0, t.1).cmp(&key)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let t = self.tokens[idx];
        if t.0 != key.0 {
            // No token on that generated line before the column; use the first token of the line.
            let first = self.tokens[idx + 1..].iter().find(|t| t.0 == key.0)?;
            return Some((first.2 + 1, first.3 + 1));
        }
        Some((t.2 + 1, t.3 + 1))
    }
}

fn source_type_for(lang: SourceLang) -> SourceType {
    match lang {
        SourceLang::TypeScript => SourceType::ts(),
        SourceLang::Tsx => SourceType::tsx(),
        SourceLang::JavaScript => SourceType::unambiguous(),
    }
}

fn diagnostic_error(path: &Path, source: &str, diags: &[oxc_diagnostics::OxcDiagnostic]) -> Error {
    let mut msgs = Vec::new();
    for d in diags.iter().take(5) {
        let loc = d
            .labels
            .first()
            .map(|l| {
                let (line, col) = line_col(source, l.offset() as usize);
                format!(" ({}:{line}:{col})", path.display())
            })
            .unwrap_or_default();
        msgs.push(format!("{}{loc}", d.message));
    }
    Error::Syntax {
        file: path.display().to_string(),
        message: msgs.join("\n"),
    }
}

fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(offset, |p| offset - p - 1) + 1;
    (line, col)
}

/// Parse (and for TypeScript, transform) `source`.
///
/// `path` is only used for diagnostics and transformer configuration.
pub fn transpile(source: &str, path: &Path, lang: SourceLang) -> Result<TranspileOutput, Error> {
    let allocator = Allocator::default();
    let source_type = source_type_for(lang);
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            allow_return_outside_function: lang == SourceLang::JavaScript,
            ..ParseOptions::default()
        })
        .parse();
    if ret.fatal_error || ret.diagnostics.has_errors() {
        let errors: Vec<_> = ret.diagnostics.errors().cloned().collect();
        return Err(diagnostic_error(path, source, &errors));
    }
    let has_module_syntax =
        ret.module_record.has_module_syntax || !ret.module_record.import_metas.is_empty();
    let mut program = ret.program;
    let semantic = SemanticBuilder::new().with_enum_eval(true).build(&program);
    let scoping = semantic.semantic.into_scoping();
    let declared_cjs_names = CJS_NAMES
        .iter()
        .copied()
        .filter(|n| scoping.get_root_binding((*n).into()).is_some())
        .collect();

    if lang == SourceLang::JavaScript {
        return Ok(TranspileOutput {
            code: source.to_string(),
            has_module_syntax,
            declared_cjs_names,
            source_map: None,
        });
    }

    let options = TransformOptions::default();
    let tret = Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if tret.diagnostics.has_errors() {
        let errors: Vec<_> = tret.diagnostics.errors().cloned().collect();
        return Err(diagnostic_error(path, source, &errors));
    }
    let cg = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: Some(path.to_path_buf()),
            ..CodegenOptions::default()
        })
        .build(&program);
    let source_map = cg.map.map(|map| {
        let mut tokens: Vec<_> = map
            .get_tokens()
            .map(|t| (t.get_dst_line(), t.get_dst_col(), t.get_src_line(), t.get_src_col()))
            .collect();
        tokens.sort_unstable();
        SourceMapTable { tokens }
    });
    Ok(TranspileOutput {
        code: cg.code,
        has_module_syntax,
        declared_cjs_names,
        source_map,
    })
}

/// The specifiers a module statically imports or re-exports from (`import … from`, `export … from`,
/// type-only imports excluded), or `None` when `source` has no ES module syntax.
///
/// # Errors
///
/// A syntax error in `source`.
pub fn static_imports(
    source: &str,
    path: &Path,
    lang: SourceLang,
) -> Result<Option<Vec<String>>, Error> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, source_type_for(lang))
        .with_options(ParseOptions {
            allow_return_outside_function: lang == SourceLang::JavaScript,
            ..ParseOptions::default()
        })
        .parse();
    if ret.fatal_error || ret.diagnostics.has_errors() {
        let errors: Vec<_> = ret.diagnostics.errors().cloned().collect();
        return Err(diagnostic_error(path, source, &errors));
    }
    let record = &ret.module_record;
    if !record.has_module_syntax && record.import_metas.is_empty() {
        return Ok(None);
    }
    let specifiers = record
        .requested_modules
        .iter()
        .filter(|(_, requests)| requests.iter().any(|r| !r.is_type))
        .map(|(specifier, _)| specifier.to_string())
        .collect();
    Ok(Some(specifiers))
}

/// Cheap check used for `.js` files inside `node_modules`: returns `false` when the
/// text certainly has no ESM syntax (so it can be loaded as CommonJS without parsing).
pub fn may_have_module_syntax(source: &str) -> bool {
    source.contains("import") || source.contains("export")
}

/// Convenience: strip TypeScript types from a snippet (used for evaluating TS code strings).
pub fn strip_types(source: &str) -> Result<String, Error> {
    Ok(transpile(source, Path::new("input.ts"), SourceLang::TypeScript)?.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_types_enums_and_param_props() {
        let src = r#"
import type { Foo } from "foo";
import { Bar, type Baz } from "bar";
interface I { a: number }
type T = string | number;
enum Color { Red, Green = 5, Blue }
class P { constructor(private readonly x: number, public y?: string) {} get v(): number { return this.x; } }
export function f<T>(a: T, b?: Baz): T { const q = a as unknown as T; return q!; }
export default new P(1).v + Color.Blue + (Bar as any);
"#;
        let out = transpile(src, Path::new("a.ts"), SourceLang::TypeScript).unwrap();
        assert!(out.has_module_syntax);
        assert!(!out.code.contains("interface"));
        assert!(!out.code.contains("\"foo\""), "type import must be elided: {}", out.code);
        assert!(out.code.contains("\"bar\""));
        assert!(out.code.contains("Color"));
        assert!(out.code.contains("this.x = x"), "{}", out.code);
        assert!(out.source_map.is_some());
    }

    #[test]
    fn detects_cjs_and_bindings() {
        let out = transpile(
            "const require = 1; module.exports = { a: require };",
            Path::new("a.js"),
            SourceLang::JavaScript,
        )
        .unwrap();
        assert!(!out.has_module_syntax);
        assert_eq!(out.declared_cjs_names, vec!["require"]);
        let out = transpile("export const x = 1;", Path::new("a.js"), SourceLang::JavaScript).unwrap();
        assert!(out.has_module_syntax);
        let out = transpile("const x = 1; module.exports = x;", Path::new("a.ts"), SourceLang::TypeScript).unwrap();
        assert!(!out.has_module_syntax);
    }

    #[test]
    fn syntax_error_has_location() {
        let err = transpile("let = ;", Path::new("bad.ts"), SourceLang::TypeScript).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad.ts:1:"), "{msg}");
    }

    #[test]
    fn source_map_lookup() {
        let src = "const a: number = 1;\n\n\ninterface X {}\nthrow new Error('x' + a);\n";
        let out = transpile(src, Path::new("m.ts"), SourceLang::TypeScript).unwrap();
        let line = out.code.lines().position(|l| l.contains("throw")).unwrap() as u32 + 1;
        let (l, _) = out.source_map.unwrap().lookup(line, 1).unwrap();
        assert_eq!(l, 5);
    }
}
