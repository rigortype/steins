//! Remembered call results (ADR-0102, slice 1): what a guard proved about a builtin
//! call's result stays true of the next identical call in the frame, until a place
//! the call names is rebound.
//!
//! ```php
//! if (strpos($h, '=') !== false) {
//!     dumpType(strpos($h, '='));   // int<0, max>, not int<0, max>|false
//! }
//! ```
//!
//! # The key
//!
//! A [`Remembered`] is held on a **call key**: the builtin's spelling and one
//! component per positional argument, each a local variable (a *place*) or a
//! scalar literal ([`key_of`]). It is structural, never textual (`strlen($s)` and
//! `strlen( $s )` are one key), and no variable can take its spelling, since it
//! contains `(`. A named or spread argument, a nested call, an operator expression, a
//! property, an element and a constant give the call no key (ADR-0102 D2). A constant
//! is refused because its spelling is not its meaning: inside a namespace `FOO` is
//! `ns\FOO` once that is defined and `\FOO` until then.
//!
//! # The gate
//!
//! A call produces a key only where three things hold, and the first is the reason
//! the others exist: **the engine's answer is a function of the key's components and
//! of nothing else the program can change.**
//!
//! 1. **The name is on [`ALLOWED`]**, a list of builtins each verified to read no
//!    ambient state at all: no locale, ini setting, environment, clock, error state,
//!    engine symbol table or stream. The catalog's `{}` row does not say this. It
//!    records that a builtin reads no *labelled* effect, and many `{}` rows read
//!    state no label names: `preg_*` consult the locale for `\w`, `strtolower` did
//!    before PHP 8.2, a float rendered to a string goes through `precision`,
//!    `json_encode` through `serialize_precision`. A remembered result that
//!    outlives a rewrite of that state is a branch the analyzer drops and PHP takes,
//!    and the catalog's `{}` is no evidence that no such state exists.
//! 2. **The call's own ADR-0099 site is exhaustive and empty** ([`gate_of`]): no gap
//!    and no label, so no argument reaches user code through the call (an object's
//!    `__toString`, a `Countable::count`).
//! 3. **The places are the frame's own** ([`is_value_place`]): every variable is
//!    passed by value, holds no object, resource or closure, is no by-reference
//!    parameter, and is not `$this`, a superglobal or `$http_response_header`. The
//!    top-level script and a property hook record no site, so they remember nothing.
//!
//! A builtin whose parameters are `string` is allowed only in a `strict_types` file
//! ([`Allowed::strict`]): a coercive call converts a float argument to a string
//! through the `precision` ini, which is ambient state.
//!
//! A setting-read row (`sprintf('%.2f', $x)`, `setlocale(LC_ALL, '0')`) is **never**
//! remembered in slice 1. Its result changes when a site rewrites the setting, and
//! the sites that can (a user function, an error handler a warning fires, an output
//! callback an `echo` fires, a tick function, a generator's caller) are not all
//! sites the effect lane records; a sound forgetting needs that posture first
//! (ADR-0102 §7). Nothing is remembered from the stat family, a `nondet.*` row, a
//! project callee or a method either.
//!
//! # Production, the consumer, forgetting
//!
//! *Production* is guard survival: [`produce`] reads the refinements a condition
//! establishes on a branch for the calls it names, by the machinery a variable's
//! refinements take. It binds a scratch variable per key to the call's own answer
//! (the builtin-call ladder, [`builtin_call_rung`]), rewrites the condition so the
//! key stands where the call stood, and applies the refinements and the
//! contract-lane subtraction to that scratch pair, so `!== false`, `=== 3`, an
//! ordering and a truthiness test narrow the key exactly as they narrow a variable.
//! What the scratch pair holds afterwards is the key's [`Lanes`].
//!
//! The *consumer* is the ladder itself ([`compose`]): a call whose key the store
//! holds answers the remembered lanes where its own rungs say less, and the
//! condition evaluator reads a remembered `Verified` finite fact as the call's
//! candidate values ([`candidates`]), so a second call decided by the first is
//! decided, and a decided guard marks its dead branch (ADR-0002).
//!
//! A key is *forgotten* by two mechanisms and survives a join only where every
//! branch holds it ([`join_remembered`]):
//!
//! 1. a place it names is rebound: [`Store::unbind`] — ADR-0070's statement-end
//!    forgetting — reaches the keys through [`Store::forget_keys_naming`], and
//!    [`forget_statement_writes`] covers the writes that never pass through it (an
//!    assignment to the place, an offset write to it);
//! 2. the condition that produced it rebinds one of its places after the call.
//!
//! No statement forgets a key for any other reason: the allowed builtins read no
//! ambient state, so nothing a callee, a handler or a callback does to the world
//! changes a result, and a callee cannot reach a local the call passes by value.
//!
//! The stratum is the refinement's: a fact the call's own row seeds `Asserted` (the
//! declared-return floor) stays `Asserted`, and a `=== literal` test pins a
//! `Verified` singleton, which is the only thing the condition evaluator reads.

