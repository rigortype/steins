//! The sidecar **wire format** — the types and the JSON codec, with no transport.
//! Pure `serde_json`; compiles on every target incl. `wasm32-unknown-unknown`
//! (ADR-0066). The native `process` transport and a future browser transport
//! (php-wasm, issue #64) share these shapes by construction — both call the same
//! constructors and parsers. A request is `{"jsonrpc":"2.0","id":N,"method":M,"params":P}`;
//! a response is `{"jsonrpc":"2.0","id":N,"result":R}` — only `params`/`result` are
//! semantic. Functions pair up: `*_params` builds `P`, `parse_*_result` reads `R`.

/// A JSON-encodable literal argument to a folded call: the scalar literals the
/// trace IR carries (ADR-0027), plus an **array literal** of them (issue #39).
/// Not a JSON object: PHP array semantics JSON cannot express are left to the
/// runtime (ADR-0004). `None` is an absent key (`[$a, $b]`), next-int assigned by
/// `$arr[] =` (PHP 8.3 changed the negative-key edge); duplicates resolve last-wins.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldArg {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    /// An array literal: entries in source order, each an already PHP-normalized
    /// key (`None` = absent) and a nested [`FoldArg`] value (nesting representable).
    Array(Vec<(Option<FoldKey>, FoldArg)>),
}

/// An explicit array-literal key on the fold wire. PHP normalization (int-like
/// strings, floats, bools, null) already ran at lowering — only two key types survive.
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
    /// An array the engine finished building (ADR-0028 2026-08-14 amendment, issue
    /// #330): a materialized [`FoldKey`] per entry, not the argument side's
    /// `Option<FoldKey>`. [`parse_fold_value`] rejects the absent spelling.
    Array(Vec<(FoldKey, FoldValue)>),
}

/// The outcome of a `fold` request (ADR-0024). An exception is a *result*, not
/// an error — `1/0` yields `Throw { class: "DivisionByZeroError" }`.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldResult {
    /// The call returned a value we can carry as a literal.
    Value(FoldValue),
    /// The call threw; `class` is the Throwable's class name.
    Throw { class: String },
    /// Untypeable: unknown function, wrong arity, unencodable result, sidecar failure.
    Widen { reason: String },
}

impl FoldResult {
    /// The decline value; process widens on IO failure, replay (ADR-0066) on no answer.
    #[must_use]
    pub fn widen(reason: impl Into<String>) -> Self {
        FoldResult::Widen { reason: reason.into() }
    }
}

/// Environment facts reported by the `env` method — coverage-posture material.
/// [`Self::extensions`] is what ADR-0049 A9 checks for family-voiding monkey-patchers.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvInfo {
    pub php_version: String,
    pub extensions: Vec<String>,
    pub sapi: String,
    /// `PHP_INT_SIZE` in **bytes** (issue #64); `None` on an old runner. php-wasm 0.1.0 is
    /// 32-bit PHP 8.5.2 (`1 << 40` silently becomes `0`) — trust this field, not the minor.
    pub int_size: Option<u32>,
}

/// The result of a `reflect(target)` existence query (ADR-0024 / ADR-0049 §1 oracle
/// b). A structured *not-found* is `exists() == false`, distinct from a failed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reflection {
    /// The name asked about (echoed back from the request).
    pub target: String,
    /// The name is a resident function (`function_exists`).
    pub function_exists: bool,
    /// The name is a resident class-like — class, interface, trait, or enum.
    pub class_like_exists: bool,
    /// Native return type: `(string)` of `getReturnType()`, or
    /// `getTentativeReturnType()` when null (ADR-0056 R1); `None` if neither exists.
    pub return_type: Option<String>,
    /// Whether [`Self::return_type`] is *tentative* (no `getReturnType()`); ADR-0056 §7.
    pub return_type_tentative: bool,
    /// `getNumberOfParameters()` — ADR-0064's mixed-pin arity leg: a bare-`mixed`
    /// rule pins the live signature instead of a structural declaration; else `None`.
    pub params_total: Option<u32>,
    /// `getNumberOfRequiredParameters()`; same `None` semantics as [`Self::params_total`].
    pub params_required: Option<u32>,
    /// `getParameters()` per position, in declaration order (ADR-0056 §9) — the
    /// parameter twin of [`Self::return_type`]. `None` where the counts above are
    /// `None`, and for the same reasons: an older runner, a replay table recorded
    /// before the field, a reflection failure, a name that is not a function. A
    /// zero-parameter function reports `Some(vec![])` — an empty list is an answer.
    pub params: Option<Vec<BuiltinParam>>,
}

/// One parameter of a resident function as the running engine reports it
/// (ADR-0056 §9): the Verified, version-correct signature the argument judgment
/// reads, never a curated row.
///
/// The three shape bits travel because each is a *decline* on the consuming side —
/// a by-ref position takes an out-parameter rather than a value, a variadic binds
/// every argument after it, and neither is a type question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinParam {
    /// `getName()` without the leading `$`, so a finding can name the parameter
    /// the way PHP's own `TypeError` does (`strlen(): Argument #1 ($string)`).
    pub name: String,
    /// The `(string)` rendering of `getType()` (`"string"`, `"?int"`,
    /// `"array|string"`), or `None` where the position declares no type at all.
    pub ty: Option<String>,
    /// `isPassedByReference()`.
    pub by_ref: bool,
    /// `isVariadic()`.
    pub variadic: bool,
    /// `isOptional()` — carried for completeness of the signature surface; the
    /// argument judgment does not read it (a defaulted position that *is* given an
    /// argument is type-checked exactly like a required one).
    pub optional: bool,
}

impl Reflection {
    /// Whether the name exists at all on the boot surface (function or class-like).
    #[must_use]
    pub fn exists(&self) -> bool {
        self.function_exists || self.class_like_exists
    }
}

