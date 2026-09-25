//! Out-parameter seeding: what a by-ref argument holds after the call — the
//! `settype` cast write at statement position (issue #595), the array out-state
//! rows of [`crate::array_out_state`] (issue #635), and the `preg_match` /
//! `preg_match_all` `$matches` shapes, which [`preg`] reads off the pattern
//! (issue #156) beside the preg flag constants and the `preg.invalid-pattern`
//! entry points.

mod preg;

pub(crate) use preg::check_preg_pattern;

// `proc_open`'s produced places live in `resource`; these keep their callers' paths.
pub(crate) use crate::resource::{apply_produced_places, seed_produced_places, stmt_produced_places};

use std::collections::HashMap;

use steins_domain::{Base, Fact, Val};
use steins_syntax::{ArgValue, CallExpr, CastTarget, CondExpr, StmtKind};

use crate::fold::Folder;
use crate::array_out_state::{array_out_rule, byref_array_shape};
use crate::asserts::guard_call_line;
use crate::builtin_returns::transfer_declaration_admits;
use crate::coerce::{php_cast_fact, settype_cast_target};
use crate::cx::Cx;
use crate::env::{Known, Store, Stratum};
use crate::existence::global_function_callee;
use crate::refine::collect_truthy_calls;
use crate::transfers::transfer_arg_known;
use crate::walk::WalkCx;

use preg::{preg_match_all_written_fact, preg_match_written_fact};

/// **Where** an out-parameter seed is being asked for, which is the same
/// question as *what the caller has proven about the call* (ADR-0077 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeedPosition {
    /// A call in a condition whose result this branch proved **truthy**.
    Guard,
    /// A bare call **statement**, where all the caller proved is that control
    /// reached the next statement — i.e. that the call returned at all.
    Statement,
}

impl SeedPosition {
    /// Whether a catalog witness is discharged at this position.
    ///
    /// [`WrittenWhen::CallReturns`] is strictly stronger than
    /// [`WrittenWhen::ReturnTruthy`] — a truthy return is a return — so it is
    /// admitted at the guard position too, where the branch proved more than it
    /// needs. The converse never holds: a statement proves nothing about the
    /// return value, so a truthiness witness stays a guard-only claim.
    ///
    /// [`WrittenWhen::CallReturns`]: steins_catalog::WrittenWhen::CallReturns
    /// [`WrittenWhen::ReturnTruthy`]: steins_catalog::WrittenWhen::ReturnTruthy
    fn admits(self, witness: steins_catalog::WrittenWhen) -> bool {
        use steins_catalog::WrittenWhen::{CallReturns, ReturnTruthy};
        match (self, witness) {
            (_, CallReturns) | (SeedPosition::Guard, ReturnTruthy) => true,
            (SeedPosition::Statement, ReturnTruthy) => false,
        }
    }
}

/// Seed the out-parameters of every call this branch proves returned truthy
/// (ADR-0077), in source order.
///
/// A truthy result is the callee's own witness that it performed its by-ref
/// write, and the ONLY branch where the written fact is sound — `preg_match` on
/// an uncompilable pattern returns `false` and writes nothing at all. Runs after
/// `walk_if` step 2's invalidation rebinds what it forgot (§3.4) and after the
/// branch's assert narrowings, so an explicit `@phpstan-assert` envelope is not
/// overwritten by a seed at a further call in the same condition.
pub(crate) fn seed_out_params(
    w: &WalkCx,
    folder: &mut dyn Folder,
    cond: &CondExpr,
    then: bool,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    let mut calls = Vec::new();
    collect_truthy_calls(cond, then, w.cx.php_minor, &mut calls);
    for call in calls {
        let seeds = out_param_seed(w, folder, call, env, store, SeedPosition::Guard);
        let line = guard_call_line(w, call);
        for (var, fact, stratum) in seeds {
            seed_out_param(&var, fact, stratum, OUT_PARAM_SEEDED, line, env, store);
        }
    }
}

