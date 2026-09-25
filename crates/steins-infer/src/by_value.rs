//! The by-value reading of a call's arguments (ADR-0070): which of the names a
//! statement hands to a call keep their facts through it ([`by_value_survivors`],
//! [`arg_is_by_value`]), and the trust stratum a resolved value carries
//! ([`value_stratum`]).

use std::collections::{HashMap, HashSet};

use steins_syntax::{ArgValue, Callee, InvalidatedVar, NameRef, NamedArg, Receiver};

use crate::is_dump_family_fqn;
use crate::assert_harness::ASSERT_SINK;
use crate::cx::Cx;
use crate::dump::{ASSERT_TYPE_FQN, name_reaches_global_var_dump, resolved_fn_fqn};
use crate::env::{Known, Store, Stratum};
use crate::project::FnResolution;
use crate::walk::mined_arm_admitted;

/// The variables `stmt` hands to a call whose facts nevertheless **survive** it
/// (ADR-0070) — the precise reading of the blanket `Stmt::invalidated` drop.
///
/// Evidence comes from two positions: [`Stmt::invalidated`], and a comparison
/// operand's [`CondOperand::Other`] `sites` (issue #158 — `count($a) === count($b)`
/// hands `$a` to the same by-value parameter `count($a);` does).
///
/// Survival is possible because PHP passes scalars, strings and arrays **by value**
/// (copy-on-write): the callee's parameter is a separate zval, so forgetting the
/// caller's shape is precision loss, not a soundness risk. A `&$x` parameter and an
/// object *handle* pierce that — the first is refused below; the second is admitted
/// since the 2026-08-09 amendment (issue #295) because ADR-0036 already sweeps the
/// referent's state earlier in the statement. See [`is_value_semantic`].
///
/// # The gate — all five must hold, per variable
///
/// 1. Every occurrence of the name in this statement's call arguments is a recorded
///    site on its [`steins_syntax::InvalidatedVar`] entry (an unprovable occurrence
///    makes the entry `opaque`, with no sites), and each callee resolves with a known
///    signature — project ([`Param::by_ref`]) or catalog builtin
///    ([`steins_catalog::by_value_arg`]). An unknown callee refuses. An occurrence is
///    a bare `$v` argument or a pure offset chain rooted at one (`count($v[0])` —
///    issue #609): a by-value position copies the element out and the root survives;
///    a by-ref one writes through the chain into the root's binding and condemns it.
/// 2. The argument is by value at that position (call-time pass-by-reference was
///    removed in PHP 8, so this is fixed by the declaration): a `&$x` parameter, an
///    argument past declared arity, or a variadic position refuses.
/// 3. The variable is value-semantic or a heap object handle — a closure value or a
///    bare guard-derived class bound still drops.
/// 4. The scope is not poisoned. Every aliasing/scope-injection construct (`$x = &$y`,
///    `global`, `static $x`, `$$v`, `extract`/`compact`, `eval`, `include`, a by-ref
///    `use (&$x)`) poisons the whole scope — including inside a project callee's own
///    body, closing the route by-value alone can't: reaching a caller local via
///    `global`.
/// 5. Language constructs (`isset`/`empty`/`unset`/`list`) never reach this path —
///    they aren't call nodes, so the lowering records no site.
///
/// # Read-site exceptions
///
/// A recognized dump callee — `PHPStan\dumpType` (D3) or global `var_dump` (D4) — is a
/// read that binds nothing (ADR-0053 §10 §3), exempt from conditions 1–3, keeping the
/// dump surface idempotent. Recognition is the emitters' own resolved-FQN rule
/// ([`dump_family`]), so gate and emitter can't disagree.
///
/// In the harness universe only ([`ASSERT_SINK`], installed by
/// [`collect_assert_types`]), a `PHPStan\Testing\assertType` site is the same kind of
/// read, keeping repeated-assert nsrt files honest. Not unconditional like the dumps —
/// with the sink absent, [`is_assert_read_site`] is `false` everywhere and the check
/// surface stays byte-identical (the [`emit_asserts`] pin).
///
/// # Replayability (ADR-0048)
///
/// The verdict is a pure function of the statement's recorded sites, the project
/// index, the static catalog, and the walk-local env/store — no reflection, boot
/// surface, or fold, so no per-name engine state needs memoizing.
///
/// [`CondOperand::Other`]: steins_syntax::CondOperand::Other
/// [`Param::by_ref`]: steins_syntax::Param::by_ref
/// [`dump_family`]: crate::dump::dump_family
/// [`collect_assert_types`]: crate::assert_harness::collect_assert_types
/// [`Stmt::invalidated`]: steins_syntax::Stmt::invalidated
/// [`emit_asserts`]: crate::dump::emit_asserts
pub(crate) fn by_value_survivors<'s>(
    cx: &Cx<'_>,
    poisoned: bool,
    invalidated: &'s [InvalidatedVar],
    env: &HashMap<String, Known>,
    store: &Store,
) -> (HashSet<&'s str>, HashSet<&'s str>) {
    let mut kept: HashSet<&'s str> = HashSet::new();
    // The kept names whose binding is an object handle AND that reached a real
    // by-value call site (not only a dump/assert read): the call may have run
    // userland on that object — an offset read on an `ArrayAccess` receiver is
    // `offsetGet`, a userland body (issue #637's adversarial review) — so the
    // object's mutable state must take the ADR-0036 sweep even though the name
    // itself keeps its handle. Direct object arguments are swept by the escape
    // rule already; an offset ROOT is not an argument, and this is its sweep.
    let mut object_kept: HashSet<&'s str> = HashSet::new();
    // Condition 4 (this scope's half): every scope on the ADR-0001 give-up list
    // keeps the blanket drop outright.
    if poisoned {
        return (kept, object_kept);
    }
    for entry in invalidated {
        // An opaque entry has an unprovable occurrence somewhere in the
        // statement — the lowering already discarded whatever provable sites
        // the name had, so no protection may be granted (the blanket drop).
        if entry.opaque {
            continue;
        }
        let var = entry.name.as_str();
        // Whether the name already passed the value-semantic gate (condition 3)
        // — a memo of its own, since `keep` can also record dump-read survival,
        // which never takes that gate.
        let mut sem_ok = false;
        let mut keep = false;
        let mut via_call = false;
        for (callee, position) in &entry.sites {
            // The read-site exceptions (docs above): a dump (ADR-0053) — and, in
            // the harness universe only, an `assertType` observation (oracle idea
            // B) — reads and binds nothing, so this occurrence keeps the name —
            // object bindings included — and never condemns it.
            if is_dump_read_site(cx, callee) || is_assert_read_site(cx, callee) {
                keep = true;
                continue;
            }
            // Condition 3, asked once per name and BEFORE any index work: a name
            // with an object binding refuses whatever its callees say, and a name
            // with no binding at all has nothing to save (dropping it is already a
            // no-op), so neither is worth resolving a callee for.
            if !sem_ok {
                if !is_value_semantic(var, env, store) {
                    keep = false;
                    break;
                }
                sem_ok = true;
            }
            if arg_is_by_value(cx, callee, *position) {
                keep = true;
                via_call = true;
            } else {
                // One by-ref (or unresolvable) occurrence condemns the name for
                // the whole statement, whatever its other occurrences promised.
                keep = false;
                break;
            }
        }
        if keep {
            kept.insert(var);
            if via_call && store.refs.contains_key(var) {
                object_kept.insert(var);
            }
        }
    }
    (kept, object_kept)
}

