//! The sidecar **wire format** — the types and the JSON codec, with no transport.
//!
//! Everything here is pure `serde_json` data manipulation, so it compiles on every
//! target including `wasm32-unknown-unknown` (ADR-0066). The process transport that
//! spawns `php` lives in the crate's `process` module and is native-only; a browser transport
//! (php-wasm, issue #64) speaks the *same* request/response shapes by construction,
//! because it calls the same constructors and the same parsers.
//!
//! # One source of truth
//!
//! A request is `{"jsonrpc":"2.0","id":N,"method":M,"params":P}` and a response is
//! `{"jsonrpc":"2.0","id":N,"result":R}`. Only the `params` half and the `result`
//! half are semantic; the framing belongs to whichever transport carries it. So the
//! functions below come in pairs — `*_params` builds `P`, `parse_*_result` reads `R`
//! — and *both* transports go through them. A second transport can therefore not
//! drift from the first without changing this file.

/// A JSON-encodable literal argument to a folded call: the scalar literals the
/// trace IR carries (ADR-0027), plus an **array literal** of them (issue #39).
///
/// # Why an array argument is a list of entries, not a JSON object
///
/// PHP array semantics that JSON cannot express are deliberately left to the
/// runtime rather than reimplemented here (ADR-0004: a fold is the value the
/// *project's own PHP* produces). An entry's key is `None` for an absent key
/// (`[$a, $b]`), and the runner appends with `$arr[] =`, so PHP's own next-int
/// rule assigns it — including the negative-key edge PHP 8.3 changed. Duplicate
/// keys resolve by plain assignment, i.e. PHP's own last-wins. A JSON object
/// could carry neither (it has no absent key, and its keys are all strings).
#[derive(Debug, Clone, PartialEq)]
pub enum FoldArg {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    /// An array literal: its entries in source order, each an already
    /// PHP-normalized key (`None` = absent, assigned by the runtime) and a value
    /// that is itself a [`FoldArg`] — so nested array literals are representable.
    Array(Vec<(Option<FoldKey>, FoldArg)>),
}

/// An explicit array-literal key on the fold wire. PHP normalization (integer-like
/// strings to `Int`, floats truncated, `bool` to `int`, `null` to `""`) has already
/// happened at lowering, so only the two runtime key types survive here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldKey {
    Int(i64),
    Str(String),
}

/// A concrete value returned by a successful fold, tagged with its PHP type.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

/// The outcome of a `fold` request (ADR-0024). An exception is a *result*, not
/// an error — `1/0` yields `Throw { class: "DivisionByZeroError" }`.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldResult {
    /// The call returned a value we can carry as a literal.
    Value(FoldValue),
    /// The call threw; `class` is the Throwable's class name.
    Throw { class: String },
    /// Anything we cannot turn into type information: unknown function, wrong
    /// arity, unencodable result, or any sidecar failure (timeout/IO/poison).
    Widen { reason: String },
}

impl FoldResult {
    /// The decline value. Public because every transport needs to spell it: the
    /// process transport widens on IO failure, and the replay transport (ADR-0066)
    /// widens on an unanswered request.
    #[must_use]
    pub fn widen(reason: impl Into<String>) -> Self {
        FoldResult::Widen { reason: reason.into() }
    }
}

/// Environment facts reported by the `env` method — coverage-posture material.
/// [`Self::extensions`] is the loaded-extension list ADR-0049 A9 consults (a
/// monkey-patch extension like `uopz`/`runkit7`/`Componere` voids the family), so
/// no separate reflect query is needed for it.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvInfo {
    pub php_version: String,
    pub extensions: Vec<String>,
    pub sapi: String,
    /// `PHP_INT_SIZE` — the engine's integer width in **bytes** (issue #64).
    /// `None` when the runner did not report one (a foreign or older runner).
    ///
    /// The version string does not determine the integer machine. php-wasm 0.1.0
    /// is PHP 8.5.2 — the pinned minor — built 32-bit, and on it `1 << 40` is `0`,
    /// `crc32('x')` is negative, `hexdec('FFFFFFFFF')` promotes to float and
    /// `strtotime('2040-01-01')` is `false`. Every one of those is a *silently
    /// wrong value*, not a failure, so the minor gate alone is unsound and the
    /// fold lane consults this instead.
    pub int_size: Option<u32>,
}