/// **The statement-position out-parameter seed** (issue #595), computed from the
/// **pre-call** env and store and applied after the statement's by-ref
/// invalidation has run.
///
/// **Reading and binding are two halves on purpose.** The input a cast consumes
/// is what the variable held *before* the call, and by the time the walk reaches
/// step 4 the name is already forgotten — a reader placed there would find
/// nothing and decline every time. So this half runs on the entry env, and
/// [`apply_stmt_out_param_seeds`] binds what it computed *after* the forgetting,
/// which is how the written fact replaces the drop rather than racing it (the
/// ADR-0077 §3.4 ordering, at the statement rung).
///
/// **A bare call statement, or an assignment that does not take the name back**
/// (issue #635 widened the original issue-#595 rung).
///
/// The refusal this started as was `$v = settype($v, 'int')`: the call performs
/// its by-ref write and the assignment *then* overwrites `$v` with the call's
/// `true`, so the last word is the assignment's and a seed would state a value
/// that never existed. But that argument is about the **target**, not about
/// assignment: `$extract = array_splice($brr, 0, 0, 1)` writes `$brr` and binds
/// `$extract`, two different names, and the write is the last word on `$brr`
/// exactly as it is at a bare call statement. So the RHS of an assignment seeds
/// every out-parameter *except* the one the assignment is about to rebind.
///
/// `return`/`echo` stay out: a `return` position's seed could only be read by a
/// statement that does not run.
///
/// **A call nested inside another call's arguments stays out too** — the form
/// `assertType('string', array_shift($arr));` takes, which is why
/// `array-shift.php:15` is still unreached. The IR lowers such a call to
/// [`ArgValue::Call`], which keeps only the name's **last segment**: nothing
/// there can tell the global `array_shift` from a namespaced function of the
/// same name, and [`global_function_callee`]'s whole job is to refuse that
/// confusion. Reaching it needs a `NameRef` in `ArgValue::Call`, which is an IR
/// change and a `SCHEMA_VERSION` bump — deliberately out of this slice.
///
/// **Reading and binding are two halves on purpose.** The input a cast consumes
/// is what the variable held *before* the call, and by the time the walk reaches
/// step 4 the name is already forgotten — a reader placed there would find
/// nothing and decline every time. So this half runs on the entry env, and
/// [`apply_stmt_out_param_seeds`] binds what it computed *after* the forgetting,
/// which is how the written fact replaces the drop rather than racing it (the
/// ADR-0077 §3.4 ordering, at the statement rung).
pub(crate) fn stmt_out_param_seeds(
    w: &WalkCx,
    folder: &mut dyn Folder,
    kind: &StmtKind,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Vec<(String, Fact, Stratum, u32)> {
    let (call, rebound) = match kind {
        StmtKind::Call(call) => (call, None),
        StmtKind::Assign { var, call: Some(call), .. } => (call, Some(var.as_str())),
        _ => return Vec::new(),
    };
    out_param_seed(w, folder, call, env, store, SeedPosition::Statement)
        .into_iter()
        .filter(|(var, _, _)| Some(var.as_str()) != rebound)
        .map(|(var, fact, stratum)| (var, fact, stratum, guard_call_line(w, call)))
        .collect()
}

/// Bind what [`stmt_out_param_seeds`] computed, after the statement's by-ref
/// invalidation forgot the same names.
pub(crate) fn apply_stmt_out_param_seeds(
    seeds: Vec<(String, Fact, Stratum, u32)>,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    for (var, fact, stratum, line) in seeds {
        seed_out_param(&var, fact, stratum, OUT_PARAM_SEEDED_STMT, line, env, store);
    }
}

/// The [`Known::bound`] provenance an out-parameter seed stamps (ADR-0077), read
/// as the clause it becomes: "from `$m`, written by the guard call on this
/// branch". Both halves of the claim are in it — the fact is the callee's, and it
/// holds *here* because the branch proved the write happened.
const OUT_PARAM_SEEDED: &str = "written by the guard call on this branch";

/// [`OUT_PARAM_SEEDED`]'s statement-position twin (issue #595). The second half
/// of the claim is weaker and says so: the write holds because the call
/// *returned*, which is all a statement proves.
const OUT_PARAM_SEEDED_STMT: &str = "written by the call at this statement";

/// **The out-parameter seed** (ADR-0077): the by-ref arguments a guard call
/// proved it wrote, paired with the fact its contract determines for each.
///
/// Called only from the branch where the call's result is proven truthy. `preg_match`
/// returns `1` and assigns the success shape, `0` and assigns `[]` — and on a
/// pattern PCRE refuses to compile it returns `false` and assigns **nothing at
/// all**, leaving the caller's variable holding whatever it held (measured, PHP
/// 8.5.9). That third outcome is the absence of an assignment, not a value a fact
/// could widen to include — so truthiness is the only place any fact is sound,
/// and a seed at the call statement would manufacture one on a reachable path.
///
/// Every leg refuses **silently**: the name stays forgotten, no diagnostic is
/// produced, and nothing distinguishes a pattern this engine cannot read from one
/// it never looked at.
fn out_param_seed(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
    position_kind: SeedPosition,
) -> Vec<(String, Fact, Stratum)> {
    // A poisoned scope (ADR-0046) has already lost the right to say which name a
    // binding is — `extract()` and variable-variables can rewrite the frame the
    // seed would land in. The same gate `apply_call_asserts` applies.
    if w.scope.poisoned {
        return Vec::new();
    }
    let Some(name) = out_param_seed_callee(w.cx, call) else { return Vec::new() };
    let Some(positions) = steins_catalog::out_params(name) else { return Vec::new() };
    let mut seeds = Vec::new();
    for &position in positions {
        // The witness leg (§3.2): only a *stated* written-when seeds, and it must
        // be one this position has proven ([`SeedPosition::admits`]). Nothing is
        // inferred from the mere existence of an `out_params` row.
        let admitted = steins_catalog::out_param_written_when(name, position)
            .is_some_and(|w| position_kind.admits(w));
        if !admitted {
            continue;
        }
        // The arity leg: an argument the call never supplied was never written.
        let Some(arg) = call.args.get(position) else { continue };
        // The aliasing leg (§3.6): only a plain local variable. `$this->m`,
        // `$arr['k']` and a variable-variable all refuse, because the write may be
        // visible to callers this scope cannot see (ADR-0063 §2.3) and because
        // nothing here could name the target if it were.
        let ArgValue::Var(var) = &arg.value else { continue };
        let Some((fact, stratum)) =
            out_param_written_fact(w, folder, name, position, call, env, store)
        else {
            continue;
        };
        seeds.push((var.clone(), fact, stratum));
    }
    seeds
}

/// The builtin an out-parameter seed may consult: the name must denote the
/// **global** function ([`global_function_callee`] — a namespaced spelling or a
/// user function of the same name is a *different function*), and a call whose
/// positional mapping a named or spread argument defeated cannot say which
/// argument is which.
pub(crate) fn out_param_seed_callee<'a>(cx: &Cx, call: &'a CallExpr) -> Option<&'a str> {
    let callee = global_function_callee(cx, call)?;
    call.positional_only.then_some(callee)
}

