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
    /// An array the engine finished building (ADR-0028's 2026-08-14 amendment,
    /// issue #330): its entries in insertion order, each with a **materialized**
    /// key.
    ///
    /// The key is a plain [`FoldKey`] rather than the argument direction's
    /// `Option<FoldKey>`, and that difference is the whole point. An argument
    /// spells an absent key so the engine assigns its own next-int; a result has
    /// no absent key left to spell, because PHP already assigned every one of
    /// them. [`parse_fold_value`] rejects the absent spelling rather than
    /// admitting it into a type this half of the wire cannot represent.
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
    /// The resident function's `ReflectionFunction::getNumberOfParameters()` — the
    /// **arity second leg** of ADR-0064's mixed-pin ruling. A rule whose name
    /// declares a bare `mixed` return (the array read-position family) has no
    /// structural declaration to countersign it, so it pins the live *signature*
    /// instead: this count must be the one the rule was written against.
    ///
    /// `None` when the name is not a resident function, when reflection failed, or
    /// when the reply came from a runner predating the field — all three are
    /// "unanswerable", and a consumer withholds its rule exactly as it does on an
    /// absent declaration. Never a guess.
    pub params_total: Option<u32>,
    /// The resident function's `ReflectionFunction::getNumberOfRequiredParameters()`,
    /// the companion of [`Self::params_total`] with the same `None` semantics.
    pub params_required: Option<u32>,
}

impl Reflection {
    /// Whether the name exists at all on the boot surface (function or class-like).
    #[must_use]
    pub fn exists(&self) -> bool {
        self.function_exists || self.class_like_exists
    }
}

/// The result of a `reflect_class(target)` request (issue #269): the **declaration**
/// the project's own PHP holds for a resident class-like, or a structured not-found.
///
/// [`Reflection`] answers *whether* a name exists; this answers *what it is*. It is
/// the class-world half of the ADR-0024 `reflect` surface, and it exists because a
/// class an installed extension provides (`Redis`, `Random\Randomizer`,
/// `Dom\Element`) has no source declaration and no builtin-catalog row — the
/// catalog carries hierarchy edges for ~350 names and no member data at all — so
/// today it is Unknown everywhere even though the engine running the project can
/// describe it completely (ADR-0049 §1: ask the real thing, never a curated stub).
///
/// `declaration == None` with a parsed reply is a **definitive not-found** on this
/// boot surface. A *failed* query is the `None` of [`parse_class_reflection_result`],
/// as everywhere else on this wire, so a consumer that forgets the third case gets
/// silence rather than a claim.
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

/// A class-like declaration as the **running engine** reports it (issue #269).
///
/// Every field is the engine's own answer about its own resident class, so it is
/// version-correct and extension-set-correct by construction — the property that
/// makes it usable where a curated list is not. It is an *envelope-grade* fact (the
/// runtime's declaration), never a proven value: see [`ClassReflection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectedClass {
    /// The declared name with the engine's own casing (`Random\Randomizer`).
    pub name: String,
    /// Class, interface, trait, or enum.
    pub kind: ReflectedClassKind,
    /// `ReflectionClass::isInternal()` — true for everything the engine or an
    /// extension declares, false for a userland class the sidecar somehow has.
    /// Part of the **origin** a consumer records beside the fact.
    pub internal: bool,
    /// `ReflectionClass::getExtensionName()` — the extension that declares it
    /// (`random`, `redis`), or `None` for a non-internal class. The other half of
    /// the origin.
    pub extension: Option<String>,
    pub is_final: bool,
    pub is_abstract: bool,
    /// The **direct** parent's name, or `None`.
    pub parent: Option<String>,
    /// `ReflectionClass::getInterfaceNames()` — the **transitive** interface set,
    /// not just the directly-implemented ones. Named as such because that is what
    /// the engine reports and re-deriving directness here would be a guess.
    pub interfaces: Vec<String>,
    /// Every method the class has, inherited ones included (the engine reports the
    /// resolved member set, which is exactly the set a call site can reach).
    pub methods: Vec<ReflectedMethod>,
    /// Class constants as *declarations*: the runner reads them off
    /// `getReflectionConstants()` and never evaluates an initializer.
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
    /// The `(string)` rendering of the declared return type, or of the *tentative*
    /// one when there is no declared one — the same discipline (and the same wire
    /// form) [`Reflection::return_type`] carries for functions.
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