use std::cell::OnceCell;
use std::collections::HashMap;

use steins_domain::Fact;
use steins_syntax::{ArgValue, CallExpr, Callee, CondExpr, CondOperand, SUPERGLOBALS, SiteKind, StmtKind};

use crate::asserts::cond_invalidations;
use crate::builtin_returns::{BuiltinRung, OptionalRungs, builtin_call_rung, floor_value_fact};
use crate::by_value::arg_is_by_value;
use crate::cx::Cx;
use crate::env::{ContractArm, Known, Store, Stratum, arg_of_val, dedup_contract_arms};
use crate::fold::Folder;
use crate::project::FnResolution;
use crate::refine::{
    Refine, apply_class_narrowing, apply_refinements, else_refinements, then_refinements,
};
use crate::site::reach::Frame;
use crate::site::{Knowledge, Lane, ResolvedSite, resolve_site};
use crate::walk::WalkCx;

/// The most keys a store holds: a guard-heavy frame stops remembering past this, so
/// the per-branch clone of the map stays cheap. A key not remembered is silence.
const MAX_KEYS: usize = 64;

/// What a [`Known::bound`] says of a fact a call key supplied.
pub(crate) const REMEMBERED_BOUND: &str = "remembered from a guard on the same call";

/// A builtin a key may be produced for, with the argument counts the verification
/// covers and whether its `string` parameters need a `strict_types` caller.
struct Allowed {
    name: &'static str,
    min_args: usize,
    max_args: usize,
    strict: bool,
}

const fn allowed(name: &'static str, min_args: usize, max_args: usize, strict: bool) -> Allowed {
    Allowed { name, min_args, max_args, strict }
}