/// The fact the callee's contract determines for the out-parameter at
/// `position`, computed from **proven arguments only** (ADR-0077 §3.3), paired
/// with the trust stratum it is bound at. Rows are dispatched by (name,
/// position) — the key the witness is indexed by.
///
/// The two preg rows bind `Asserted` (§3.3): the shape rests on a declared
/// contract plus proven inputs, never on observing a run. The `settype` row
/// carries its **input's** stratum instead, because the fact it states is the
/// input's own value put through a measured conversion — an `Asserted` phpdoc
/// input stays `Asserted`, and a `Verified` one has nothing weaker to inherit
/// (the grid itself is `Verified` engine behaviour). The issue-#635 array rows
/// carry the input's stratum for exactly that reason: each states the caller's
/// own array put through a measured rearrangement.
fn out_param_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    name: &str,
    position: usize,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    match (name.to_ascii_lowercase().as_str(), position) {
        ("preg_match", 2) => {
            preg_match_written_fact(w, folder, call, env).map(|f| (f, Stratum::Asserted))
        }
        ("preg_match_all", 2) => {
            preg_match_all_written_fact(w, folder, call, env).map(|f| (f, Stratum::Asserted))
        }
        ("settype", 0) => settype_written_fact(w, folder, call, env, store),
        (n, 0) => array_out_state_fact(w, folder, n, call, env, store),
        _ => None,
    }
}