/// The verdict of a `preg_compile(pattern)` request (issue #189 / ADR-0078): what
/// the **project's own PCRE** does when handed the pattern.
///
/// Steins' pattern reader (`steins_catalog::preg`) decides that a pattern is a
/// proven literal worth asking about; it never decides whether PCRE accepts it.
/// That is ADR-0004's rule — ask the real thing — and it is what keeps the
/// `preg.invalid-pattern` id off the zero-FP hazard of reporting a pattern the
/// reader dislikes but PCRE compiles happily.
///
/// There are exactly two *answers*. "Cannot answer" is not a variant: it is the
/// `None` of [`parse_preg_compile_result`], spelled the way every other unanswerable
/// reply on this wire is spelled (a `widen`), so a consumer that forgets the third
/// case gets silence rather than a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PregCompile {
    /// The engine's PCRE accepted the pattern.
    Compiles,
    /// The engine's PCRE **refused** the pattern. `message` is the engine's own
    /// complaint with PHP's `<function>(): ` prefix stripped — the probe calls
    /// `preg_match`, but the *call site* may be any `preg_*` entry point, so the
    /// prefix is the caller's to re-attach and only the PCRE half travels.
    /// Measured at PHP 8.5.9: `Compilation failed: missing closing parenthesis at
    /// offset 9`, `Delimiter must not be alphanumeric, backslash, or NUL byte`,
    /// `Unknown modifier 'Z'`, `No ending delimiter '/' found`, `Empty regular
    /// expression`.
    Refuses { message: String },
}

/// The project's own PHP's answer to `defined($name)` for a **global constant**
/// (ADR-0078, issue #198) — the existence oracle the `constant.undefined` ladder
/// ends on, for everything the runtime provides rather than the project: extension
/// constants, and constants an already-loaded bootstrap defined.
///
/// It exists because the builtin catalog is never an absence oracle (ADR-0049 §1):
/// a constant missing from a curated list proves nothing about the engine actually
/// running the project. Only the engine can say.
///
/// As with [`PregCompile`], there are exactly two *answers*; "cannot answer" is the
/// `None` of [`parse_defined_result`], so a consumer that forgets the third case
/// gets silence rather than a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantDefined {
    /// The engine has the constant — a homonym stands, so no absence claim holds.
    Defined,
    /// The engine does not have the constant.
    NotDefined,
}

/// The wire tag that marks an array, in **both** directions: an array argument
/// (issue #39) and, since ADR-0028's 2026-08-14 amendment, an array result
/// (issue #330). One tag, because the two envelopes are the same envelope.
///
/// A scalar encodes to a bare JSON scalar, so a JSON *object* on this wire can
/// only be a tagged envelope. Today the tag is therefore a readability aid and a
/// shape check rather than a disambiguator — but the result decoder dispatches on
/// it anyway, so ADR-0080 §3.1's `__steins_bytes` can join as a sibling tag
/// rather than forcing a second envelope.
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

/// The `params` of a `reflect_class` request (issue #269). The same single-`target`
/// shape `reflect` uses — one method, one question, so a replay key built for either
/// is built the same way.
#[must_use]
pub fn reflect_class_params(target: &str) -> serde_json::Value {
    serde_json::json!({ "target": target })
}

/// The `params` of a `preg_compile` request: the whole PCRE pattern *as PHP would
/// receive it* — delimiters and modifiers included, because the delimiter and the
/// modifier letters are exactly the parts PCRE can refuse.
#[must_use]
pub fn preg_compile_params(pattern: &str) -> serde_json::Value {
    serde_json::json!({ "pattern": pattern })
}