/// The builtins that read no ambient state, each checked against php-src (8.5) and
/// witnessed to give one answer for one argument list across a `setlocale`, an
/// `ini_set`, an error handler, an output callback and a tick function:
///
/// * `strlen`, `ord`, `str_contains`, `str_starts_with`, `str_ends_with`, `strpos`,
///   `strrpos`: byte comparisons of their string arguments. `stripos` and `strripos`
///   fold case through the locale before 8.2 and are not here. A coercive caller
///   converts a float argument through `precision`, so these take `strict`.
/// * `count` of one argument: an array's element count (a `Countable` object runs
///   user code, which the site gate refuses). `COUNT_RECURSIVE` walks nested
///   arrays whose reference slots another alias can rewrite, so it is not here.
/// * `is_int`, `is_integer`, `is_long`, `is_float`, `is_double`, `is_string`,
///   `is_bool`, `is_array`, `is_null`, `is_scalar`, `is_numeric`: the zval's type tag
///   (`is_numeric` parses with the engine's own `.`-only parser, no locale).
///   `is_resource` is not here: `fclose` changes it through a by-value pass.
/// * `array_key_exists`, `array_key_first`, `array_key_last`: a hash lookup. A `null`
///   or fractional key is a deprecation, which reaches a handler but changes no
///   result.
/// * `ctype_digit`, `ctype_xdigit`: the C standard fixes both to the ASCII digits in
///   every locale (ADR-0101 §3.9).
/// * `intdiv`, `abs`: arithmetic on their arguments.
///
/// Not here, and why: `in_array` and `array_search` (an array can hold a reference
/// slot another alias rewrites between the two calls), `gettype` and `is_resource`
/// (a closed handle), every float-to-string renderer (`strval`, `implode`,
/// `json_encode`, `var_export`, `str_replace`), the case folders, `trim` and its
/// kin, `preg_*`, and `number_format`.
const ALLOWED: &[Allowed] = &[
    allowed("strlen", 1, 1, true),
    allowed("ord", 1, 1, true),
    allowed("str_contains", 2, 2, true),
    allowed("str_starts_with", 2, 2, true),
    allowed("str_ends_with", 2, 2, true),
    allowed("strpos", 2, 3, true),
    allowed("strrpos", 2, 3, true),
    allowed("count", 1, 1, false),
    allowed("is_int", 1, 1, false),
    allowed("is_integer", 1, 1, false),
    allowed("is_long", 1, 1, false),
    allowed("is_float", 1, 1, false),
    allowed("is_double", 1, 1, false),
    allowed("is_string", 1, 1, false),
    allowed("is_bool", 1, 1, false),
    allowed("is_array", 1, 1, false),
    allowed("is_null", 1, 1, false),
    allowed("is_scalar", 1, 1, false),
    allowed("is_numeric", 1, 1, false),
    allowed("array_key_exists", 2, 2, false),
    allowed("array_key_first", 1, 1, false),
    allowed("array_key_last", 1, 1, false),
    allowed("ctype_digit", 1, 1, false),
    allowed("ctype_xdigit", 1, 1, false),
    allowed("intdiv", 2, 2, false),
    allowed("abs", 1, 1, false),
];

/// The [`Allowed`] row of a resolved builtin name, if it has one.
fn allowed_row(builtin: &str) -> Option<&'static Allowed> {
    ALLOWED.iter().find(|a| a.name == builtin)
}

/// A call's result as the two lanes a variable carries it in: the value-domain fact
/// with its stratum, and the declared-contract arm list (each arm its own stratum).
/// Either may be absent; a [`Remembered`] holds at least one.
#[derive(Clone, PartialEq, Default)]
pub(crate) struct Lanes {
    pub(crate) fact: Option<(Fact, Stratum)>,
    pub(crate) arms: Option<Vec<ContractArm>>,
}

/// One remembered call result: its [`Lanes`] and the places its key names.
#[derive(Clone)]
pub(crate) struct Remembered {
    pub(crate) lanes: Lanes,
    pub(crate) places: Vec<String>,
}

/// A name no key may take as a place. `$this` and the superglobals can change
/// without a statement of this frame naming them, and `$http_response_header` is
/// written by the engine itself.
fn is_special_name(name: &str) -> bool {
    name == "this" || name == "http_response_header" || SUPERGLOBALS.contains(&name)
}

/// The key of the call `name(args)` and the places it names, or `None` where a
/// component is not a local variable or a scalar literal (ADR-0102 §2.1).
pub(crate) fn key_of(name: &str, args: &[ArgValue]) -> Option<(String, Vec<String>)> {
    let mut parts = Vec::with_capacity(args.len());
    let mut places: Vec<String> = Vec::new();
    for arg in args {
        match arg {
            ArgValue::Var(v) => {
                if is_special_name(v) {
                    return None;
                }
                parts.push(format!("${v}"));
                if !places.contains(v) {
                    places.push(v.clone());
                }
            }
            ArgValue::Int(_)
            | ArgValue::Float(_)
            | ArgValue::Str(_)
            | ArgValue::Bool(_)
            | ArgValue::Null => parts.push(arg.render()),
            _ => return None,
        }
    }
    let name = name.trim_start_matches('\\').to_ascii_lowercase();
    Some((format!("{name}({})", parts.join(", ")), places))
}