/// The result of a `reflect_class(target)` request (issue #269): the resident **declaration**,
/// or a structured not-found (extension classes like `Redis` have no source declaration —
/// ADR-0049 §1). `None` is definitive here; a failed query is a different `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassReflection {
    /// The name asked about (echoed back from the request).
    pub target: String,
    /// The resident declaration, or `None` when this engine has no such class-like.
    pub declaration: Option<ReflectedClass>,
}

impl ClassReflection {
    /// Whether the engine has the class-like at all.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.declaration.is_some()
    }
}

/// A class-like declaration as the **running engine** reports it (issue #269) —
/// version-correct by construction; *envelope-grade*, never proven: [`ClassReflection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectedClass {
    /// The declared name with the engine's own casing (`Random\Randomizer`).
    pub name: String,
    /// Class, interface, trait, or enum.
    pub kind: ReflectedClassKind,
    /// `isInternal()` — true for engine/extension classes; part of the **origin**.
    pub internal: bool,
    /// `getExtensionName()` — declaring extension, or `None`; other half of the origin.
    pub extension: Option<String>,
    pub is_final: bool,
    pub is_abstract: bool,
    /// The **direct** parent's name, or `None`.
    pub parent: Option<String>,
    /// `getInterfaceNames()` — **transitive** interfaces; re-deriving directness guesses.
    pub interfaces: Vec<String>,
    /// Every method, inherited included — the resolved set a call site can reach.
    pub methods: Vec<ReflectedMethod>,
    /// Class constants as *declarations* (`getReflectionConstants()`), never evaluated.
    pub constants: Vec<ReflectedConst>,
    pub properties: Vec<ReflectedProperty>,
}

/// Which class-like kind a [`ReflectedClass`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectedClassKind {
    Class,
    Interface,
    Trait,
    Enum,
}

/// PHP member visibility, as reported by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

/// One method off a [`ReflectedClass`] — the engine's own signature surface for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectedMethod {
    pub name: String,
    pub is_static: bool,
    pub is_abstract: bool,
    pub is_final: bool,
    pub visibility: Visibility,
    /// `getNumberOfParameters()`.
    pub params_total: u32,
    /// `getNumberOfRequiredParameters()`.
    pub params_required: u32,
    /// Return type, or *tentative* when absent — same form as [`Reflection::return_type`].
    pub return_type: Option<String>,
    /// Whether [`Self::return_type`] came from the tentative return type.
    pub return_type_tentative: bool,
}

/// One class constant off a [`ReflectedClass`]. The **name and visibility only** —
/// no value: reading a value would mean evaluating an initializer inside the sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectedConst {
    pub name: String,
    pub visibility: Visibility,
}

/// One property off a [`ReflectedClass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectedProperty {
    pub name: String,
    pub is_static: bool,
    pub visibility: Visibility,
}

/// The verdict of a `preg_compile(pattern)` request (#189/ADR-0078): what the
/// **project's own PCRE** does with it (`steins_catalog::preg` flags patterns worth
/// asking about, never decides if PCRE accepts them — ADR-0004). Two *answers* only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PregCompile {
    /// The engine's PCRE accepted the pattern.
    Compiles,
    /// The engine's PCRE **refused** the pattern; `message` is PCRE's complaint with PHP's
    /// `<function>(): ` prefix stripped (probe calls `preg_match`) — re-attach is caller's.
    Refuses { message: String },
}

/// The project's own PHP's answer to `defined($name)` for a **global constant**
/// (ADR-0078, issue #198) — the `constant.undefined` ladder's final oracle, since
/// the builtin catalog is never an absence oracle (ADR-0049 §1). Two *answers*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantDefined {
    /// The engine has the constant — a homonym stands, so no absence claim holds.
    Defined,
    /// The engine does not have the constant.
    NotDefined,
}

/// The wire tag marking an array, in **both** directions: an array argument
/// (issue #39) and, since ADR-0028's 2026-08-14 amendment, an array result (#330).
/// A JSON *object* here is always tagged; ADR-0080 §3.1's `__steins_bytes` can join later.
pub const ARRAY_TAG: &str = "__steins_array";

// Requests: the `params` half of each method.

/// The `params` of an `env` request: no arguments, but the empty object is spelled
/// here so a replay transport's key is byte-identical to the process transport's.
#[must_use]
pub fn env_params() -> serde_json::Value {
    serde_json::json!({})
}

/// The `params` of a `reflect` request.
#[must_use]
pub fn reflect_params(target: &str) -> serde_json::Value {
    serde_json::json!({ "target": target })
}

/// The `params` of a `reflect_class` request (issue #269) — same shape as `reflect`.
#[must_use]
pub fn reflect_class_params(target: &str) -> serde_json::Value {
    serde_json::json!({ "target": target })
}

/// The `params` of a `preg_compile` request: the whole pattern as PHP would receive
/// it — delimiters and modifiers included, since those are exactly what PCRE can refuse.
#[must_use]
pub fn preg_compile_params(pattern: &str) -> serde_json::Value {
    serde_json::json!({ "pattern": pattern })
}

/// The `params` of a `defined` request: the fully-resolved name as `defined()`
/// receives it (`FOO`, `App\FOO`) — no leading `\`; case preserved (case-sensitive).
#[must_use]
pub fn defined_params(name: &str) -> serde_json::Value {
    serde_json::json!({ "name": name })
}

/// The `params` of a `fold` request: the function's simple name, its already
/// budget-checked literal arguments (each encoded by [`fold_arg_to_json`]), and
/// the **call site's** calling convention.
///
/// `strict` is `declare(strict_types=1)` at the file the call is *written* in,
/// not a property of the runner or of the analysis. It belongs in the params
/// because the params ARE the request key (ADR-0066 §2): a strict call site and
/// a weak one ask different questions of the same name and arguments, so they
/// must not share a replay-table entry or a memo slot, and putting the field
/// here is what keeps them apart everywhere at once.
///
/// `None` when any argument has no JSON spelling ([`fold_arg_to_json`]) — the
/// request is not askable, so the caller widens rather than sending a question
/// about a value the source does not contain.
#[must_use]
pub fn fold_params(name: &str, args: &[FoldArg], strict: bool) -> Option<serde_json::Value> {
    let args: Option<Vec<_>> = args.iter().map(fold_arg_to_json).collect();
    Some(serde_json::json!({
        "function": name,
        "args": args?,
        "strict": strict,
    }))
}