/// The `params` of a `defined` request: the constant's fully-resolved name, exactly
/// as PHP's `defined()` would receive it (`FOO`, `App\FOO`) — no leading `\`, and
/// **case as written**, since constant names are case-sensitive.
#[must_use]
pub fn defined_params(name: &str) -> serde_json::Value {
    serde_json::json!({ "name": name })
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
        // Absent (a runner predating the arity surface — including every canned
        // replay table recorded before it) or JSON `null` both map to `None`: an
        // unanswerable arity, on which a mixed-pinned rule withholds. Back-compat
        // is load-bearing here — an old reply must keep parsing, not become a
        // parse failure that would silence the reflected envelope too.
        params_total: parse_count(result.get("params_total")),
        params_required: parse_count(result.get("params_required")),
    })
}

/// Interpret a `reflect_class` `result` object for the name `target` that was asked
/// about (issue #269). Only a structured `class_reflection` reply is an answer; a
/// `widen` — a malformed request, a reflection failure inside the runner, or a
/// runner too old to implement the method — is unknown and yields `None`.
///
/// **A declaration parses whole or not at all.** A reply that says the class exists
/// but whose member lists do not read cleanly yields `None`, not a class with fewer
/// members: a consumer must never be able to mistake "we could not read the members"
/// for "the class has none". That is the same reason the runner widens on a
/// reflection failure rather than returning a half-filled declaration.
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
        // A structured not-found. An absent/odd `exists` reads as not-found too:
        // the conservative direction here is "the engine does not have it", which
        // buys no claim on its own — the caller's every use of a declaration is
        // gated on there BEING one.
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

/// Map every element of a JSON array through `one`, failing whole on any element
/// that does not read — the all-or-nothing rule of
/// [`parse_class_reflection_result`].
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

/// Visibility, whole or not at all: an unrecognized tag is a reply we do not
/// understand, and guessing `public` would widen a member's reach by fiat.
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

/// One parameter count off a reflection reply: a non-negative JSON integer, or
/// `None` for absent / null / anything that is not one.
fn parse_count(v: Option<&serde_json::Value>) -> Option<u32> {
    v.and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

/// Interpret a `preg_compile` `result` object. `Some` only for a structured
/// `{kind: "preg"}` verdict; **everything else is `None`** — a `widen` (a malformed
/// request, a runner too old to implement the method, a `false` return the runner
/// could not attribute to a compile refusal), an unknown `status`, or a `refuses`
/// carrying no message.
///
/// `None` means *unanswerable*, and the caller's only sound reading of it is
/// silence. A refusal is the sole thing this parser will ever assert, and only on a
/// reply that says so in as many words.
#[must_use]
pub fn parse_preg_compile_result(result: &serde_json::Value) -> Option<PregCompile> {
    if result.get("kind").and_then(serde_json::Value::as_str) != Some("preg") {
        return None;
    }
    match result.get("status").and_then(serde_json::Value::as_str)? {
        "compiles" => Some(PregCompile::Compiles),
        "refuses" => {
            // A refusal without PCRE's own words is not evidence we will quote, so
            // it is unanswerable rather than a message-less claim.
            let message = result.get("message").and_then(serde_json::Value::as_str)?;
            (!message.is_empty()).then(|| PregCompile::Refuses { message: message.to_owned() })
        }
        _ => None,
    }
}

/// Interpret a `defined` `result` object as a [`ConstantDefined`] (ADR-0078, issue
/// #198), or `None` when the reply is anything else — a widen (a malformed request,
/// a name the runner refused to ask about, a runner too old to implement the
/// method) or an unknown `status`.
///
/// `None` means *unanswerable*, and the caller's only sound reading of it is
/// silence — the same discipline [`parse_preg_compile_result`] and
/// [`parse_reflection_result`] carry.
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
        // An array **result** crosses the seam since ADR-0028's 2026-08-14
        // amendment (issue #330). The old boundary here cited #41/#42's
        // array-return work; that closed on the type rung, and issue #327 gave a
        // literal with an unknown slot its own `Fact::Shape` rung — which a fold
        // result never reaches, because PHP built the whole array and it lands on
        // the concrete path instead.
        "array" => parse_fold_array(value).map(FoldValue::Array),
        // Anything else (objects, resources) has no literal in our IR at all.
        _ => None,
    }
}