/// The function name and positional arguments of a plain call a key could stand for.
fn call_parts(call: &CallExpr) -> Option<(&str, Vec<ArgValue>)> {
    if !call.positional_only || call.has_spread || !call.named_args.is_empty() {
        return None;
    }
    let Callee::Function(name) = &call.receiver else { return None };
    Some((name.as_str(), call.args.iter().map(|a| a.value.clone()).collect()))
}

/// The gate of a resolved site (ADR-0102 §2.5): exhaustive and empty. A label, a gap,
/// a project edge or a declared bound each refuse.
fn gate_of(site: &ResolvedSite) -> bool {
    site.gaps.is_empty() && site.targets.is_empty()
}

/// The per-walk cache of the frame the effect lane reads a scope's sites against.
/// Built lazily: a frame that remembers nothing never resolves a site.
pub(crate) struct RememberCache<'a> {
    frame: OnceCell<Option<Frame<'a>>>,
}

impl Default for RememberCache<'_> {
    fn default() -> Self {
        Self { frame: OnceCell::new() }
    }
}

/// The knowledge a site is resolved with here: the effect lane's, with no plugin
/// channel. The channel colours what no row covers and leaves the gap, and a gap
/// is a refusal either way.
const EFFECTS: Knowledge<'static> = Knowledge::Catalog { lane: Lane::Effects, plugins: None };

impl<'a> RememberCache<'a> {
    /// The frame the scope's sites are read against, once.
    fn frame(&self, w: &WalkCx<'a, '_>) -> Option<&Frame<'a>> {
        self.frame
            .get_or_init(|| {
                let (class, params, sites) = w.cx.scope_site_frame(w.scope)?;
                Some(Frame::new(class, params, sites))
            })
            .as_ref()
    }

    /// Whether the call's own site is found, a plain call, and passes the gate.
    fn gate(&self, w: &WalkCx<'a, '_>, call: &CallExpr) -> bool {
        let Some(frame) = self.frame(w) else { return false };
        let Some(site) = frame.sites.iter().find(|s| {
            s.span.start == call.span.start && matches!(&s.kind, SiteKind::Call { .. })
        }) else {
            return false;
        };
        gate_of(&resolve_site(w.cx, frame, site, &EFFECTS))
    }
}

/// Forget the keys a statement's own write to a place reaches without passing
/// through [`Store::unbind`]: an assignment to the variable, an offset write,
/// append or unset on it.
pub(crate) fn forget_statement_writes(kind: &StmtKind, store: &mut Store) {
    if store.remembered.is_empty() {
        return;
    }
    match kind {
        StmtKind::Assign { var, .. } => store.forget_keys_naming(var),
        StmtKind::OffsetWrite { base, .. }
        | StmtKind::OffsetUnset { base, .. }
        | StmtKind::OffsetAppend { base, .. } => store.forget_keys_naming(base),
        _ => {}
    }
}

/// A call a condition names that may be remembered: its key, the places it names and
/// the spans it appears at.
struct Candidate {
    key: String,
    name: String,
    args: Vec<ArgValue>,
    places: Vec<String>,
    spans: Vec<u32>,
}

/// Whether `var` may stand as a place in a key: a value held in the frame alone. An
/// object, a resource or a closure can change without the name being written; a
/// by-reference parameter is another frame's variable.
fn is_value_place(w: &WalkCx, var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if store.refs.contains_key(var)
        || store.members.contains_key(var)
        || env.get(var).is_some_and(|k| k.closure.is_some())
    {
        return false;
    }
    !w.cx.scope_params(w.scope).is_some_and(|ps| ps.iter().any(|p| p.by_ref && p.name == var))
}

