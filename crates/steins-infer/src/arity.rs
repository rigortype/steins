//! Arity: `call.too-few-arguments` / `call.unknown-named-argument` against the
//! resolved declaration, and `call.printf-too-few-arguments` (ADR-0078 / issue #188)
//! against a folded format string.

use std::collections::{HashMap, HashSet};

use steins_domain::PhpStr;
use steins_syntax::{
    ArgValue, CallExpr, Callee, MethodDecl, Param, Receiver, StaticClass, Visibility,
};

use crate::cx::Cx;
use crate::env::{Known, Store};
use crate::existence::global_function_callee;
use crate::project::{Diagnostic, FnResolution};
use crate::fold::Folder;
use crate::{
    CALL_PRINTF_TOO_FEW_ARGUMENTS_ID, CALL_TOO_FEW_ARGUMENTS_ID, CALL_UNKNOWN_NAMED_ARGUMENT_ID,
};

// ---------------------------------------------------------------------------
// Arity: `call.too-few-arguments` / `call.unknown-named-argument`
// (ADR-0049 §6 / S5 — the userland arms).
//
// The verified PHP 8.5 table is ASYMMETRIC (every row `php -r`-checked): too few
// positional/named arguments to a userland target is always a fatal
// `ArgumentCountError`; too MANY to a non-variadic runs clean (extras ignored),
// never a finding (ADR-0002; `call.too-many-arguments` stays
// REGISTERED_NOT_YET_EMITTED for the internal slice, M2); an unknown named
// argument to a non-variadic is a fatal `Error`, while a variadic silently
// collects it (`fv(x: 1)` → `{"x":1}`). A named argument overwriting a positional
// (`f(1, a: 5)`) is also a fatal `Error` — a DEFERRED id, a silence leg here.
// Verified runtime precedence: **overwrite ≻ unknown-named ≻ too-few** (`f(z: 9)`
// on `f($a, $b)` throws unknown-name, not `ArgumentCountError`), honored below so
// the emitted id never misnames the consequence. Internal (builtin) targets take
// their arity from sidecar reflection, shipped with the reflect slice (M2).
//
// Provability rests on the RESOLVED TARGET's ground-truth signature: functions —
// a uniquely-indexed userland function (ADR-0049 A2 legs: not Ambiguous, not
// builtin-shadowed; conditional declaration re-dams; boot-surface homonym cleared
// via sidecar); methods/constructors/statics — under a proven-EXACT receiver, or
// under a lower-bound one whose target no override can reach.
// The general declared-receiver variant stays UNSOUND: an override may ADD optional
// parameters (`P::m(int $a)` vs `Q::m($a = 0, $b = 0)`), so `$p->m()` on a
// declared `P` holding a `Q` satisfies the runtime contract and runs — a finding
// there is a false positive, REFUSED outright (never deferred, unlike
// `phpdoc.undefined-method`). What issue #388 admits is the complement of that
// reason, not an exception to it: a **final method** cannot be overridden and a
// **final receiver class** has no descendant to hold, so no such `Q` exists and the
// signature the walk finds is the one every instance runs. Exactness reuses S2's
// gate: `new`, a `class_exact` heap object, or a textual `Class::` static; the
// lower-bound lane is a `$var` bound to a non-exact heap object (a declared
// parameter, above all) under the final guard; `$this` (membership, A1),
// `self::`/`static::`/`parent::`, `?->`, and every dynamic form are silent.
// Call-site conditions: no argument unpacking (`...` ⇒ count unproven; counting
// proven Singleton arrays is deferred); `f(...)` is not a call; named binding
// resolves case-SENSITIVELY, exactly as PHP binds parameter names.
// ---------------------------------------------------------------------------

/// A resolved arity target: the callee's parameter list (the ground-truth
/// signature) and its PHP display name for the message (`format`, `Order::pay`,
/// `Order::__construct`).
struct ArityTarget<'a> {
    params: &'a [Param],
    display: String,
}

