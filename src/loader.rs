//! Module specifier resolution and source loading (with TypeScript stripping).

use std::path::{Path, PathBuf};

use crate::permissions::normalize_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Bare specifiers like `"lodash"` are not supported (no package manager).
    Bare(String),
    /// URL schemes other than `file:` are not supported.
    UnsupportedScheme(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Bare(s) => write!(
                f,
                "Relative import path \"{s}\" not prefixed with / or ./ or ../"
            ),
            ResolveError::UnsupportedScheme(s) => write!(f, "Unsupported module scheme in \"{s}\""),
        }
    }
}

/// Resolve `specifier` as imported from the module at `referrer` (an absolute
/// path). Only relative (`./`, `../`), absolute (`/`) and `file://`
/// specifiers are accepted. The result is lexically normalised so the same
/// file always has the same key in the module map (important for cycles).
pub fn resolve(specifier: &str, referrer: &Path) -> Result<PathBuf, ResolveError> {
    if let Some(rest) = specifier.strip_prefix("file://") {
        return Ok(normalize_path(Path::new(rest), Path::new("/")));
    }
    if specifier.contains("://") || specifier.starts_with("data:") || specifier.starts_with("node:")
    {
        return Err(ResolveError::UnsupportedScheme(specifier.to_string()));
    }
    if specifier.starts_with('/') {
        return Ok(normalize_path(Path::new(specifier), Path::new("/")));
    }
    if specifier.starts_with("./") || specifier.starts_with("../") {
        let base = referrer.parent().unwrap_or(Path::new("/"));
        return Ok(normalize_path(Path::new(specifier), base));
    }
    Err(ResolveError::Bare(specifier.to_string()))
}

/// Media type decided from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    JavaScript,
    TypeScript,
    Json,
}

pub fn media_type(path: &Path) -> MediaType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "mts" | "cts" | "tsx") => MediaType::TypeScript,
        Some("json") => MediaType::Json,
        _ => MediaType::JavaScript,
    }
}

/// Strip TypeScript syntax using oxc (parse -> semantic -> TS transform ->
/// codegen). Type-only imports/exports are removed; enums and namespaces are
/// lowered to JS. No type checking is performed.
pub fn strip_types(source: &str, path: &Path) -> Result<String, String> {
    use oxc::allocator::Allocator;
    use oxc::codegen::Codegen;
    use oxc::parser::Parser;
    use oxc::semantic::SemanticBuilder;
    use oxc::span::SourceType;
    use oxc::transformer::{TransformOptions, Transformer};

    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path)
        .unwrap_or_else(|_| SourceType::ts())
        .with_module(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if ret.diagnostics.has_errors() {
        let msgs: Vec<String> = ret.diagnostics.iter().map(|e| e.to_string()).collect();
        return Err(format!("{}: {}", path.display(), msgs.join("; ")));
    }
    let mut program = ret.program;
    let sem = SemanticBuilder::new().with_enum_eval(true).build(&program);
    let scoping = sem.semantic.into_scoping();
    let options = TransformOptions::default();
    let tret =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if tret.diagnostics.has_errors() {
        return Err(format!("{}: TypeScript transform failed", path.display()));
    }
    Ok(Codegen::new().build(&program).code)
}

/// Turn the raw bytes of a module file into JavaScript source.
pub fn to_javascript(path: &Path, source: String) -> Result<String, String> {
    match media_type(path) {
        MediaType::JavaScript => Ok(source),
        MediaType::TypeScript => strip_types(&source, path),
        MediaType::Json => Ok(format!("export default {};", source.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_resolution() {
        let r = Path::new("/proj/src/main.ts");
        assert_eq!(
            resolve("./a.ts", r).unwrap(),
            PathBuf::from("/proj/src/a.ts")
        );
        assert_eq!(
            resolve("../lib/b.js", r).unwrap(),
            PathBuf::from("/proj/lib/b.js")
        );
        assert_eq!(
            resolve("./x/../y.js", r).unwrap(),
            PathBuf::from("/proj/src/y.js")
        );
        assert_eq!(resolve("/abs/c.js", r).unwrap(), PathBuf::from("/abs/c.js"));
        assert_eq!(
            resolve("file:///abs/d.js", r).unwrap(),
            PathBuf::from("/abs/d.js")
        );
    }

    #[test]
    fn same_file_same_key() {
        let a = resolve("./b.js", Path::new("/p/a.js")).unwrap();
        let b = resolve("../p/./b.js", Path::new("/p/sub/../c.js")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_bare_and_remote() {
        let r = Path::new("/p/a.js");
        assert_eq!(
            resolve("lodash", r),
            Err(ResolveError::Bare("lodash".into()))
        );
        assert!(matches!(
            resolve("https://x.com/a.js", r),
            Err(ResolveError::UnsupportedScheme(_))
        ));
        assert!(matches!(
            resolve("node:fs", r),
            Err(ResolveError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn media_types() {
        assert_eq!(media_type(Path::new("a.ts")), MediaType::TypeScript);
        assert_eq!(media_type(Path::new("a.mts")), MediaType::TypeScript);
        assert_eq!(media_type(Path::new("a.js")), MediaType::JavaScript);
        assert_eq!(media_type(Path::new("a.json")), MediaType::Json);
    }

    #[test]
    fn strips_types() {
        let src = r#"
            import type { Foo } from "./foo.ts";
            interface P { x: number }
            type Id = string;
            enum Color { Red, Green }
            export function add(a: number, b: number): number { return a + b as number; }
            const p: P = { x: 1 };
            let g = <T,>(v: T): T => v;
            export { Color };
        "#;
        let out = strip_types(src, Path::new("t.ts")).unwrap();
        assert!(!out.contains("interface"), "{out}");
        assert!(!out.contains("import type"), "{out}");
        assert!(!out.contains(": number"), "{out}");
        assert!(out.contains("function add(a, b)"), "{out}");
        assert!(out.contains("Color"), "{out}");
    }

    #[test]
    fn ts_syntax_error_is_reported() {
        let err = strip_types("let x: = 1;", Path::new("bad.ts")).unwrap_err();
        assert!(err.contains("bad.ts"));
    }

    #[test]
    fn json_modules() {
        let js = to_javascript(Path::new("a.json"), "{\"a\": 1}\n".into()).unwrap();
        assert_eq!(js, "export default {\"a\": 1};");
    }
}