/// Encode a [`FoldArg`] as JSON, preserving float-ness (`5.0` not `5`); an array
/// becomes `{"__steins_array": [[key, value], …]}`, keys `null`/int/string, recursing.
///
/// `None` when the argument cannot be spelled in JSON, which is exactly the
/// non-finite floats: `INF`, `-INF` and `NAN` have no JSON token, and PHP mints
/// the first two from ordinary source (`1e309` overflows to `INF` in the lexer).
/// This is fallible rather than lossy on purpose. A substitution here is not an
/// imprecision but a **different question**: an earlier revision encoded them as
/// `null`, and a weak-mode `floor(1e309)` came back `0.0` — PHP's answer for
/// `floor(null)` — as a `Verified` value where the program's own answer is
/// `INF`. Callers widen on `None`; a producer that can see the source (the
/// analysis' own fold gate) declines earlier still, and this is the floor under
/// it that no future producer can step through.
#[must_use]
pub fn fold_arg_to_json(arg: &FoldArg) -> Option<serde_json::Value> {
    Some(match arg {
        FoldArg::Int(v) => serde_json::json!(v),
        FoldArg::Float(v) => serde_json::Value::Number(serde_json::Number::from_f64(*v)?),
        FoldArg::Str(v) => serde_json::json!(v),
        FoldArg::Bool(v) => serde_json::json!(v),
        FoldArg::Null => serde_json::Value::Null,
        FoldArg::Array(entries) => {
            let items: Option<Vec<serde_json::Value>> = entries
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        None => serde_json::Value::Null,
                        Some(FoldKey::Int(i)) => serde_json::json!(i),
                        Some(FoldKey::Str(s)) => serde_json::json!(s),
                    };
                    // One unspellable element makes the WHOLE array unaskable:
                    // dropping it would send a shorter array, which is a
                    // different argument, not a wider one.
                    Some(serde_json::Value::Array(vec![key, fold_arg_to_json(v)?]))
                })
                .collect();
            serde_json::json!({ ARRAY_TAG: items? })
        }
    })
}

// Responses: the `result` half of each method.

/// Interpret an `env` `result`; `None` on any unrecognized shape (never fabricated).
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
        // Absent on an old runner: unknown width, which the fold gate declines on.
        int_size: result.get("int_size").and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok()),
    })
}

/// Interpret a `reflect` `result` for `target`; only a structured `reflection`
/// reply answers, else `None`. Falls back to `target` if the reply omits the echo.
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
        // Absent (old runner) or JSON `null` both map to `None`; the seeder widens (ADR-0056).
        return_type: result
            .get("return_type")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        return_type_tentative: result
            .get("return_type_tentative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        // Absent (old runner/replay) or `null` both map to `None`: a mixed-pinned
        // rule withholds. An old reply must keep parsing, never fail outright.
        params_total: parse_count(result.get("params_total")),
        params_required: parse_count(result.get("params_required")),
        // Whole or not at all (ADR-0056 §9): a list one of whose entries does not
        // read is not a signature, and a truncated one would silently renumber
        // every position after the gap. Absent / `null` / unreadable all collapse
        // to `None`, which withholds the judgment.
        params: result.get("params").and_then(parse_builtin_params),
    })
}

/// Read a `reflection` reply's `params` array. `None` unless the value is an
/// array **every** entry of which parses (see the call site).
fn parse_builtin_params(value: &serde_json::Value) -> Option<Vec<BuiltinParam>> {
    value.as_array()?.iter().map(parse_builtin_param).collect()
}

/// One `params` entry. `name` is required — a position with no name is not a
/// position this reply can describe. The type is `None` on absent or `null` (an
/// untyped position); the three shape bits default to `false`, which is the shape
/// of an ordinary by-value required parameter.
fn parse_builtin_param(value: &serde_json::Value) -> Option<BuiltinParam> {
    let flag = |k: &str| value.get(k).and_then(serde_json::Value::as_bool).unwrap_or(false);
    Some(BuiltinParam {
        name: value.get("name")?.as_str()?.to_owned(),
        ty: value.get("type").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        by_ref: flag("by_ref"),
        variadic: flag("variadic"),
        optional: flag("optional"),
    })
}

/// Interpret a `reflect_class` `result` for `target` (issue #269); only a
/// structured `class_reflection` reply answers, else `None`. **Parses whole or
/// not at all** — unreadable member lists yield `None`, never a truncated class.
#[must_use]
pub fn parse_class_reflection_result(
    result: &serde_json::Value,
    target: &str,
) -> Option<ClassReflection> {
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("class_reflection") {
        return None;
    }
    let echoed = result
        .get("target")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(target)
        .to_owned();
    if result.get("exists").and_then(serde_json::Value::as_bool) != Some(true) {
        // Structured not-found (absent/odd `exists` reads the same way).
        return Some(ClassReflection { target: echoed, declaration: None });
    }
    let declaration = ReflectedClass {
        name: result.get("name").and_then(serde_json::Value::as_str)?.to_owned(),
        kind: parse_class_kind(result.get("class_kind").and_then(serde_json::Value::as_str)?)?,
        internal: result.get("internal").and_then(serde_json::Value::as_bool).unwrap_or(false),
        extension: result
            .get("extension")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        is_final: result.get("final").and_then(serde_json::Value::as_bool).unwrap_or(false),
        is_abstract: result.get("abstract").and_then(serde_json::Value::as_bool).unwrap_or(false),
        parent: result.get("parent").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        interfaces: parse_name_list(result.get("interfaces")?)?,
        methods: parse_list(result.get("methods")?, parse_reflected_method)?,
        constants: parse_list(result.get("constants")?, parse_reflected_const)?,
        properties: parse_list(result.get("properties")?, parse_reflected_property)?,
    };
    Some(ClassReflection { target: echoed, declaration: Some(declaration) })
}