/// Whether a call-argument site's callee is a **dump-surface read** (ADR-0053):
/// the reserved `PHPStan\dumpType` pair (D3) by resolved FQN, or the global
/// `var_dump` (D4) by the PHP fallback rule — each by exactly the recognizer its
/// emitter uses ([`dump_family`]'s FQN rule, [`recognizes_var_dump`]'s name
/// core), so the survival gate and the emitters can never disagree about what a
/// dump is. See the exception paragraph on [`by_value_survivors`].
///
/// [`dump_family`]: crate::dump::dump_family
/// [`recognizes_var_dump`]: crate::dump::recognizes_var_dump
fn is_dump_read_site(cx: &Cx<'_>, r: &NameRef) -> bool {
    is_dump_family_fqn(&resolved_fn_fqn(cx, r)) || name_reaches_global_var_dump(cx, r)
}

/// Whether a call-argument site's callee is the **harness assertType read**
/// (oracle idea B): the reserved `PHPStan\Testing\assertType` FQN, recognized
/// only while the [`ASSERT_SINK`] is installed — the same condition, and the
/// same resolved-FQN rule ([`ASSERT_TYPE_FQN`]), that gates [`emit_asserts`],
/// so the survival gate and the observer can never disagree about what an
/// assertion is. With no sink (every normal check) this is `false` for every
/// site and `assertType` stays an ordinary call — the check surface is
/// byte-identical. See the exception paragraph on [`by_value_survivors`].
///
/// [`emit_asserts`]: crate::dump::emit_asserts
fn is_assert_read_site(cx: &Cx<'_>, r: &NameRef) -> bool {
    ASSERT_SINK.with(|s| s.borrow().is_some()) && resolved_fn_fqn(cx, r) == ASSERT_TYPE_FQN
}