/// The number of **required** parameters (ADR-0049 §6): the 1-based index of the
/// last parameter that is neither variadic nor default-valued. Matches PHP 8.5's
/// `ReflectionFunctionAbstract::getNumberOfRequiredParameters`, including the
/// deprecated "optional parameter declared before a required one is implicitly
/// required" shape (`f($a = 1, $b)` ⇒ 2, `php -r`-verified). A variadic is never
/// required; by-ref and promoted parameters are required exactly like any other
/// (both `php -r`-verified).
fn required_param_count(params: &[Param]) -> usize {
    let mut required = 0;
    for (i, p) in params.iter().enumerate() {
        if !p.variadic && !p.has_default {
            required = i + 1;
        }
    }
    required
}

/// The receiver an arity method/static/constructor call dispatches on.
struct ArityReceiver {
    /// The class the chain walk starts at.
    class: String,
    method: String,
    /// A textual `Class::m()` spelling — a static call to a NON-static method
    /// raises `Error: Non-static method … cannot be called statically` *before* any
    /// `ArgumentCountError` (`php -r`-verified), so the caller silences that shape
    /// rather than misnaming the consequence.
    is_static_call: bool,
    /// Whether `class` is the receiver's **exact** runtime class. A lower bound
    /// (issue #388's declared parameter) reaches the signature only through the
    /// override guard [`resolve_arity_method`] applies.
    exact: bool,
}

/// Resolve the receiver class + method name for an arity method/static/constructor
/// call, or `None` where no class is proven at all. Constructors and textual
/// `Class::` statics are exact by construction; a `$var` receiver is exact under a
/// `class_exact` heap fact and a **lower bound** without one.
fn arity_method_receiver(
    cx: &Cx,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<ArityReceiver> {
    match &call.receiver {
        Callee::Construct { class } => Some(ArityReceiver {
            class: cx.class_fqn(class),
            method: "__construct".to_owned(),
            is_static_call: false,
            exact: true,
        }),
        Callee::Method { receiver, method, nullsafe } => {
            if *nullsafe {
                return None; // `?->` excluded in v1 (S2 leg (l)).
            }
            let (class, exact) = match receiver {
                Receiver::New { class, .. } => (cx.class_fqn(class), true),
                Receiver::Var(v) => {
                    if poisoned {
                        return None;
                    }
                    let obj = store.obj_of(v)?;
                    (obj.class.clone(), obj.class_exact)
                }
                // A1: `$this` is a membership fact, never exactness — silent. Unlike
                // a declared parameter it also has no *declaration* behind it: the
                // enclosing class is where the method is being written, not a
                // contract some caller must satisfy.
                Receiver::This => return None,
                // A depth-1 property-fetch receiver carries no exact-class proof for
                // arity dispatch (ADR-0052 §7) — silent.
                Receiver::Prop { .. } => return None,
            };
            Some(ArityReceiver { class, method: method.clone(), is_static_call: false, exact })
        }
        Callee::Static { class, method } => match class {
            StaticClass::Named(name) => Some(ArityReceiver {
                class: cx.class_fqn(name),
                method: method.clone(),
                is_static_call: true,
                exact: true,
            }),
            StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static => None,
        },
        Callee::Function(_) | Callee::DynamicVar(_) | Callee::Dynamic => None,
    }
}

/// Walk `start_fqn`'s exact-receiver chain resolving `method` to its declaring
/// [`MethodDecl`] under S2's closure discipline. Returns the method, its declaring
/// class's simple name, the ordered traversed FQNs (for the A2ii homonym leg), and
/// whether any traversed class was declared conditionally (A2i). `None` on any
/// obstacle: an unresolvable/`Ambiguous`/absent class, an enum (A3 — methods not
/// lowered), a trait name or a `uses_traits` class (a trait could shadow the method
/// with a different signature), a cycle, an **abstract** or **non-public** resolved
/// method (a protected/private method may route to `__call` or raise a distinct
/// visibility `Error` — not an `ArgumentCountError`), or the method being absent
/// from the whole chain (that is S2's job, not arity's).
fn walk_arity_chain<'a>(
    cx: &Cx<'a>,
    start_fqn: &str,
    method: &str,
) -> Option<(&'a MethodDecl, String, Vec<String>, bool)> {
    let mut cur = start_fqn.to_owned();
    let mut seen: HashSet<String> = HashSet::new();
    let mut traversed: Vec<String> = Vec::new();
    let mut any_conditional = false;
    loop {
        if !seen.insert(cur.to_ascii_lowercase()) {
            return None; // cycle — closure cannot terminate soundly.
        }
        let (cfile, cd) = cx.find_class(&cur)?; // unique project class, or bust.
        if cd.is_enum || cd.is_trait || cd.uses_traits {
            return None;
        }
        traversed.push(cur.clone());
        if cd.conditional {
            any_conditional = true;
        }
        if let Some(m) = cd.methods.iter().find(|m| m.name.eq_ignore_ascii_case(method)) {
            if m.is_abstract || m.visibility != Visibility::Public {
                return None;
            }
            return Some((m, cd.name.clone(), traversed, any_conditional));
        }
        // A `None` parent ends the chain: the method is absent from the whole chain
        // — that is S2's `call.undefined-method`, never arity's id.
        cur = cx.units[cfile].tree.resolve_class_fqn(cd.parent.as_ref()?);
    }
}