/// Map every JSON array element through `one`, failing whole if any element doesn't read.
fn parse_list<T>(
    value: &serde_json::Value,
    one: impl Fn(&serde_json::Value) -> Option<T>,
) -> Option<Vec<T>> {
    value.as_array()?.iter().map(one).collect()
}

/// A JSON array of strings, whole or not at all.
fn parse_name_list(value: &serde_json::Value) -> Option<Vec<String>> {
    parse_list(value, |v| v.as_str().map(ToOwned::to_owned))
}

fn parse_class_kind(tag: &str) -> Option<ReflectedClassKind> {
    match tag {
        "class" => Some(ReflectedClassKind::Class),
        "interface" => Some(ReflectedClassKind::Interface),
        "trait" => Some(ReflectedClassKind::Trait),
        "enum" => Some(ReflectedClassKind::Enum),
        _ => None,
    }
}

/// Visibility, whole or not at all: an unrecognized tag never guesses `public`.
fn parse_visibility(value: Option<&serde_json::Value>) -> Option<Visibility> {
    match value.and_then(serde_json::Value::as_str)? {
        "public" => Some(Visibility::Public),
        "protected" => Some(Visibility::Protected),
        "private" => Some(Visibility::Private),
        _ => None,
    }
}

fn parse_reflected_method(value: &serde_json::Value) -> Option<ReflectedMethod> {
    Some(ReflectedMethod {
        name: value.get("name").and_then(serde_json::Value::as_str)?.to_owned(),
        is_static: value.get("static").and_then(serde_json::Value::as_bool).unwrap_or(false),
        is_abstract: value.get("abstract").and_then(serde_json::Value::as_bool).unwrap_or(false),
        is_final: value.get("final").and_then(serde_json::Value::as_bool).unwrap_or(false),
        visibility: parse_visibility(value.get("visibility"))?,
        params_total: parse_count(value.get("params_total"))?,
        params_required: parse_count(value.get("params_required"))?,
        return_type: value.get("return_type").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        return_type_tentative: value
            .get("return_type_tentative")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_reflected_const(value: &serde_json::Value) -> Option<ReflectedConst> {
    Some(ReflectedConst {
        name: value.get("name").and_then(serde_json::Value::as_str)?.to_owned(),
        visibility: parse_visibility(value.get("visibility"))?,
    })
}

fn parse_reflected_property(value: &serde_json::Value) -> Option<ReflectedProperty> {
    Some(ReflectedProperty {
        name: value.get("name").and_then(serde_json::Value::as_str)?.to_owned(),
        is_static: value.get("static").and_then(serde_json::Value::as_bool).unwrap_or(false),
        visibility: parse_visibility(value.get("visibility"))?,
    })
}

/// One parameter count off a reflection reply: non-negative JSON integer, else `None`.
fn parse_count(v: Option<&serde_json::Value>) -> Option<u32> {
    v.and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

/// Interpret a `preg_compile` `result`; `Some` only for a structured
/// `{kind: "preg"}` verdict, else `None` — `widen`, unknown `status`, empty `refuses`.
#[must_use]
pub fn parse_preg_compile_result(result: &serde_json::Value) -> Option<PregCompile> {
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("preg") {
        return None;
    }
    match result.get("status").and_then(serde_json::Value::as_str)? {
        "compiles" => Some(PregCompile::Compiles),
        "refuses" => {
            // A refusal without PCRE's own words is unanswerable, not a message-less claim.
            let message = result.get("message").and_then(serde_json::Value::as_str)?;
            (!message.is_empty()).then(|| PregCompile::Refuses { message: message.to_owned() })
        }
        _ => None,
    }
}

/// Interpret a `defined` `result` as [`ConstantDefined`] (ADR-0078, issue #198),
/// else `None` — same unanswerable discipline as [`parse_preg_compile_result`].
#[must_use]
pub fn parse_defined_result(result: &serde_json::Value) -> Option<ConstantDefined> {
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("constant") {
        return None;
    }
    match result.get("status").and_then(serde_json::Value::as_str)? {
        "defined" => Some(ConstantDefined::Defined),
        "not_defined" => Some(ConstantDefined::NotDefined),
        _ => None,
    }
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

/// Whether a *stored* fold `result` replays as the engine's own answer — i.e.
/// [`parse_fold_result`] on it yields the recorded verdict rather than one of
/// its own malformed-shape widens (`unencodable value`, `throw without class`,
/// `unknown result kind`).
///
/// [`parse_fold_result`] deliberately collapses malformedness into a widen,
/// because on a live transport there is nobody better to ask. A recorded row
/// (ADR-0092 §4) is different: a malformed row is a **miss for that row** —
/// the live engine is still there to ask — so its reader needs to tell "the
/// engine answered widen" from "the bytes rotted", which this predicate does.
/// The branches mirror [`parse_fold_result`]'s exactly; a shape that parser
/// learns to read is well-formed here in the same commit.
#[must_use]
pub fn fold_result_is_well_formed(result: &serde_json::Value) -> bool {
    match result.get("kind").and_then(serde_json::Value::as_str) {
        Some("value") => parse_fold_value(result).is_some(),
        Some("throw") => result.get("class").and_then(serde_json::Value::as_str).is_some(),
        Some("widen") => true,
        _ => false,
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
        // Array **results** cross the seam since ADR-0028's 2026-08-14 amendment
        // (issue #330, superseding #41/#42); issue #327's `Fact::Shape` never applies here.
        "array" => parse_fold_array(value).map(FoldValue::Array),
        // Anything else (objects, resources) has no literal in our IR at all.
        _ => None,
    }
}

/// Decode an `{"__steins_array": [[key, value], …]}` envelope, or `None` (widen)
/// for any malformed shape. Stricter than the argument decoder: a *result* is
/// already fully built, so an absent (`null`) or duplicate key both reject (ADR-0004).
fn parse_fold_array(value: &serde_json::Value) -> Option<Vec<(FoldKey, FoldValue)>> {
    let items = value.get(ARRAY_TAG)?.as_array()?;
    let mut entries: Vec<(FoldKey, FoldValue)> = Vec::with_capacity(items.len());
    for item in items {
        let pair = item.as_array()?;
        let [key, value] = pair.as_slice() else { return None };
        let key = parse_fold_key(key)?;
        // Linear by design: budget caps entries at a few hundred; a hash set would cost more.
        if entries.iter().any(|(seen, _)| *seen == key) {
            return None;
        }
        entries.push((key, parse_fold_leaf(value)?));
    }
    Some(entries)
}

/// Decode one materialized array key: `null` (absent-key spelling) or a float widens.
fn parse_fold_key(key: &serde_json::Value) -> Option<FoldKey> {
    match key {
        serde_json::Value::Number(n) if n.is_i64() => n.as_i64().map(FoldKey::Int),
        serde_json::Value::String(s) => Some(FoldKey::Str(s.clone())),
        _ => None,
    }
}

/// Decode one value inside an envelope. Scalars arrive **bare** (float-ness kept
/// via `JSON_PRESERVE_ZERO_FRACTION`). A JSON *object* dispatches on its tag
/// ([`ARRAY_TAG`] today; `__steins_bytes` later, ADR-0080 §3.1) — else non-UTF-8 widens.
fn parse_fold_leaf(value: &serde_json::Value) -> Option<FoldValue> {
    match value {
        serde_json::Value::Null => Some(FoldValue::Null),
        serde_json::Value::Bool(b) => Some(FoldValue::Bool(*b)),
        serde_json::Value::Number(n) if n.is_i64() => n.as_i64().map(FoldValue::Int),
        serde_json::Value::Number(n) => n.as_f64().map(FoldValue::Float),
        serde_json::Value::String(s) => Some(FoldValue::Str(s.clone())),
        serde_json::Value::Object(o) if o.contains_key(ARRAY_TAG) => {
            parse_fold_array(value).map(FoldValue::Array)
        }
        // Untagged object, unknown tag, or bare JSON array (no keys possible).
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => None,
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

    /// A full `class_reflection` reply, for the parser tests below to mutate.
    fn class_reply() -> serde_json::Value {
        serde_json::json!({
            "kind": "class_reflection",
            "target": "Random\\Randomizer",
            "exists": true,
            "name": "Random\\Randomizer",
            "class_kind": "class",
            "internal": true,
            "extension": "random",
            "final": true,
            "abstract": false,
            "parent": serde_json::Value::Null,
            "interfaces": [],
            "methods": [{
                "name": "getInt",
                "static": false,
                "abstract": false,
                "final": false,
                "visibility": "public",
                "params_total": 2,
                "params_required": 2,
                "return_type": "int",
                "return_type_tentative": false,
            }],
            "constants": [],
            "properties": [{ "name": "engine", "static": false, "visibility": "public" }],
        })
    }

    #[test]
    fn reflect_class_params_carries_the_target() {
        assert_eq!(
            reflect_class_params("Random\\Randomizer"),
            serde_json::json!({ "target": "Random\\Randomizer" })
        );
    }

    #[test]
    fn a_class_reflection_reply_reads_the_whole_declaration() {
        let r = parse_class_reflection_result(&class_reply(), "x").expect("a parsed reply");
        assert_eq!(r.target, "Random\\Randomizer");
        assert!(r.exists());
        let d = r.declaration.expect("a declaration");
        assert_eq!(d.name, "Random\\Randomizer");
        assert_eq!(d.kind, ReflectedClassKind::Class);
        assert!(d.internal);
        assert_eq!(d.extension.as_deref(), Some("random"));
        assert!(d.is_final && !d.is_abstract);
        assert_eq!(d.parent, None);
        assert_eq!(d.methods.len(), 1);
        assert_eq!(d.methods[0].name, "getInt");
        assert_eq!(d.methods[0].params_total, 2);
        assert_eq!(d.methods[0].return_type.as_deref(), Some("int"));
        assert_eq!(d.methods[0].visibility, Visibility::Public);
        assert_eq!(d.properties[0].name, "engine");
    }

    #[test]
    fn a_not_found_class_is_an_answer_not_a_decline() {
        let reply = serde_json::json!({
            "kind": "class_reflection", "target": "Redis", "exists": false,
        });
        let r = parse_class_reflection_result(&reply, "Redis").expect("an answer");
        assert!(!r.exists());
        assert_eq!(r.declaration, None);
    }

    #[test]
    fn a_widen_reply_is_a_decline() {
        // Old runner or failed reflection: unanswerable, never an empty class.
        let widen = serde_json::json!({ "kind": "widen", "reason": "unknown method" });
        assert_eq!(parse_class_reflection_result(&widen, "Redis"), None);
    }

    #[test]
    fn a_declaration_parses_whole_or_not_at_all() {
        // Each mutation would otherwise yield fewer members — decline instead.
        let mut missing_member_name = class_reply();
        missing_member_name["methods"][0]
            .as_object_mut()
            .expect("a method object")
            .remove("name");
        assert_eq!(parse_class_reflection_result(&missing_member_name, "x"), None);

        let mut unknown_visibility = class_reply();
        unknown_visibility["properties"][0]["visibility"] = serde_json::json!("secret");
        assert_eq!(parse_class_reflection_result(&unknown_visibility, "x"), None);

        let mut unknown_kind = class_reply();
        unknown_kind["class_kind"] = serde_json::json!("record");
        assert_eq!(parse_class_reflection_result(&unknown_kind, "x"), None);

        let mut no_lists = class_reply();
        no_lists.as_object_mut().expect("the reply").remove("constants");
        assert_eq!(parse_class_reflection_result(&no_lists, "x"), None);

        let mut no_arity = class_reply();
        no_arity["methods"][0]["params_total"] = serde_json::Value::Null;
        assert_eq!(parse_class_reflection_result(&no_arity, "x"), None);
    }

    #[test]
    fn a_class_reflection_falls_back_to_the_asked_target() {
        let mut no_echo = class_reply();
        no_echo.as_object_mut().expect("the reply").remove("target");
        let r = parse_class_reflection_result(&no_echo, "Asked\\About").expect("a parsed reply");
        assert_eq!(r.target, "Asked\\About");
    }

    #[test]
    fn fold_params_encode_scalars_with_their_phpness() {
        let p = fold_params("strtoupper", &[FoldArg::Str("ab".to_owned())], true).expect("askable");
        assert_eq!(
            p,
            serde_json::json!({ "function": "strtoupper", "args": ["ab"], "strict": true })
        );
        // The convention is part of the params, so the two call sites ask
        // DIFFERENT questions — which is what keeps them out of one memo slot
        // and one replay-table row.
        let weak =
            fold_params("strtoupper", &[FoldArg::Str("ab".to_owned())], false).expect("askable");
        assert_ne!(weak, p, "a weak call site is a different request");
        assert_eq!(weak["strict"], serde_json::json!(false));
        // A float stays a float on the wire — `5.0`, not `5`.
        let p = fold_params("strval", &[FoldArg::Float(5.0)], true).expect("askable");
        assert_eq!(p["args"][0].as_f64(), Some(5.0));
        assert!(p["args"][0].is_f64(), "float-ness survives: {p}");
    }

    /// A non-finite float has no JSON token, so the request is **not askable**.
    ///
    /// The failure this pins is not a lost fold but a fabricated value: while
    /// this encoder substituted `null`, a weak-mode `floor(1e309)` folded to
    /// PHP's `floor(null)` answer, `0.0`, where the program itself says `INF`.
    /// Nesting is charged too — one unspellable element makes the whole array
    /// unaskable, since a shorter array is a different argument.
    #[test]
    fn a_non_finite_float_has_no_askable_request() {
        for v in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(fold_arg_to_json(&FoldArg::Float(v)), None);
            assert_eq!(fold_params("floor", &[FoldArg::Float(v)], false), None);
            let nested = FoldArg::Array(vec![(
                Some(FoldKey::Str("k".into())),
                FoldArg::Array(vec![(None, FoldArg::Float(v))]),
            )]);
            assert_eq!(fold_params("count", &[nested], true), None, "nesting is charged");
        }
        // The neighbours still travel: this refuses a token, not a type.
        assert!(fold_params("floor", &[FoldArg::Float(f64::MAX)], false).is_some());
        assert!(fold_params("floor", &[FoldArg::Float(-0.0)], false).is_some());
    }

    #[test]
    fn fold_params_encode_arrays_as_tagged_entry_lists() {
        let arg = FoldArg::Array(vec![
            (None, FoldArg::Int(1)),
            (Some(FoldKey::Str("k".into())), FoldArg::Bool(true)),
            (Some(FoldKey::Int(-3)), FoldArg::Null),
        ]);
        let p = fold_params("count", &[arg], true).expect("askable");
        assert_eq!(
            p["args"][0][ARRAY_TAG],
            serde_json::json!([[serde_json::Value::Null, 1], ["k", true], [-3, serde_json::Value::Null]])
        );
    }

    #[test]
    fn nested_array_arguments_nest_their_envelopes() {
        let inner = FoldArg::Array(vec![(None, FoldArg::Int(7))]);
        let outer = FoldArg::Array(vec![(None, inner)]);
        let p = fold_params("count", &[outer], true).expect("askable");
        assert_eq!(p["args"][0][ARRAY_TAG][0][1][ARRAY_TAG][0][1], serde_json::json!(7));
    }

    /// An `{kind:"value", type:"array"}` reply carrying an entry-list envelope.
    fn array_reply(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "kind": "value", "type": "array", "value": { ARRAY_TAG: entries } })
    }

    /// JSON `"5"` and `5` are different array keys; a string-keyed object
    /// couldn't tell them apart.
    #[test]
    fn an_array_result_decodes_int_and_string_keys_as_distinct() {
        let r = array_reply(serde_json::json!([["5", "s"], [5, "i"]]));
        assert_eq!(
            parse_fold_result(&r),
            FoldResult::Value(FoldValue::Array(vec![
                (FoldKey::Str("5".to_owned()), FoldValue::Str("s".to_owned())),
                (FoldKey::Int(5), FoldValue::Str("i".to_owned())),
            ]))
        );
    }

    /// Float-ness included, via the reply's `JSON_PRESERVE_ZERO_FRACTION`.
    #[test]
    fn an_array_result_decodes_bare_scalar_leaves() {
        let r = array_reply(serde_json::json!([[0, 1], [1, 1.5], [2, true], [3, null], [4, "x"]]));
        assert_eq!(
            parse_fold_result(&r),
            FoldResult::Value(FoldValue::Array(vec![
                (FoldKey::Int(0), FoldValue::Int(1)),
                (FoldKey::Int(1), FoldValue::Float(1.5)),
                (FoldKey::Int(2), FoldValue::Bool(true)),
                (FoldKey::Int(3), FoldValue::Null),
                (FoldKey::Int(4), FoldValue::Str("x".to_owned())),
            ]))
        );
    }

    #[test]
    fn a_nested_array_result_nests_its_envelope() {
        let inner = serde_json::json!({ ARRAY_TAG: [[0, 7]] });
        let r = array_reply(serde_json::json!([["k", inner]]));
        assert_eq!(
            parse_fold_result(&r),
            FoldResult::Value(FoldValue::Array(vec![(
                FoldKey::Str("k".to_owned()),
                FoldValue::Array(vec![(FoldKey::Int(0), FoldValue::Int(7))]),
            )]))
        );
    }

    /// The decoder's two strictnesses (ADR-0028 §2): no absent, no duplicate key.
    #[test]
    fn an_absent_or_duplicated_result_key_widens() {
        let absent = array_reply(serde_json::json!([[serde_json::Value::Null, "x"]]));
        assert_eq!(parse_fold_result(&absent), FoldResult::widen("unencodable value"));
        let dup = array_reply(serde_json::json!([[1, "a"], [1, "b"]]));
        assert_eq!(parse_fold_result(&dup), FoldResult::widen("unencodable value"));
        // A duplicate reached through the *string* spelling is the same refusal.
        let dup_str = array_reply(serde_json::json!([["k", "a"], ["k", "b"]]));
        assert_eq!(parse_fold_result(&dup_str), FoldResult::widen("unencodable value"));
        // A float key is not a PHP array key at all.
        let float_key = array_reply(serde_json::json!([[1.5, "x"]]));
        assert_eq!(parse_fold_result(&float_key), FoldResult::widen("unencodable value"));
    }

    #[test]
    fn a_malformed_array_envelope_widens() {
        // Tag missing: a bare JSON array cannot carry keys — why the envelope exists.
        let untagged = serde_json::json!({ "kind": "value", "type": "array", "value": [1, 2] });
        assert_eq!(parse_fold_result(&untagged), FoldResult::widen("unencodable value"));
        // Unknown tag (top and nested) — the extension point ADR-0080 §3.1 will fill.
        let unknown = serde_json::json!({
            "kind": "value", "type": "array", "value": { "__steins_bytes": "wA==" },
        });
        assert_eq!(parse_fold_result(&unknown), FoldResult::widen("unencodable value"));
        let nested_unknown = array_reply(serde_json::json!([[0, { "__steins_bytes": "wA==" }]]));
        assert_eq!(parse_fold_result(&nested_unknown), FoldResult::widen("unencodable value"));
        // An entry that is not a two-element pair.
        let short = array_reply(serde_json::json!([[0]]));
        assert_eq!(parse_fold_result(&short), FoldResult::widen("unencodable value"));
        let long = array_reply(serde_json::json!([[0, "a", "b"]]));
        assert_eq!(parse_fold_result(&long), FoldResult::widen("unencodable value"));
        // An empty array is not malformed — it is an empty array.
        assert_eq!(
            parse_fold_result(&array_reply(serde_json::json!([]))),
            FoldResult::Value(FoldValue::Array(vec![]))
        );
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

    /// `int_size` is the OPTIONAL env field: an old runner still answers, width unknown.
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
    fn reflection_carries_the_parameter_counts() {
        let refl = serde_json::json!({
            "kind": "reflection",
            "target": "substr",
            "function": true,
            "class_like": false,
            "return_type": "string",
            "return_type_tentative": false,
            "params_total": 3,
            "params_required": 2,
        });
        let r = parse_reflection_result(&refl, "substr").expect("reflection");
        assert_eq!(r.params_total, Some(3));
        assert_eq!(r.params_required, Some(2));
    }

    #[test]
    fn an_old_format_reflection_reply_still_parses_with_no_arity() {
        // BACK-COMPAT PIN: an old reply (no arity fields) must keep parsing.
        let old = serde_json::json!({
            "kind": "reflection",
            "target": "strlen",
            "exists": true,
            "function": true,
            "class_like": false,
            "return_type": "int",
            "return_type_tentative": false,
        });
        let r = parse_reflection_result(&old, "strlen").expect("an old reply still parses");
        assert_eq!(r.return_type.as_deref(), Some("int"));
        assert_eq!(r.params_total, None);
        assert_eq!(r.params_required, None);
        // An explicit JSON `null` (live reflection failure) reads the same way: unanswerable.
        let failed = serde_json::json!({
            "kind": "reflection",
            "target": "strlen",
            "function": true,
            "class_like": false,
            "return_type": null,
            "params_total": null,
            "params_required": null,
        });
        let r = parse_reflection_result(&failed, "strlen").expect("reflection");
        assert_eq!(r.params_total, None);
        assert_eq!(r.params_required, None);
        // The parameter list of ADR-0056 §9 rides the same back-compat rule: an old
        // reply and a live reflection failure are both unanswerable, never empty.
        assert_eq!(r.params, None);
        assert_eq!(parse_reflection_result(&old, "strlen").expect("old").params, None);
    }

    #[test]
    fn reflection_carries_the_parameter_list() {
        let refl = serde_json::json!({
            "kind": "reflection",
            "target": "preg_match",
            "function": true,
            "class_like": false,
            "return_type": "int|false",
            "params": [
                { "name": "pattern", "type": "string", "by_ref": false, "variadic": false, "optional": false },
                { "name": "subject", "type": "string", "by_ref": false, "variadic": false, "optional": false },
                { "name": "matches", "type": null, "by_ref": true, "variadic": false, "optional": true },
            ],
        });
        let p = parse_reflection_result(&refl, "preg_match").expect("reflection").params.expect("params");
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].name, "pattern");
        assert_eq!(p[0].ty.as_deref(), Some("string"));
        assert!(!p[0].by_ref && !p[0].variadic && !p[0].optional);
        // The out-parameter: untyped, by reference, optional — three declines in one row.
        assert_eq!(p[2].ty, None);
        assert!(p[2].by_ref && p[2].optional);
    }

    #[test]
    fn a_parameter_list_parses_whole_or_not_at_all() {
        // A nameless entry cannot be described, and a partial list would renumber
        // every position after the gap — so the WHOLE list is withheld.
        let broken = serde_json::json!({
            "kind": "reflection",
            "target": "f",
            "function": true,
            "class_like": false,
            "params": [{ "name": "a", "type": "int" }, { "type": "int" }],
        });
        assert_eq!(parse_reflection_result(&broken, "f").expect("reflection").params, None);
        // A zero-parameter function answers with an empty list, which is an answer.
        let nullary = serde_json::json!({
            "kind": "reflection",
            "target": "time",
            "function": true,
            "class_like": false,
            "return_type": "int",
            "params": [],
        });
        assert_eq!(parse_reflection_result(&nullary, "time").expect("reflection").params, Some(Vec::new()));
    }

    #[test]
    fn reflection_falls_back_to_the_asked_target() {
        let refl = serde_json::json!({ "kind": "reflection", "function": false, "class_like": true });
        let r = parse_reflection_result(&refl, "Countable").expect("reflection");
        assert_eq!(r.target, "Countable");
        assert_eq!(r.return_type, None);
    }

    #[test]
    fn preg_compile_params_carry_the_whole_pattern() {
        // Delimiters/modifiers travel verbatim — exactly what PCRE can refuse.
        assert_eq!(
            preg_compile_params("/a/Z"),
            serde_json::json!({ "pattern": "/a/Z" })
        );
    }

    #[test]
    fn preg_compile_result_reads_both_verdicts() {
        let ok = serde_json::json!({ "kind": "preg", "status": "compiles" });
        assert_eq!(parse_preg_compile_result(&ok), Some(PregCompile::Compiles));
        let bad = serde_json::json!({
            "kind": "preg",
            "status": "refuses",
            "message": "Compilation failed: missing closing parenthesis at offset 9",
        });
        assert_eq!(
            parse_preg_compile_result(&bad),
            Some(PregCompile::Refuses {
                message: "Compilation failed: missing closing parenthesis at offset 9".to_owned()
            })
        );
    }

    /// The zero-FP half: every non-explicit-verdict shape reads as unanswerable.
    #[test]
    fn an_unrecognized_preg_compile_reply_is_unanswerable() {
        for bad in [
            serde_json::json!({ "kind": "widen", "reason": "unknown method" }),
            serde_json::json!({ "kind": "widen", "reason": "runtime limit, not a compile refusal" }),
            serde_json::json!({ "kind": "preg" }),
            serde_json::json!({ "kind": "preg", "status": "maybe" }),
            serde_json::json!({ "kind": "preg", "status": "refuses" }),
            serde_json::json!({ "kind": "preg", "status": "refuses", "message": "" }),
            serde_json::json!({}),
            serde_json::json!(42),
        ] {
            assert_eq!(parse_preg_compile_result(&bad), None, "{bad}");
        }
    }

    #[test]
    fn defined_params_carry_the_name_verbatim() {
        // Case is NOT folded: PHP constants are case-sensitive, so `Foo` ≠ `FOO`.
        assert_eq!(defined_params("App\\Foo"), serde_json::json!({ "name": "App\\Foo" }));
        assert_ne!(defined_params("FOO"), defined_params("Foo"));
    }

    #[test]
    fn defined_result_reads_both_verdicts() {
        let yes = serde_json::json!({ "kind": "constant", "status": "defined" });
        assert_eq!(parse_defined_result(&yes), Some(ConstantDefined::Defined));
        let no = serde_json::json!({ "kind": "constant", "status": "not_defined" });
        assert_eq!(parse_defined_result(&no), Some(ConstantDefined::NotDefined));
    }

    /// Same zero-FP discipline as [`an_unrecognized_preg_compile_reply_is_unanswerable`].
    #[test]
    fn an_unrecognized_defined_reply_is_unanswerable() {
        for bad in [
            serde_json::json!({ "kind": "widen", "reason": "unknown method" }),
            serde_json::json!({ "kind": "widen", "reason": "class constants are not asked here" }),
            serde_json::json!({ "kind": "constant" }),
            serde_json::json!({ "kind": "constant", "status": "maybe" }),
            // Must not mistake a reflect-shaped reply for a defined-shaped one.
            serde_json::json!({ "kind": "reflection", "exists": false }),
            serde_json::json!({}),
            serde_json::json!(42),
        ] {
            assert_eq!(parse_defined_result(&bad), None, "{bad}");
        }
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

    /// The `type:"array"` row is a **malformed envelope** (bare JSON array, no
    /// keys) — not the old blanket refusal, lifted by ADR-0028's 2026-08-14 amendment.
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

    /// The well-formedness predicate agrees with the parser row by row: a shape
    /// the parser reads as the engine's own verdict is well-formed, and every
    /// shape it collapses into a malformed-shape widen is not (ADR-0092 §4's
    /// stored-row reader tells the two apart to re-ask the live engine).
    #[test]
    fn well_formedness_splits_answers_from_rot() {
        for good in [
            serde_json::json!({ "kind": "value", "type": "string", "value": "AB" }),
            serde_json::json!({ "kind": "throw", "class": "DivisionByZeroError" }),
            serde_json::json!({ "kind": "widen", "reason": "unknown function" }),
            // A reason-less widen still parses as a widen, so it is an answer.
            serde_json::json!({ "kind": "widen" }),
        ] {
            assert!(fold_result_is_well_formed(&good), "{good}");
        }
        for rotten in [
            serde_json::json!({}),
            serde_json::json!({ "kind": "array" }),
            serde_json::json!({ "kind": "value", "type": "array", "value": [] }),
            serde_json::json!({ "kind": "value" }),
            serde_json::json!({ "kind": "throw" }),
            serde_json::json!(42),
            serde_json::json!("garbage"),
        ] {
            assert!(!fold_result_is_well_formed(&rotten), "{rotten}");
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
