//! The inference engine for the first vertical slice.
//!
//! It implements exactly one proof-layer diagnostic (ADR-0002, held to the
//! zero-false-positive bar): [`ID`] = `type.argument-mismatch` (ADR-0022 kebab
//! `family.rule`). A call to a **user-defined function in the same file** that
//! passes a **literal** argument which **provably** raises a runtime `TypeError`
//! under PHP 8.1+ semantics (ADR-0011), honoring the calling file's
//! `declare(strict_types=1)`, is flagged. Everything not provable is silent.
//!
//! The whole thing is a salsa tracked query ([`diagnostics`]) built on
//! `steins-db`'s [`parse`] / [`function_index`] queries (ADR-0009), so it is a
//! memoized fact, not a batch pass.

use steins_db::{Db, SourceFile, function_index, parse};
use steins_syntax::{ArgValue, FunctionDecl, ParamType, ScalarType};

/// The registry id for the one check this crate emits (ADR-0022).
pub const ID: &str = "type.argument-mismatch";

/// A proof-layer finding. Kept deliberately flat so the CLI can render text or
/// JSON without knowing anything about the analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub id: &'static str,
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

/// The proof-layer diagnostics for one file, as a memoized salsa query.
#[salsa::tracked]
pub fn diagnostics(db: &dyn Db, file: SourceFile) -> Vec<Diagnostic> {
    let tree = parse(db, file);
    let functions = function_index(db, file);
    check(tree, functions, file.path(db))
}

/// The pure checking core (no salsa) — easy to unit-test and to reuse.
#[must_use]
pub fn check(
    tree: &steins_syntax::SourceTree,
    functions: &[FunctionDecl],
    path: &str,
) -> Vec<Diagnostic> {
    let strict = tree.has_strict_types();
    let mut out = Vec::new();

    for call in tree.calls() {
        // Only statically-known callees with purely positional literal args.
        if !call.positional_only {
            continue;
        }
        let Some(callee) = call.callee.as_deref() else { continue };

        // Resolve to a *unique* same-file user function. Ambiguity → silence.
        let mut matches = functions.iter().filter(|f| f.name == callee);
        let Some(decl) = matches.next() else { continue };
        if matches.next().is_some() {
            continue; // redeclaration; not our call to make.
        }

        for (i, arg) in call.args.iter().enumerate() {
            let Some(param) = decl.params.get(i) else { break }; // extra args → skip (no arity check)
            if param.variadic {
                break; // this and every later position bind to the variadic — skip.
            }
            if param.by_ref {
                continue; // by-ref params are not literal-checkable here.
            }
            let Some(ty) = param.ty else { continue }; // untyped / non-scalar → skip.
            if matches!(arg.value, ArgValue::Other) {
                continue; // non-literal → not provable.
            }

            if is_type_error(strict, ty, &arg.value) {
                let pos = tree.position(arg.span.start);
                let mode = if strict { "strict" } else { "coercive" };
                let message = format!(
                    "argument {} to {}() cannot become {} ${} — proven TypeError ({} mode)",
                    arg.value.render(),
                    callee,
                    ty.scalar.keyword(),
                    param.name,
                    mode,
                );
                out.push(Diagnostic { id: ID, path: path.to_owned(), line: pos.line, column: pos.column, message });
            }
        }
    }

    out
}

/// The truth table: does passing `arg` to a parameter of type `ty` provably
/// raise a `TypeError` under PHP 8.1+ (given `strict` = `declare(strict_types=1)`)?
///
/// The bar is *provable breakage*. When unsure, return `false` (silent).
fn is_type_error(strict: bool, ty: ParamType, arg: &ArgValue) -> bool {
    // `null` is special and mode-independent: it satisfies a nullable param and
    // otherwise always errors — userland functions never coerce `null`.
    if matches!(arg, ArgValue::Null) {
        return !ty.nullable;
    }

    if strict {
        // Strict mode: no scalar coercion, with the single exception of int→float.
        match ty.scalar {
            ScalarType::Int => !matches!(arg, ArgValue::Int(_)),
            ScalarType::Float => !matches!(arg, ArgValue::Int(_) | ArgValue::Float(_)),
            ScalarType::String => !matches!(arg, ArgValue::Str(_)),
            ScalarType::Bool => !matches!(arg, ArgValue::Bool(_)),
        }
    } else {
        // Coercive mode: the only literal TypeErrors are non-numeric strings into
        // a numeric (int|float) parameter. Everything else coerces silently.
        match ty.scalar {
            ScalarType::Int | ScalarType::Float => match arg {
                ArgValue::Str(s) => !php_is_numeric(s),
                _ => false,
            },
            ScalarType::String | ScalarType::Bool => false,
        }
    }
}

/// PHP 8 `is_numeric` semantics: optional leading/trailing whitespace, optional
/// sign, decimal integer or float with optional exponent. Hex, `inf`, and `nan`
/// are *not* numeric strings.
fn php_is_numeric(s: &str) -> bool {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    let bytes = s.as_bytes();
    let mut i = 0;

    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }

    let mut saw_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        saw_digit = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return false;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut saw_exp = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_exp = true;
        }
        if !saw_exp {
            return false;
        }
    }

    i == bytes.len()
}