/// Resolve a userland **function** call to its arity target (ADR-0049 §6 / A2
/// legs). Cheap textual resolution first, then the sidecar-backed legs.
fn resolve_arity_function<'a>(
    cx: &Cx<'a>,
    folder: &mut dyn Folder,
    call: &CallExpr,
) -> Option<ArityTarget<'a>> {
    let r = call.callee_ref.as_ref()?;
    // Unique userland function only — `Ambiguous` and builtin-shadowed both resolve
    // to `Unknown` (silent); a catalogued builtin is the internal slice (M2).
    let FnResolution::User(site) = cx.resolve_function(r) else {
        return None;
    };
    let decl = cx.fn_decl(site);
    // A9 + the A2ii homonym leg both require a live sidecar.
    if !folder.absence_family_available() {
        return None;
    }
    // A2i: a conditionally-declared function re-dams the claim.
    if decl.conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii: the resolved FQN must be answered NOT-present as a boot-surface
    // function (a homonym extension function may be the real runtime binding — the
    // `function_exists`-guarded polyfill shadowed by a loaded extension).
    match folder.boot_surface_function(&decl.fqn) {
        Some(false) => {}
        Some(true) | None => return None,
    }
    Some(ArityTarget { params: &decl.params, display: decl.name.clone() })
}

/// Resolve a method/static/constructor arity target under S2's chain closure
/// (ADR-0049 §6). Cheap textual legs first.
fn resolve_arity_method<'a>(
    cx: &Cx<'a>,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
) -> Option<ArityTarget<'a>> {
    let recv = arity_method_receiver(cx, call, store, poisoned)?;
    let start_fqn = recv.class;
    let method = recv.method;
    // `new AbstractClass()` / `new SomeInterface()` raises `Error: Cannot
    // instantiate abstract class / interface` BEFORE any `ArgumentCountError`
    // (`php -r`-verified) — silence it (would misname the consequence).
    if let Callee::Construct { .. } = &call.receiver {
        let (_, start_cd) = cx.find_class(&start_fqn)?;
        if start_cd.is_abstract || start_cd.is_interface {
            return None;
        }
    }
    let (mdecl, declaring_name, traversed, any_conditional) =
        walk_arity_chain(cx, &start_fqn, &method)?;
    // The **override guard** on a lower-bound receiver (issue #388). The §6 refusal
    // of the declared-receiver lane rests on one shape: an override may ADD optional
    // parameters, so a declared `P` holding a `Q` satisfies a signature the walk from
    // `P` never sees. `final` forecloses exactly that — a final method cannot be
    // overridden at all (PHP rejects a subclass, and a trait use, that tries: "Cannot
    // override final method"), and a final receiver class has no descendant to hold,
    // which makes it exact in everything but the bit. So the refusal stands wherever
    // its reason does, and the two shapes where the reason cannot arise are admitted:
    // the same `final_or_private` road `resolve_guarded` takes, minus `private`,
    // which `walk_arity_chain`'s public-only rule has already excluded.
    if !recv.exact
        && !mdecl.is_final
        && !cx.find_class(&start_fqn).is_some_and(|(_, cd)| cd.is_final)
    {
        return None;
    }
    // A static call (`Class::m()`) to a NON-static method raises the non-static
    // `Error` before any `ArgumentCountError` — silence it (would misname).
    if recv.is_static_call && !mdecl.is_static {
        return None;
    }
    // A9 + the A2ii homonym leg both require a live sidecar.
    if !folder.absence_family_available() {
        return None;
    }
    // A2i: a conditional class anywhere on the traversed chain re-dams the claim.
    if any_conditional && !cx.dam.is_clear() {
        return None;
    }
    // A2ii: every traversed class must be boot-surface-absent as a class-like.
    for fqn in &traversed {
        match folder.boot_surface_class_like(fqn) {
            Some(false) => {}
            Some(true) | None => return None,
        }
    }
    let display = if method.eq_ignore_ascii_case("__construct") {
        format!("{declaring_name}::__construct")
    } else {
        format!("{declaring_name}::{}", mdecl.name)
    };
    Some(ArityTarget { params: &mdecl.params, display })
}