/// The candidate `call` is, if it is one: an [`ALLOWED`] builtin the project does not
/// shadow, called with a key's components and an argument count the allowance covers,
/// by value at every place, whose site passes the gate.
fn candidate(
    w: &WalkCx,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<Candidate> {
    let cx = w.cx;
    let (name, args) = call_parts(call)?;
    let r = call.callee_ref.as_ref()?;
    let FnResolution::Builtin(builtin) = cx.resolve_function(r) else { return None };
    if cx.index.has_simple_function(name) {
        return None;
    }
    let row = allowed_row(&builtin)?;
    if !(row.min_args..=row.max_args).contains(&args.len()) || (row.strict && !cx.strict()) {
        return None;
    }
    let (key, places) = key_of(name, &args)?;
    for (i, arg) in args.iter().enumerate() {
        if let ArgValue::Var(v) = arg

            && !(arg_is_by_value(cx, r, u32::try_from(i).ok()?) && is_value_place(w, v, env, store))
        {
            return None;
        }
    }
    if !w.remember.gate(w, call) {
        return None;
    }
    Some(Candidate { key, name: name.to_owned(), args, places, spans: vec![call.span.start] })
}

/// Collect the candidates a condition names, in source order, one per key.
fn collect(
    w: &WalkCx,
    cond: &CondExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    out: &mut Vec<Candidate>,
) {
    let note = |call: &CallExpr, out: &mut Vec<Candidate>| {
        let Some(c) = candidate(w, call, env, store) else { return };
        match out.iter_mut().find(|o| o.key == c.key) {
            Some(o) => o.spans.extend(c.spans),
            None => out.push(c),
        }
    };
    match cond {
        CondExpr::Call { call, .. } => note(call, out),
        CondExpr::Cmp { lhs, rhs, .. } => {
            for op in [lhs, rhs] {
                if let CondOperand::Other { call: Some(call), .. } = op {
                    note(call, out);
                }
            }
        }
        CondExpr::Truthy(CondOperand::Other { call: Some(call), .. }) => note(call, out),
        CondExpr::Not(c) => collect(w, c, env, store, out),
        CondExpr::And(a, b, _) | CondExpr::Or(a, b, _) => {
            collect(w, a, env, store, out);
            collect(w, b, env, store, out);
        }
        _ => {}
    }
}

/// The condition with each candidate call replaced by the key standing as a variable,
/// so the refinement machinery reads a call's guard as a variable's.
fn rewrite(cond: &CondExpr, keys: &HashMap<u32, &str>) -> CondExpr {
    let operand = |op: &CondOperand| match op {
        CondOperand::Other { call: Some(call), .. } => match keys.get(&call.span.start) {
            Some(key) => CondOperand::Var((*key).to_owned()),
            None => op.clone(),
        },
        _ => op.clone(),
    };
    match cond {
        CondExpr::Call { call, .. } => match keys.get(&call.span.start) {
            Some(key) => CondExpr::Truthy(CondOperand::Var((*key).to_owned())),
            None => cond.clone(),
        },
        CondExpr::Cmp { op, lhs, rhs } => {
            CondExpr::Cmp { op: *op, lhs: operand(lhs), rhs: operand(rhs) }
        }
        CondExpr::Truthy(op) => CondExpr::Truthy(operand(op)),
        CondExpr::Not(c) => CondExpr::Not(Box::new(rewrite(c, keys))),
        CondExpr::And(a, b, span) => {
            CondExpr::And(Box::new(rewrite(a, keys)), Box::new(rewrite(b, keys)), *span)
        }
        CondExpr::Or(a, b, span) => {
            CondExpr::Or(Box::new(rewrite(a, keys)), Box::new(rewrite(b, keys)), *span)
        }
        _ => cond.clone(),
    }
}

/// The variable a refinement narrows.
fn refine_var(r: &Refine) -> &str {
    match r {
        Refine::Exact(v, _)
        | Refine::NotNull(v)
        | Refine::Exclude(v, _)
        | Refine::IntRange(v, _)
        | Refine::Truthy(v) => v,
    }
}

/// The lanes a ladder rung answers, or `None` for a rung that binds a handle
/// (a handle is never remembered: its state is the heap's, not the key's).
fn lanes_of(rung: BuiltinRung) -> Option<Lanes> {
    Some(match rung {
        BuiltinRung::ResourceFold(..) | BuiltinRung::ResourceArms(..) => return None,
        BuiltinRung::Shape(fact, stratum) => Lanes { fact: Some((fact, stratum)), arms: None },
        BuiltinRung::Envelope(fact) => Lanes { fact: Some((fact, Stratum::Verified)), arms: None },
        BuiltinRung::Floor(arms) => Lanes {
            fact: floor_value_fact(&arms).map(|f| (f, Stratum::Asserted)),
            arms: Some(arms),
        },
        BuiltinRung::Remembered(lanes) => lanes,
    })
}

/// **Production** (ADR-0102 §2.2): record on the branch's store what the condition,
/// held on the polarity `then`, proves about the calls it names.
///
/// Each candidate becomes a scratch variable seeded with the call's own answer, the
/// condition is rewritten to name it, and the refinements the condition establishes
/// on that variable are applied to the scratch pair by the same functions that apply
/// them to a real one. What the pair holds afterwards, where it moved, is the key's
/// [`Lanes`].
///
/// The keys naming a place the condition itself may rebind are dropped at the end
/// (the condition's invalidation set, read as `walk_if` reads it): a conjunct that
/// writes `$s` after `strlen($s) > 3` has made the refinement stale.
pub(crate) fn produce(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &HashMap<String, Known>,
    store: &mut Store,
) {
    if w.scope.poisoned || store.remembered.len() >= MAX_KEYS {
        return;
    }
    let mut cands = Vec::new();
    collect(w, cond, env, store, &mut cands);
    if cands.is_empty() {
        return;
    }
    let keys: HashMap<u32, &str> =
        cands.iter().flat_map(|c| c.spans.iter().map(|s| (*s, c.key.as_str()))).collect();
    let rewritten = rewrite(cond, &keys);
    let refs: Vec<Refine> =
        if then { then_refinements(&rewritten) } else { else_refinements(&rewritten) }
            .into_iter()
            .filter(|r| refine_var(r).contains('('))
            .collect();
    cands.retain(|c| refs.iter().any(|r| refine_var(r) == c.key));
    if cands.is_empty() {
        return;
    }
    let mut senv: HashMap<String, Known> = HashMap::new();
    let mut sstore = Store::default();
    let mut seeded: HashMap<String, Lanes> = HashMap::new();
    for c in &cands {
        let rungs = OptionalRungs { resource_folds: false, resource_arms: false };
        let rung = builtin_call_rung(w.cx, folder, &c.name, &c.args, env, Some(store), false, rungs);
        let lanes = match rung {
            Some(rung) => match lanes_of(rung) {
                Some(lanes) => lanes,
                None => continue,
            },
            None => Lanes::default(),
        };
        if let Some((fact, stratum)) = lanes.fact.clone() {
            let bound = Some(REMEMBERED_BOUND.to_owned());
            senv.insert(c.key.clone(), Known::value_strat(fact, 0, bound, stratum));
        }
        if let Some(arms) = lanes.arms.clone() {
            sstore.contract.insert(c.key.clone(), arms);
        }
        seeded.insert(c.key.clone(), lanes);
    }
    apply_refinements(&refs, &mut senv, &mut sstore, Stratum::Verified);
    apply_class_narrowing(w, &rewritten, then, &mut sstore);
    for c in &cands {
        let Some(seed) = seeded.get(&c.key) else { continue };
        let learned = Lanes {
            fact: senv.get(&c.key).and_then(|k| k.fact.clone().map(|f| (f, k.stratum))),
            // A lane subtracted to nothing says no value reaches here, which is a verdict's
            // to give and not a fact's: nothing is remembered.
            arms: sstore.contract.get(&c.key).filter(|a| !a.is_empty()).cloned(),
        };
        if &learned == seed || (learned.fact.is_none() && learned.arms.is_none()) {
            continue;
        }
        store.remembered.insert(
            c.key.clone(),
            Remembered { lanes: learned, places: c.places.clone() },
        );
    }
    for v in cond_invalidations(w.cx, cond, env, store, false) {
        store.forget_keys_naming(&v);
    }
}

/// **The consumer** (ADR-0102 §2.2) at the builtin-call ladder: where the store
/// remembers the call's key, the remembered lanes answer, unless a rung that reads
/// the arguments has already folded the call to one value or the call binds a
/// handle. Every rung's answer is an over-approximation of the one value, and so
/// is the remembered one; the sharper of the two is taken whole rather than met.
pub(crate) fn compose(
    cx: &Cx,
    inner: Option<BuiltinRung>,
    name: &str,
    args: &[ArgValue],
    store: Option<&Store>,
    poisoned: bool,
) -> Option<BuiltinRung> {
    let Some(store) = store else { return inner };
    if poisoned || store.remembered.is_empty() {
        return inner;
    }
    let Some((key, _)) = key_of(name, args) else { return inner };
    let Some(remembered) = store.remembered.get(&key) else { return inner };
    if cx.index.has_simple_function(name) {
        return inner;
    }
    match &inner {
        Some(BuiltinRung::ResourceFold(..) | BuiltinRung::ResourceArms(..)) => inner,
        Some(BuiltinRung::Shape(Fact::Singleton(_), _)) => inner,
        _ => Some(BuiltinRung::Remembered(remembered.lanes.clone())),
    }
}

/// The candidate values of a condition operand that is a remembered call: the finite
/// members of a `Verified` remembered fact, which is what a decided guard may rest on
/// (an `Asserted` fact answers nothing here, as an `Asserted` binding does not).
pub(crate) fn candidates(w: &WalkCx, op: &CondOperand, store: &Store) -> Option<Vec<ArgValue>> {
    let CondOperand::Other { call: Some(call), .. } = op else { return None };
    if store.remembered.is_empty() || w.scope.poisoned {
        return None;
    }
    let (name, args) = call_parts(call)?;
    let (key, _) = key_of(name, &args)?;
    let remembered = store.remembered.get(&key)?;
    if w.cx.index.has_simple_function(name) {
        return None;
    }
    let (fact, stratum) = remembered.lanes.fact.as_ref()?;
    if *stratum != Stratum::Verified {
        return None;
    }
    fact.finite_members().map(|vals| vals.iter().map(arg_of_val).collect())
}

/// The join of two remembered lanes: the fact is their join at the weaker stratum,
/// the arms their union. A lane one side lacks is lacking in the join.
fn join_lanes(a: &Lanes, b: &Lanes) -> Option<Lanes> {
    let fact = match (&a.fact, &b.fact) {
        (Some((fa, sa)), Some((fb, sb))) => fa.join(fb).map(|f| (f, sa.min(*sb))),
        _ => None,
    };
    let arms = match (&a.arms, &b.arms) {
        (Some(x), Some(y)) => {
            let mut merged = x.clone();
            merged.extend(y.iter().cloned());
            dedup_contract_arms(&mut merged);
            Some(merged)
        }
        _ => None,
    };
    (fact.is_some() || arms.is_some()).then_some(Lanes { fact, arms })
}

/// The remembered call results after a merge (ADR-0102 §2.3): a key survives only
/// where every branch holds it, and holds the join of the branches' lanes.
pub(crate) fn join_remembered(first: &Store, rest: &[&Store]) -> HashMap<String, Remembered> {
    let mut out = HashMap::new();
    'keys: for (key, r0) in &first.remembered {
        let mut lanes = r0.lanes.clone();
        for s in rest {
            let Some(r) = s.remembered.get(key) else { continue 'keys };
            let Some(joined) = join_lanes(&lanes, &r.lanes) else { continue 'keys };
            lanes = joined;
        }
        out.insert(key.clone(), Remembered { lanes, places: r0.places.clone() });
    }
    out
}