/// Whether `var` holds a **value-semantic** binding worth saving — a scalar,
/// string, or array — rather than an object handle or nothing at all (ADR-0070
/// condition 3).
///
/// [`Fact`] has no object layer by construction, so the object question is asked
/// of the carriers that hold one: the heap handle lane, the guard-derived
/// class-bound lane, and the closure value on the binding itself.
///
/// The heap handle lane admits (ADR-0070 amendment, issue #295): a by-value call
/// cannot change an object's class or rebind the caller's variable — only its
/// mutable state, which is already invalidated earlier in the statement by the
/// ADR-0036 escape-and-sweep. Dropping the var→id link on top of that would erase
/// the allocation identity (exact class, readonly facts, generic carry) for no
/// soundness gain. The route that could rebind (`&$x`) is refused by condition 2;
/// sideways routes (`global`, `extract`, `$$v`, `eval`) by condition 4.
///
/// A guard-derived class bound alone (`Member` with no heap object) deliberately
/// does not follow it here — a separate consumer set to measure later.
///
/// A name none of the lanes mention answers `false` too, purely as a cost gate:
/// invalidating an unbound name is already a no-op.
///
/// [`Fact`]: steins_domain::Fact
fn is_value_semantic(var: &str, env: &HashMap<String, Known>, store: &Store) -> bool {
    if store.refs.contains_key(var) {
        return true;
    }
    if store.members.contains_key(var) {
        return false;
    }
    match env.get(var) {
        Some(k) => k.closure.is_none(),
        // No value binding: only a declared-arm (contract) lane is left to save.
        None => store.contract.contains_key(var),
    }
}

/// Whether one recorded site — callee reference plus 0-based argument
/// `position` — is a **by-value** argument position of a callee with a known
/// signature (ADR-0070 conditions 1 and 2). The refusing answer is `false` for
/// every uncertainty — an unresolved name, an ambiguous one, a method, an
/// argument past the declared arity.
pub(crate) fn arg_is_by_value(cx: &Cx<'_>, callee: &NameRef, position: u32) -> bool {
    let position = position as usize;
    match cx.resolve_arg_function(callee) {
        // The catalog states this name's argument semantics; `Some(true)` is the
        // only admitting answer (`None` cannot occur — it is what made the name
        // resolve to `Builtin` — but is spelled out rather than assumed). Keyed
        // by the resolved catalog name, not `callee.raw`: an aliased import
        // (`use function trim as t;`) spells the call `t`, which the catalog
        // has never heard of (issue #279).
        FnResolution::Builtin(builtin_name) => {
            steins_catalog::by_value_arg_frame(&builtin_name, position, mined_arm_admitted())
                == Some(true)
        }
        FnResolution::User(fn_site) => {
            // The declaration answers condition 2 directly, and it is the cheap
            // half — asked first so a by-ref parameter refuses without the scope
            // lookup below. A variadic position refuses: the analysis does not
            // model spread/variadic binding (v1). An argument past the declared
            // arity is `func_get_args()` territory, with nothing to read.
            match cx.fn_decl(fn_site).params.get(position) {
                Some(p) if !p.by_ref && !p.variadic => {}
                _ => return false,
            }
            // Condition 4's callee half: a body that itself defeats value
            // tracking (`global $w`, `extract`, `$$v`, `eval`) may reach the
            // caller's binding by a route argument passing does not describe.
            matches!(cx.fn_scope(fn_site), Some((_, body)) if !body.poisoned)
        }
        FnResolution::Unknown => false,
    }
}