/// The finding half: given a resolved target, apply the ordered arity checks
/// (overwrite ≻ unknown-named ≻ too-few) to one call site, honoring the verified
/// runtime precedence so the emitted id never misnames the consequence.
fn emit_arity(cx: &Cx, call: &CallExpr, target: &ArityTarget, out: &mut Vec<Diagnostic>) {
    let params = target.params;
    // Shape gates. Unpacking (or a non-canonical order) leaves the count unproven.
    if call.has_spread {
        return;
    }
    let pos = call.args.len();
    let named = &call.named_args;
    // First-class-callable `f(...)` lowers to an arg-less non-positional call — not
    // a call for arity. (Any real call is `positional_only`, or has ≥1 arg.)
    if !call.positional_only && pos == 0 && named.is_empty() {
        return;
    }

    // Overwrite guard (verified precedence #1): a named argument targeting a
    // parameter already filled by a positional argument (`f(1, a: 5)`) raises the
    // DEFERRED overwrite `Error` — silence both of our ids so neither misclaims.
    let overwrite = named
        .iter()
        .any(|n| params.iter().position(|p| p.name == n.name).is_some_and(|i| i < pos));
    if overwrite {
        return;
    }

    let has_variadic = params.iter().any(|p| p.variadic);
    // unknown-named (verified precedence #2): a named argument matching no parameter
    // of a NON-variadic target is a fatal `Error`; a variadic silently collects it.
    // Parameter-name matching is case-SENSITIVE (`f(A: 1)` on `$a` is unknown).
    if !has_variadic
        && let Some(unknown) = named.iter().find(|n| !params.iter().any(|p| p.name == n.name))
    {
        let at = cx.tree().position(call.span.start);
        out.push(Diagnostic {
            id: CALL_UNKNOWN_NAMED_ARGUMENT_ID,
            facet: None,
            fix: None,
            path: cx.path().to_owned(),
            line: at.line,
            column: at.column,
            message: format!(
                "unknown named argument ${} to {}() — no parameter ${}, provable Error",
                unknown.name, target.display, unknown.name,
            ),
        });
        return;
    }

    // too-few (verified precedence #3): a required parameter covered by neither a
    // positional argument (index < pos) nor a named argument of that name.
    let required = required_param_count(params);
    let uncovered =
        (0..required).any(|i| i >= pos && !named.iter().any(|n| n.name == params[i].name));
    if uncovered {
        let passed = pos + named.len();
        let at = cx.tree().position(call.span.start);
        out.push(Diagnostic {
            id: CALL_TOO_FEW_ARGUMENTS_ID,
            facet: None,
            fix: None,
            path: cx.path().to_owned(),
            line: at.line,
            column: at.column,
            message: format!(
                "too few arguments to {}(): {passed} passed, {required} required — provable ArgumentCountError",
                target.display,
            ),
        });
    }
}