/// The result of a `reflect(target)` existence query (ADR-0024 surface / ADR-0049
/// §1 oracle (b)): whether the project's own PHP knows `target` among its builtins
/// and loaded extensions. A structured *not-found* is `exists() == false`, distinct
/// from a failed query (which the wrapper returns as `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reflection {
    /// The name asked about (echoed back from the request).
    pub target: String,
    /// The name is a resident function (`function_exists`).
    pub function_exists: bool,
    /// The name is a resident class-like — class, interface, trait, or enum.
    pub class_like_exists: bool,
    /// The resident function's native return type, as the `(string)` rendering of
    /// `ReflectionFunction::getReturnType()` — or `getTentativeReturnType()` when
    /// the former is null (ADR-0056 R1). `None` when the name is not a function or
    /// declares no return type at all. Examples: `"bool"`, `"int"`, `"?string"`,
    /// `"int|false"`. This is the reflected *envelope* the return-fact seeder
    /// lowers; it is the running engine's own declaration for its own builtin, so
    /// it is version-correct by construction (ADR-0056 §1).
    pub return_type: Option<String>,
    /// Whether [`Self::return_type`] came from the *tentative* return type (the
    /// function declared no `getReturnType()` but the engine carries a tentative
    /// one). Still the engine's own claim; recorded distinctly per ADR-0056 §7.
    pub return_type_tentative: bool,
}

impl Reflection {
    /// Whether the name exists at all on the boot surface (function or class-like).
    #[must_use]
    pub fn exists(&self) -> bool {
        self.function_exists || self.class_like_exists
    }
}

/// The wire tag that marks an array argument. A scalar argument encodes to a bare
/// JSON scalar, so a JSON *object* can only ever be this envelope — the tag is a
/// readability aid and a shape check for the runner, not a disambiguator.
pub const ARRAY_TAG: &str = "__steins_array";

// ---------------------------------------------------------------------------
// Requests: the `params` half of each method.
// ---------------------------------------------------------------------------

/// The `params` of an `env` request. The method takes no arguments; the empty
/// object is still spelled here so the request key a replay transport builds is
/// byte-identical to the one the process transport sends.
#[must_use]
pub fn env_params() -> serde_json::Value {
    serde_json::json!({})
}

/// The `params` of a `reflect` request.
#[must_use]
pub fn reflect_params(target: &str) -> serde_json::Value {
    serde_json::json!({ "target": target })
}

/// The `params` of a `fold` request: the function's simple name plus its already
/// budget-checked literal arguments, each encoded by [`fold_arg_to_json`].
#[must_use]
pub fn fold_params(name: &str, args: &[FoldArg]) -> serde_json::Value {
    serde_json::json!({
        "function": name,
        "args": args.iter().map(fold_arg_to_json).collect::<Vec<_>>(),
    })
}

/// Encode a [`FoldArg`] as JSON, preserving float-ness (`5.0`, not `5`).
///
/// An array becomes `{"__steins_array": [[key, value], …]}` with `key` being
/// `null` (absent), an integer, or a string — the three [`FoldKey`] states. Values
/// recurse, so a nested array literal encodes as a nested envelope.
#[must_use]
pub fn fold_arg_to_json(arg: &FoldArg) -> serde_json::Value {
    match arg {
        FoldArg::Int(v) => serde_json::json!(v),
        FoldArg::Float(v) => serde_json::Number::from_f64(*v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        FoldArg::Str(v) => serde_json::json!(v),
        FoldArg::Bool(v) => serde_json::json!(v),
        FoldArg::Null => serde_json::Value::Null,
        FoldArg::Array(entries) => {
            let items: Vec<serde_json::Value> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        None => serde_json::Value::Null,
                        Some(FoldKey::Int(i)) => serde_json::json!(i),
                        Some(FoldKey::Str(s)) => serde_json::json!(s),
                    };
                    serde_json::Value::Array(vec![key, fold_arg_to_json(v)])
                })
                .collect();
            serde_json::json!({ ARRAY_TAG: items })
        }
    }
}

// ---------------------------------------------------------------------------
// Responses: the `result` half of each method.
// ---------------------------------------------------------------------------

/// Interpret an `env` `result` object. `None` on any shape we do not recognize —
/// the caller treats that as "unanswerable", never as a fabricated environment.
#[must_use]
pub fn parse_env_result(result: &serde_json::Value) -> Option<EnvInfo> {
    Some(EnvInfo {
        php_version: result.get("php_version")?.as_str()?.to_owned(),
        extensions: result
            .get("extensions")?
            .as_array()?
            .iter()
            .filter_map(|e| e.as_str().map(ToOwned::to_owned))
            .collect(),
        sapi: result.get("sapi")?.as_str()?.to_owned(),
        // Absent on a runner that predates the field: unknown width, which the
        // fold gate treats as "not provably 64-bit" and declines.
        int_size: result.get("int_size").and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok()),
    })
}