/// Decode an `{"__steins_array": [[key, value], …]}` envelope, or `None` for any
/// malformed shape — which widens.
///
/// # Why this decoder is stricter than the runner's argument decoder
///
/// The two envelopes are the same envelope, but they describe arrays at different
/// moments. An *argument* is an array literal as written, so it still spells the
/// things the engine has yet to decide: an absent key (`null`) awaiting PHP's
/// next-int, and a duplicate key awaiting PHP's last-wins. A *result* is an array
/// PHP has already finished building — every key materialized, every duplicate
/// resolved, normalization done — so neither spelling is reachable in one.
///
/// Both are therefore rejected rather than interpreted. Honoring a `null` key
/// would mean re-deriving here a next-int this engine did not choose, and
/// honoring a duplicate would mean choosing last-wins on the engine's behalf —
/// the exact class of Rust-reimplements-PHP error ADR-0004 exists to prevent.
/// Rejecting turns that class of runner bug into a widen instead.
fn parse_fold_array(value: &serde_json::Value) -> Option<Vec<(FoldKey, FoldValue)>> {
    let items = value.get(ARRAY_TAG)?.as_array()?;
    let mut entries: Vec<(FoldKey, FoldValue)> = Vec::with_capacity(items.len());
    for item in items {
        let pair = item.as_array()?;
        let [key, value] = pair.as_slice() else { return None };
        let key = parse_fold_key(key)?;
        // Linear, and deliberately so: the budget caps an envelope at a few
        // hundred entries, so the quadratic term is bounded by a constant and a
        // hash set would cost more than it saves.
        if entries.iter().any(|(seen, _)| *seen == key) {
            return None;
        }
        entries.push((key, parse_fold_leaf(value)?));
    }
    Some(entries)
}

/// Decode one materialized array key. A JSON `null` is the absent-key spelling an
/// argument uses and a result cannot have; a float or any other JSON shape is not
/// a PHP array key at all. Both widen.
fn parse_fold_key(key: &serde_json::Value) -> Option<FoldKey> {
    match key {
        serde_json::Value::Number(n) if n.is_i64() => n.as_i64().map(FoldKey::Int),
        serde_json::Value::String(s) => Some(FoldKey::Str(s.clone())),
        _ => None,
    }
}