/// Run the full ADR-0049 §6 userland arity ladder for one call and emit
/// `call.too-few-arguments` / `call.unknown-named-argument` iff every leg
/// holds. Called only from the plain per-scope pass (`descent.is_none()`),
/// so a site is judged once, never re-emitted under an interprocedural descent.
pub(crate) fn check_arity(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    store: &Store,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    let target = match &call.receiver {
        Callee::Function(_) => resolve_arity_function(cx, folder, call),
        Callee::Method { .. } | Callee::Static { .. } | Callee::Construct { .. } => {
            resolve_arity_method(cx, folder, call, store, poisoned)
        }
        Callee::DynamicVar(_) | Callee::Dynamic => None,
    };
    let Some(target) = target else {
        return;
    };
    emit_arity(cx, call, &target, out);
}

// ---------------------------------------------------------------------------
// `call.printf-too-few-arguments` (ADR-0078, issue #188).
//
// A `printf`-family call whose FOLDED LITERAL format string demands more
// placeholders than it's proven to supply is a fatal in PHP 8, same as
// `call.too-few-arguments` — but the evidence is a folded format string, not
// a resolved callee signature, so this is a distinct id, never laundered
// into the M2 internal-arity slot (`call.too-many-arguments`, still
// `REGISTERED_NOT_YET_EMITTED`).
//
// `php -r`-witnessed (PHP 8.5.9); unproven/malformed formats always decline
// (`None`, whole-format silence) rather than guess — a missed finding is safe:
//   printf("%s %s", "one")              => ArgumentCountError: 3 required, 2 given
//   sprintf("%s %s", "one")             => ArgumentCountError: 3 required, 2 given
//   fprintf(STDOUT, "%s %s", "one")     => ArgumentCountError: 4 required, 3 given
//   vprintf("%s %s", ["one"])           => ValueError: arguments array must contain 2, 1 given
//   vsprintf("%s %s", ["one"])          => ValueError: arguments array must contain 2, 1 given
//   sprintf("%s", "one", "two")         => "one" (too MANY runs clean, never a
//                                                  finding — ADR-0002/0049 §6 asymmetry)
//   sprintf("100%%")                    => "100%" (`%%` is not a placeholder)
//   sprintf("%1$s %1$s", "a")           => "a a" (positional ref is MAX not additive)
//   sprintf("%s %s %1$s", "a", "b")     => "a b a" (auto-index and positional refs
//                                                    are independent counters)
//   sprintf("%z", "x")                  => ValueError: unknown format specifier "z" (UNPROVEN)
//   sprintf("%05.2f %-10s %'x10d", 1.0) => parses fine given enough args
//   sprintf("%0$s", "x")                => ValueError: argument number specifier
//                                                  must be > 0 (`%0$` invalid)
//
// A malformed/dangling `%` makes PHP's required count diverge from a simple
// placeholder count — witnessed but deliberately not reproduced here.
// ---------------------------------------------------------------------------

/// One `printf`-family target's call shape (ADR-0078): which positional
/// argument carries the format string, and whether values arrive as
/// trailing positional arguments (`printf`/`sprintf`/`fprintf`) or a single
/// array right after the format (`vprintf`/`vsprintf`).
#[derive(Clone, Copy)]
enum PrintfShape {
    /// Format at `format_pos`; every argument after it is one value.
    Variadic { format_pos: usize },
    /// Format at `format_pos`; the array of values is the very next argument.
    Array { format_pos: usize },
}

/// Recognize a call as a `printf`-family builtin (ADR-0078 scope: `printf`,
/// `sprintf`, `fprintf`, `vprintf`, `vsprintf`), or `None` otherwise —
/// including a namespaced/aliased/userland-shadowed spelling, which
/// [`global_function_callee`] already refuses (same entry point every
/// builtin recognizer here uses, e.g. `existence_predicate`).
fn printf_family_shape(cx: &Cx, call: &CallExpr) -> Option<(&'static str, PrintfShape)> {
    let callee = global_function_callee(cx, call)?;
    let (name, shape): (&'static str, PrintfShape) = if callee.eq_ignore_ascii_case("printf") {
        ("printf", PrintfShape::Variadic { format_pos: 0 })
    } else if callee.eq_ignore_ascii_case("sprintf") {
        ("sprintf", PrintfShape::Variadic { format_pos: 0 })
    } else if callee.eq_ignore_ascii_case("fprintf") {
        ("fprintf", PrintfShape::Variadic { format_pos: 1 })
    } else if callee.eq_ignore_ascii_case("vprintf") {
        ("vprintf", PrintfShape::Array { format_pos: 0 })
    } else if callee.eq_ignore_ascii_case("vsprintf") {
        ("vsprintf", PrintfShape::Array { format_pos: 0 })
    } else {
        return None;
    };
    Some((name, shape))
}