/// **The array out-state rows** (issue #635): the sort family, the pointer
/// moves and the two queue ends, each of which rewrites argument 0 and returns
/// something that says nothing about it.
///
/// Two premises, refusing cheapest-first:
///
/// 1. **The declaration pin** (ADR-0061 §2): the running engine must still
///    declare the return the rule was written against — `true` for the twelve
///    sorts, `mixed` for the other four — *and* the arity measured at
///    `PINNED_PHP`, since neither return spelling pins which parameter is the
///    array (ADR-0064 Amendment B).
/// 2. **A plain local variable at argument 0.** Everything after that is
///    [`ArrayOutRule::written_fact`]'s business, and it does not decline: a
///    claim it cannot use falls to the floor the witness alone establishes.
///
/// **The appended values** are read for the two names that write them
/// ([`ArrayOutRule::consumes_values`]), through the same
/// [`transfer_arg_known`] ladder every other argument reader uses — so a value
/// that is abstract but known lands as its own fact, and one the walk proved
/// nothing about is the unknown floor for that entry alone.
///
/// **Stratum.** A precise answer carries the input's own, for the reason the
/// `settype` row does: it states the caller's array put through a measured
/// rearrangement, so an `Asserted` phpdoc input stays `Asserted`. An appended
/// value can only lower it further (ADR-0061 §3: a binding cannot come out more
/// trusted than what was written into it). The floor is `Verified` — it is the
/// engine's behaviour and inherits nothing from a claim that was never made.
fn array_out_state_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    name: &str,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    let rule = array_out_rule(name)?;
    if !transfer_declaration_admits(w.cx, folder, name, rule.declared, Some(rule.arity)) {
        return None;
    }
    let ArgValue::Var(var) = &call.args.first()?.value else { return None };
    let claim = out_param_input_claim(env, store, var);
    let shape = claim.as_ref().and_then(|(input, _)| byref_array_shape(input));
    let mut stratum = match &shape {
        Some(_) => claim.as_ref().expect("a shape came from a claim").1,
        None => Stratum::Verified,
    };
    let mut values = Vec::new();
    if rule.consumes_values() {
        for arg in call.args.iter().skip(1) {
            match transfer_arg_known(w.cx, folder, &arg.value, env, Some(store)) {
                Some((fact, s)) => {
                    stratum = stratum.min(s);
                    values.push(Some(fact));
                }
                None => values.push(None),
            }
        }
    }
    Some((rule.written_fact(shape.as_ref(), &values), stratum))
}

/// **What `settype($var, $type)` wrote into `$var`** (issue #595), for a call
/// whose every premise is proven — else `None`, which leaves the caller's by-ref
/// invalidation standing (the FP-safe floor).
///
/// Four premises, in the order that refuses cheapest-first:
///
/// 1. **The declaration pin** (ADR-0061 §2 through
///    [`transfer_declaration_admits`]): the running engine must still declare
///    `settype(): bool`, and — since the parameter this rule writes is declared
///    `mixed`, which pins nothing on its own — its arity must still be the
///    `(2, 2)` measured at `PINNED_PHP` (ADR-0064 Amendment B). A silent engine
///    withholds rather than being trusted.
/// 2. **A proven type string.** The second argument must resolve to a literal
///    string through the fold gate every other reader here uses
///    ([`Cx::resolve_literal`]) — an unproven variable declines. A byte string
///    that is not valid UTF-8 declines with it: it names no type php-src accepts.
/// 3. **A target the value domain can spell** ([`settype_cast_target`]):
///    `'object'` writes a `stdClass`, which is not a [`Fact`], and every
///    spelling php-src refuses raises a `ValueError` before writing anything.
/// 4. **A pre-call claim about the input** ([`out_param_input_claim`]) and a grid cell
///    for it ([`php_cast_fact`]) — **except for `'null'`** (issue #635), which
///    overwrites whatever was there and so needs no premise about it at all.
///    Measured at PHP 8.5.9 over every input class php-src can hold — `int`,
///    `float`, `string`, `bool`, `null`, `array`, `object`, a `Closure` and a
///    `resource` — the call answers `true` and writes `NULL` for all nine and
///    raises for none. So the one column with no input dependence stops
///    pretending to have one: `settype($o, 'null')` on an `object`-declared
///    parameter, whose claim this domain cannot spell, now answers `null`
///    instead of declining. It is bound `Verified` — it is the engine's own
///    behaviour and inherits nothing from a claim that was never read.
///
/// [`Cx::resolve_literal`]: crate::cx::Cx::resolve_literal
fn settype_written_fact(
    w: &WalkCx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    store: &Store,
) -> Option<(Fact, Stratum)> {
    if !transfer_declaration_admits(w.cx, folder, "settype", &["bool"], Some((2, 2))) {
        return None;
    }
    let type_arg = &call.args.get(1)?.value;
    let ArgValue::Str(spelling) = w.cx.resolve_literal(type_arg, env, w.scope.poisoned, folder)?
    else {
        return None;
    };
    let target = settype_cast_target(spelling.as_str()?)?;
    let ArgValue::Var(var) = &call.args.first()?.value else { return None };
    if target == CastTarget::Null {
        return Some((Fact::Singleton(Val::Null), Stratum::Verified));
    }
    let (input, stratum) = out_param_input_claim(env, store, var)?;
    Some((php_cast_fact(&input, target)?, stratum))
}