/// Interpret a `reflect` `result` object for the name `target` that was asked
/// about. Only a structured `reflection` reply is an existence answer; a `widen`
/// (malformed request, or a runner too old to implement `reflect`) is unknown and
/// yields `None`. `target` is the fallback for a reply that does not echo the name.
#[must_use]
pub fn parse_reflection_result(result: &serde_json::Value, target: &str) -> Option<Reflection> {
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("reflection") {
        return None;
    }
    Some(Reflection {
        target: result
            .get("target")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(target)
            .to_owned(),
        function_exists: result.get("function").and_then(serde_json::Value::as_bool).unwrap_or(false),
        class_like_exists: result
            .get("class_like")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        // Absent (an older runner) or JSON `null` both map to `None` — no
        // reflected envelope, so the seeder widens away (ADR-0056).
        return_type: result
            .get("return_type")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        return_type_tentative: result
            .get("return_type_tentative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Interpret a `fold` `result` object (`{kind, ...}`) as a [`FoldResult`]. Any
/// shape we do not recognize widens — never a wrong value.
#[must_use]
pub fn parse_fold_result(result: &serde_json::Value) -> FoldResult {
    match result.get("kind").and_then(serde_json::Value::as_str) {
        Some("value") => parse_fold_value(result)
            .map_or_else(|| FoldResult::widen("unencodable value"), FoldResult::Value),
        Some("throw") => match result.get("class").and_then(serde_json::Value::as_str) {
            Some(class) => FoldResult::Throw { class: class.to_owned() },
            None => FoldResult::widen("throw without class"),
        },
        Some("widen") => FoldResult::widen(
            result.get("reason").and_then(serde_json::Value::as_str).unwrap_or("widen").to_owned(),
        ),
        _ => FoldResult::widen("unknown result kind"),
    }
}

/// Turn a `{kind:"value", value, type}` object into a typed [`FoldValue`]. The
/// `type` tag disambiguates cases JSON alone cannot (e.g. `1` as int vs. bool).
#[must_use]
pub fn parse_fold_value(result: &serde_json::Value) -> Option<FoldValue> {
    let value = result.get("value")?;
    match result.get("type").and_then(serde_json::Value::as_str)? {
        "int" => value.as_i64().map(FoldValue::Int),
        "float" => value.as_f64().map(FoldValue::Float),
        "string" => value.as_str().map(|s| FoldValue::Str(s.to_owned())),
        "bool" => value.as_bool().map(FoldValue::Bool),
        "null" => Some(FoldValue::Null),
        // An array **result** widens: issue #39 makes array literals fold
        // *arguments*, and stops there deliberately. Carrying a folded array back
        // as a value would seed synthesized array facts into the env, which is the
        // array-return work of #41/#42 — a documented boundary, not an oversight.
        // Anything else (objects, resources) has no literal in our IR at all.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_params_is_the_empty_object() {
        assert_eq!(env_params(), serde_json::json!({}));
    }

    #[test]
    fn reflect_params_carries_the_target() {
        assert_eq!(reflect_params("strlen"), serde_json::json!({ "target": "strlen" }));
    }

    #[test]
    fn fold_params_encode_scalars_with_their_phpness() {
        let p = fold_params("strtoupper", &[FoldArg::Str("ab".to_owned())]);
        assert_eq!(p, serde_json::json!({ "function": "strtoupper", "args": ["ab"] }));
        // A float stays a float on the wire — `5.0`, not `5`.
        let p = fold_params("strval", &[FoldArg::Float(5.0)]);
        assert_eq!(p["args"][0].as_f64(), Some(5.0));
        assert!(p["args"][0].is_f64(), "float-ness survives: {p}");
    }

    #[test]
    fn fold_params_encode_arrays_as_tagged_entry_lists() {
        let arg = FoldArg::Array(vec![
            (None, FoldArg::Int(1)),
            (Some(FoldKey::Str("k".to_owned())), FoldArg::Bool(true)),
            (Some(FoldKey::Int(-3)), FoldArg::Null),
        ]);
        let p = fold_params("count", &[arg]);
        assert_eq!(
            p["args"][0][ARRAY_TAG],
            serde_json::json!([[serde_json::Value::Null, 1], ["k", true], [-3, serde_json::Value::Null]])
        );
    }

    #[test]
    fn nested_array_arguments_nest_their_envelopes() {
        let inner = FoldArg::Array(vec![(None, FoldArg::Int(7))]);
        let outer = FoldArg::Array(vec![(None, inner)]);
        let p = fold_params("count", &[outer]);
        assert_eq!(p["args"][0][ARRAY_TAG][0][1][ARRAY_TAG][0][1], serde_json::json!(7));
    }

    #[test]
    fn env_result_parses_or_declines() {
        let ok = serde_json::json!({
            "php_version": "8.5.8",
            "extensions": ["Core", "standard"],
            "sapi": "cli",
            "int_size": 8,
        });
        assert_eq!(
            parse_env_result(&ok),
            Some(EnvInfo {
                php_version: "8.5.8".to_owned(),
                extensions: vec!["Core".to_owned(), "standard".to_owned()],
                sapi: "cli".to_owned(),
                int_size: Some(8),
            })
        );
        // A missing field is unanswerable, not a partial environment.
        assert_eq!(parse_env_result(&serde_json::json!({ "php_version": "8.5.8" })), None);
        assert_eq!(parse_env_result(&serde_json::json!("nope")), None);
    }

    /// `int_size` is the one OPTIONAL env field: a runner that predates it still
    /// answers, and the width reads as unknown (which the fold gate declines on).
    #[test]
    fn an_absent_int_size_is_unknown_not_a_failed_env() {
        let old = serde_json::json!({
            "php_version": "8.5.8",
            "extensions": [],
            "sapi": "cli",
        });
        let env = parse_env_result(&old).expect("env still parses");
        assert_eq!(env.int_size, None);
        // A 32-bit engine reports its real width — this is php-wasm's shape.
        let wasm = serde_json::json!({
            "php_version": "8.5.2",
            "extensions": [],
            "sapi": "embed",
            "int_size": 4,
        });
        assert_eq!(parse_env_result(&wasm).expect("env").int_size, Some(4));
    }

    #[test]
    fn reflection_requires_the_reflection_kind() {
        let widen = serde_json::json!({ "kind": "widen", "reason": "unknown method" });
        assert_eq!(parse_reflection_result(&widen, "strlen"), None);
        let refl = serde_json::json!({
            "kind": "reflection",
            "target": "strlen",
            "function": true,
            "class_like": false,
            "return_type": "int",
        });
        let r = parse_reflection_result(&refl, "strlen").expect("reflection");
        assert!(r.function_exists && !r.class_like_exists && r.exists());
        assert_eq!(r.return_type.as_deref(), Some("int"));
        assert!(!r.return_type_tentative);
    }

    #[test]
    fn reflection_falls_back_to_the_asked_target() {
        let refl = serde_json::json!({ "kind": "reflection", "function": false, "class_like": true });
        let r = parse_reflection_result(&refl, "Countable").expect("reflection");
        assert_eq!(r.target, "Countable");
        assert_eq!(r.return_type, None);
    }

    #[test]
    fn fold_result_kinds_round_trip() {
        let v = serde_json::json!({ "kind": "value", "type": "string", "value": "AB" });
        assert_eq!(parse_fold_result(&v), FoldResult::Value(FoldValue::Str("AB".to_owned())));
        let t = serde_json::json!({ "kind": "throw", "class": "DivisionByZeroError" });
        assert_eq!(parse_fold_result(&t), FoldResult::Throw { class: "DivisionByZeroError".to_owned() });
        let w = serde_json::json!({ "kind": "widen", "reason": "unknown function" });
        assert_eq!(parse_fold_result(&w), FoldResult::widen("unknown function"));
    }

    #[test]
    fn an_unrecognized_fold_result_widens_never_values() {
        for bad in [
            serde_json::json!({}),
            serde_json::json!({ "kind": "array" }),
            serde_json::json!({ "kind": "value", "type": "array", "value": [] }),
            serde_json::json!({ "kind": "throw" }),
            serde_json::json!(42),
        ] {
            assert!(matches!(parse_fold_result(&bad), FoldResult::Widen { .. }), "{bad}");
        }
    }

    #[test]
    fn the_type_tag_disambiguates_int_from_bool() {
        let i = serde_json::json!({ "kind": "value", "type": "int", "value": 1 });
        assert_eq!(parse_fold_value(&i), Some(FoldValue::Int(1)));
        let b = serde_json::json!({ "kind": "value", "type": "bool", "value": true });
        assert_eq!(parse_fold_value(&b), Some(FoldValue::Bool(true)));
        let n = serde_json::json!({ "kind": "value", "type": "null", "value": serde_json::Value::Null });
        assert_eq!(parse_fold_value(&n), Some(FoldValue::Null));
    }
}