/// The number of argument slots a `printf`-family format string demands
/// (ADR-0078): the maximum over every recognized specifier's 1-based
/// position — an explicit `%n$` position, or the next value of an
/// independent auto-increment counter for every NON-positional specifier, in
/// source order (counters are independent, never additive; see the witness
/// table above for `%s %2$s %s`).
///
/// `None` — whole-format UNPROVEN, silence — when ANY `%`-sequence fails to
/// walk to a complete recognized specifier: an unknown conversion character,
/// a dangling `%`, or an explicit position that isn't a valid 1-based index
/// (`%0$`, see witness table). `%%` is the literal-percent escape ONLY when
/// the two `%` bytes are directly adjacent.
///
/// Recognized conversion characters (PHP manual's `sprintf` spec, PHP 8.5):
/// `b c d e E f F g G h H o s u x X`.
fn printf_placeholder_count(fmt: &PhpStr) -> Option<usize> {
    // php-src walks the format byte by byte, so this reader does too — a
    // lossy decode would shift every position after an invalid byte.
    let b = fmt.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut auto = 0usize;
    let mut max_pos = 0usize;
    while i < n {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        // The `%%` literal-percent escape: no argument, only when the second
        // `%` sits immediately after the first (php -r-witnessed above).
        if i + 1 < n && b[i + 1] == b'%' {
            i += 2;
            continue;
        }
        i += 1; // past the opening '%'.

        // Optional positional prefix: digit+ immediately followed by '$'.
        let digits_start = i;
        while i < n && b[i].is_ascii_digit() {
            i += 1;
        }
        let explicit_pos = if i < n && i > digits_start && b[i] == b'$' {
            let digits = std::str::from_utf8(&b[digits_start..i]).ok()?;
            let pos: usize = digits.parse().ok()?;
            i += 1; // past '$'.
            if pos == 0 {
                // `%0$` — php -r-witnessed `ValueError`, an invalid position:
                // decline the whole format rather than guess its meaning.
                return None;
            }
            Some(pos)
        } else {
            // Not positional after all — rewind past the digits we scanned.
            i = digits_start;
            None
        };

        // Flags: `-`, `+`, ` `, `0` (any order, any repeat), or a custom pad
        // `'` + one literal pad character (PHP's own grammar: any byte may
        // follow the quote).
        loop {
            if i >= n {
                return None; // dangling `%` mid-flags — unparseable, decline.
            }
            match b[i] {
                b'-' | b'+' | b' ' | b'0' => i += 1,
                b'\'' => {
                    if i + 1 >= n {
                        return None; // quote with no pad char — dangling.
                    }
                    i += 2;
                }
                _ => break,
            }
        }
        // Width.
        while i < n && b[i].is_ascii_digit() {
            i += 1;
        }
        // Precision: `.` optionally followed by digits.
        if i < n && b[i] == b'.' {
            i += 1;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i >= n {
            return None; // dangling specifier — no type character at all.
        }
        let type_char = b[i];
        if !matches!(
            type_char,
            b'b' | b'c'
                | b'd'
                | b'e'
                | b'E'
                | b'f'
                | b'F'
                | b'g'
                | b'G'
                | b'h'
                | b'H'
                | b'o'
                | b's'
                | b'u'
                | b'x'
                | b'X'
        ) {
            // Unknown conversion character — `php -r`-witnessed `ValueError`;
            // the whole format is UNPROVEN (never guess).
            return None;
        }
        i += 1; // past the type character.

        let pos = match explicit_pos {
            Some(p) => p,
            None => {
                auto += 1;
                auto
            }
        };
        max_pos = max_pos.max(pos);
    }
    Some(max_pos)
}

/// Run the `call.printf-too-few-arguments` check (ADR-0078, issue #188) for
/// one call, emitting iff every leg holds. Called only from the plain
/// per-scope pass (mirrors [`check_arity`]'s `descent.is_none()` gating), so a
/// site is judged once, never re-emitted under an interprocedural descent.
pub(crate) fn check_printf_arity(
    cx: &Cx,
    folder: &mut dyn Folder,
    call: &CallExpr,
    env: &HashMap<String, Known>,
    poisoned: bool,
    out: &mut Vec<Diagnostic>,
) {
    // Purely positional calls only (ADR-0078 scope): `positional_only` excludes
    // both unpacking (`...$args`, runtime cardinality) and named arguments (not
    // provably bound against an effectively-variadic signature); the
    // first-class-callable shape (`sprintf(...)`) naturally has too few args.
    if !call.positional_only {
        return;
    }
    let Callee::Function(_) = &call.receiver else { return };
    let Some((name, shape)) = printf_family_shape(cx, call) else { return };

    let format_pos = match shape {
        PrintfShape::Variadic { format_pos } | PrintfShape::Array { format_pos } => format_pos,
    };
    let Some(format_arg) = call.args.get(format_pos) else { return };
    // Format string must come through the fold gate as a proven literal
    // (`Singleton` string fact) — a plain local, a global const/`define()`, or
    // a foldable concatenation all qualify, as for any proof-layer fold
    // consumer; a non-folded format is silence.
    let Some(ArgValue::Str(fmt)) = cx.resolve_literal(&format_arg.value, env, poisoned, folder)
    else {
        return;
    };
    let Some(required) = printf_placeholder_count(&fmt) else {
        return; // an unknown conversion char / malformed specifier — silence.
    };
    if required == 0 {
        return; // nothing to prove too few of.
    }

    match shape {
        PrintfShape::Variadic { format_pos } => {
            // Every argument after the format is one value; PHP counts the
            // format (and, for `fprintf`, the stream) as required too, so
            // `php_required` = `format_pos + 1` + placeholder count, and
            // `php_given` is the call's positional arg count (matches the
            // witnesses above).
            let supplied = call.args.len().saturating_sub(format_pos + 1);
            if supplied >= required {
                return;
            }
            let php_required = format_pos + 1 + required;
            let php_given = call.args.len();
            let at = cx.tree().position(call.span.start);
            out.push(Diagnostic {
                id: CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
                facet: None,
                fix: None,
                path: cx.path().to_owned(),
                line: at.line,
                column: at.column,
                message: format!(
                    "too few arguments to {name}(): format needs {required} placeholder \
                     argument(s), {supplied} supplied — provable ArgumentCountError: \
                     {php_required} arguments are required, {php_given} given",
                ),
            });
        }
        PrintfShape::Array { format_pos } => {
            // Values arrive as ONE array argument after the format. Report
            // only against a proven array of KNOWN size (ADR-0078: "unknown
            // size = silence") — an unresolved variable, non-array, or call
            // result is silence.
            let Some(array_arg) = call.args.get(format_pos + 1) else { return };
            let Some(ArgValue::Array(items)) =
                cx.resolve_literal(&array_arg.value, env, poisoned, folder)
            else {
                return;
            };
            let supplied = items.len();
            if supplied >= required {
                return;
            }
            let at = cx.tree().position(call.span.start);
            out.push(Diagnostic {
                id: CALL_PRINTF_TOO_FEW_ARGUMENTS_ID,
                facet: None,
                fix: None,
                path: cx.path().to_owned(),
                line: at.line,
                column: at.column,
                message: format!(
                    "too few arguments to {name}(): format needs {required} placeholder \
                     argument(s), array holds {supplied} — provable ValueError: The \
                     arguments array must contain {required} items, {supplied} given",
                ),
            });
        }
    }
}