/// The trust stratum a resolved value carries (ADR-0052 §5 derivation clause): the
/// minimum over every env/heap fact consumed while resolving `value`. A literal or
/// fully-literal subtree is `Verified`; a bare `$var` takes its env stratum; a
/// property fetch takes the prop's stratum; an array/call/ternary takes the min
/// over its parts. Stamps the derived binding with `min(inputs)`, closing the
/// laundering hazard the audit's `$pair = [$x, 99]` snippet names.
pub(crate) fn value_stratum(
    cx: &Cx<'_>,
    value: &ArgValue,
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Stratum {
    match value {
        ArgValue::Var(name) => env.get(name).map_or(Stratum::Verified, |k| k.stratum),
        // A property fetch takes its prop's stratum; with no store in scope (the
        // variable-call check) a prop fetch never resolves to a proof premise, so
        // `Verified` is the correct neutral answer.
        ArgValue::PropFetch { var, prop } => {
            store.map_or(Stratum::Verified, |s| s.prop_stratum(var, prop))
        }
        ArgValue::Array(items) => items
            .iter()
            .fold(Stratum::Verified, |acc, (_, v)| acc.min(value_stratum(cx, v, env, store))),
        ArgValue::Call(_, args) => {
            args.iter().fold(Stratum::Verified, |acc, v| acc.min(value_stratum(cx, v, env, store)))
        }
        // A method call's own arguments, plus a receiver `new`'s (issue #386): every
        // value the call consumes is a value its result derives from, and the
        // receiver's construction arguments are consumed exactly as the call's are.
        // The receiver *object*'s stratum is not read here — this seam sees no heap
        // beyond a prop fetch, and the summary that does carries its own `min`.
        ArgValue::MethodCall { callee, args, named } => {
            let recv = match callee {
                Callee::Method { receiver: Receiver::New { args, named, .. }, .. } => {
                    value_stratum_of_args(cx, args, named, env, store)
                }
                _ => Stratum::Verified,
            };
            recv.min(value_stratum_of_args(cx, args, named, env, store))
        }
        ArgValue::Ternary { then_val, else_val, .. } => {
            value_stratum(cx, then_val, env, store).min(value_stratum(cx, else_val, env, store))
        }
        // `$a ?? $b` consumes both operands' facts (a widening join): `min` (§5).
        ArgValue::Coalesce(a, b, _) => {
            value_stratum(cx, a, env, store).min(value_stratum(cx, b, env, store))
        }
        // `$a . $b` consumes both operands' facts to build one string — the same
        // derivation clause: `min`. An asserted operand must not launder itself into
        // a verified result string.
        ArgValue::Concat(a, b) => {
            value_stratum(cx, a, env, store).min(value_stratum(cx, b, env, store))
        }
        // A bare global constant (ADR-0094 §3.2). Everything this resolver
        // answers is `Verified` — true of every host the project can run on —
        // except a value fixed by the `[runtime] os` pin, which is the user's
        // claim about the deployment host and must not launder into a
        // proof-layer premise through a comparison or a concatenation.
        //
        // By the name PHP RESOLVES the reference to, which is what the `Cx` is
        // for: `use const PHP_EOL as EOL;` spells `EOL`, and matching the raw
        // spelling let the alias carry the pinned value at `Verified`.
        ArgValue::GlobalConst(r) => {
            if crate::global_consts::denotes_os_pinned_constant(cx, r) {
                Stratum::Asserted
            } else {
                Stratum::Verified
            }
        }
        _ => Stratum::Verified,
    }
}

/// The `min` stratum over one call's positional and named argument values — the
/// [`value_stratum`] derivation clause applied to an argument list.
fn value_stratum_of_args(
    cx: &Cx<'_>,
    args: &[ArgValue],
    named: &[NamedArg],
    env: &HashMap<String, Known>,
    store: Option<&Store>,
) -> Stratum {
    args.iter()
        .map(|v| value_stratum(cx, v, env, store))
        .chain(named.iter().map(|n| value_stratum(cx, &n.value, env, store)))
        .fold(Stratum::Verified, Stratum::min)
}