/// Decode one value inside an envelope. Scalars arrive **bare** — there is no
/// per-leaf `type` tag, because JSON already separates the five PHP scalars once
/// the response preserves float-ness (`JSON_PRESERVE_ZERO_FRACTION`, which the
/// runner sets on every reply).
///
/// A JSON *object* is the extension point: it dispatches on its tag, and today
/// [`ARRAY_TAG`] is the only one. ADR-0080 §3.1's tagged byte string
/// (`__steins_bytes`, base64) is meant to arrive as a **sibling arm of this
/// match**, not as a second envelope — until it does, a non-UTF-8 string anywhere
/// in a result widens the whole result at the runner.
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
        // An untagged object, an unknown tag, or a bare JSON array (which the
        // envelope replaces precisely because a JSON array cannot carry keys).
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
        // A runner too old to implement `reflect_class`, or one whose reflection
        // failed: unanswerable, never an empty class.
        let widen = serde_json::json!({ "kind": "widen", "reason": "unknown method" });
        assert_eq!(parse_class_reflection_result(&widen, "Redis"), None);
    }

    #[test]
    fn a_declaration_parses_whole_or_not_at_all() {
        // Each mutation below would, under a lenient parser, yield a class with
        // FEWER members than it has — which a consumer could read as "the class
        // lacks that member". Every one of them must decline instead.
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
            (Some(FoldKey::Str("k".into())), FoldArg::Bool(true)),
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

    /// An `{kind:"value", type:"array"}` reply carrying an entry-list envelope.
    fn array_reply(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "kind": "value", "type": "array", "value": { ARRAY_TAG: entries } })
    }

    /// The distinction the whole envelope exists for: JSON `"5"` and JSON `5` are
    /// two different array keys, and a JSON *object* keyed by strings could not tell
    /// them apart. (PHP itself never produces the string form in a materialized
    /// array — see [`super::parse_fold_array`] — but the decoder must not be the
    /// place that erases the difference.)
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

    /// Scalars ride bare inside the envelope, each landing on its own PHP type —
    /// float-ness included, which the reply's `JSON_PRESERVE_ZERO_FRACTION` is what
    /// keeps distinguishable from an int.
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

    /// The result decoder's two strictnesses, and the ground under both (ADR-0028's
    /// 2026-08-14 amendment §2): a materialized array has neither an absent key nor
    /// a duplicate one, so admitting either spelling would mean this crate deciding
    /// a next-int or a last-wins the engine never reported. Both widen instead.
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
        // The tag is missing: a bare JSON array cannot carry keys, which is the
        // whole reason the envelope exists.
        let untagged = serde_json::json!({ "kind": "value", "type": "array", "value": [1, 2] });
        assert_eq!(parse_fold_result(&untagged), FoldResult::widen("unencodable value"));
        // An unknown tag, at the top and nested — the extension point ADR-0080 §3.1
        // will fill, refusing everything until it does.
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
        // BACK-COMPAT PIN for the absent-arity handling documented in
        // `parse_reflection_result`: an old reply must keep parsing.
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
        // An explicit JSON `null` (a reflection failure on a live runner) reads the
        // same way: unanswerable, never zero.
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
        // Delimiters and modifiers travel verbatim: they are exactly what PCRE can
        // refuse (`Unknown modifier 'Z'`, `Delimiter must not be alphanumeric`).
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

    /// The zero-FP half: every shape that is not an explicit verdict reads as
    /// unanswerable, which the consumer turns into silence.
    #[test]
    fn an_unrecognized_preg_compile_reply_is_unanswerable() {
        for bad in [
            // A runner too old to implement the method.
            serde_json::json!({ "kind": "widen", "reason": "unknown method" }),
            // The runner could not attribute the `false` to a compile refusal.
            serde_json::json!({ "kind": "widen", "reason": "runtime limit, not a compile refusal" }),
            serde_json::json!({ "kind": "preg" }),
            serde_json::json!({ "kind": "preg", "status": "maybe" }),
            // A refusal with nothing to quote is not evidence.
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
        // Case is NOT folded: PHP constant names are case-sensitive, so `Foo` and
        // `FOO` are different questions and the wire must keep them apart.
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

    /// The zero-FP half: anything that is not an explicit verdict is unanswerable,
    /// which the consumer turns into silence.
    #[test]
    fn an_unrecognized_defined_reply_is_unanswerable() {
        for bad in [
            // A runner too old to implement the method.
            serde_json::json!({ "kind": "widen", "reason": "unknown method" }),
            // The runner refused to ask about a class-constant name.
            serde_json::json!({ "kind": "widen", "reason": "class constants are not asked here" }),
            serde_json::json!({ "kind": "constant" }),
            serde_json::json!({ "kind": "constant", "status": "maybe" }),
            // The reflect reply shape must not be mistaken for this one.
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

    /// Note the `type:"array"` row: it is here as a **malformed envelope** (a bare
    /// JSON array carries no keys), not as the old blanket refusal of array results
    /// — that boundary is lifted by ADR-0028's 2026-08-14 amendment, and the
    /// well-formed case is pinned above.
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