/// The claim this walk holds about what `$var` **holds** on entry to the call,
/// with its stratum — the cast's input.
///
/// Both lanes, in the order every fact read takes them (ADR-0037): the env value
/// fact first, then the declared-arm lane lowered as one union
/// ([`steins_contract::to_fact`], the issue-#589 `lane_claim` reading).
///
/// The lane fallback carries **one clause of its own**: a lane whose only arm is
/// `float` lowers to `float` here, where `to_fact` floors it to nothing. The
/// floor is about *slot admission* — a `float` declaration ACCEPTS an int
/// (PHPStan core semantics), so a `Fact::General { base: Float }` would reject
/// values the declaration admits. This reader asks the other question: what the
/// slot HOLDS, which for a float-declared name is a float on every path PHP can
/// reach it by (the boundary converts in coercive mode and fatals in strict).
/// That is the same claim the dump surface already renders for such a name.
fn out_param_input_claim(
    env: &HashMap<String, Known>,
    store: &Store,
    var: &str,
) -> Option<(Fact, Stratum)> {
    if let Some(known) = env.get(var)
        && let Some(fact) = &known.fact
    {
        return Some((fact.clone(), known.stratum));
    }
    let arms = store.contract_arms(var)?;
    let stratum = if arms.iter().any(|a| a.stratum == Stratum::Asserted) {
        Stratum::Asserted
    } else {
        Stratum::Verified
    };
    let union =
        steins_contract::ContractTy::Union(arms.iter().map(|a| a.ty.clone()).collect());
    let fact = steins_contract::to_fact(&union).or_else(|| match arms {
        [only] if only.ty == steins_contract::ContractTy::Base(Base::Float) => {
            Some(Fact::General { base: Base::Float, nullable: false })
        }
        _ => None,
    })?;
    Some((fact, stratum))
}

/// Bind an out-parameter seed on a branch env (ADR-0077 §3.4).
///
/// The order is fixed, seed second: [`walk_if`] has already run
/// [`cond_invalidations`] over the pre-branch env, so the name reaches this
/// forgotten and this rebinds it — no race. The statement rung reaches the same
/// arrangement by its own route ([`stmt_out_param_seeds`] reads first, then
/// [`apply_stmt_out_param_seeds`] binds after step 4's forgetting).
///
/// `stratum` is the row's, not this function's: the preg rows bind `Asserted`
/// (§3.3 — a declared contract plus proven inputs, never an observed run), the
/// `settype` row inherits its input's. Either way a seed may silence but only a
/// `Verified` one may premise a proof.
///
/// [`walk_if`]: crate::branch::walk_if
/// [`cond_invalidations`]: crate::asserts::cond_invalidations
fn seed_out_param(
    var: &str,
    fact: Fact,
    stratum: Stratum,
    bound: &str,
    line: u32,
    env: &mut HashMap<String, Known>,
    store: &mut Store,
) {
    env.insert(var.to_owned(), Known::value_strat(fact, line, Some(bound.to_owned()), stratum));
    // The callee assigned the whole variable, so the guard-derived class facts and
    // the declared-arm lane the old value earned are void — the same reasoning any
    // rebinding applies.
    store.unbind(var);
}
