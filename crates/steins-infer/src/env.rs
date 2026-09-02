//! The env: values lifted into the four-layer `steins_domain::Fact` (ADR-0035), the
//! [`Store`] with its heap objects, strata and vouches, the descent summaries, and
//! the joins that merge the envs of live branches back together.

use std::collections::{HashMap, HashSet};

use steins_contract::{ContractTy, normalize};
use steins_domain::Certainty;
use steins_syntax::{ArgValue, ArrayKey, NameRef, NormKey, Scope, StaticClass, normalize_array};

use crate::fold::Folder;
use crate::fold_args::FOLD_ARRAY_MAX_DEPTH;
use crate::contract::GenericCarry;
use crate::cx::Cx;
use crate::descent::scope_class;
use crate::inaccessible::class_scope_known;
use crate::shape_projection::shape_fact;
use crate::transfers::transfer_arg_known;

// ---------------------------------------------------------------------------
// The env stores the full four-layer `steins_domain::Fact` (ADR-0035); the algebra
// lives in `steins-domain`, this crate only converts to/from the trace IR's
// `ArgValue` at the two seams below and calls the domain's joins, membership, and
// trinary queries. Stage-2 abstract facts (`Refined`/`General`) flow through the
// env like the finite layers: no *value* resolves (only `Singleton` does), but
// they carry knowledge for guard refinements (ADR-0031 stage 2), native-type
// seeding, and contract-on-fact acceptance (ADR-0030).
// ---------------------------------------------------------------------------

/// The domain value-fact (four layers), aliased as `Fact` throughout the walk.
use steins_domain::Fact;
use steins_domain::{Base, Key as VKey, PhpStr, Refinement, ShapeFact, StrPreds, Val};

/// The conversion seam **into** the domain: a literal (or fully-literal array)
/// [`ArgValue`] to a domain [`Val`]. Array keys carry PHP key-normalization in
/// insertion order (reusing [`normalize_array`], matching [`VKey`]). Any
/// non-literal element (or a non-literal `ArgValue`) yields `None` — the fact is
/// dropped (the safe side).
pub(crate) fn val_of(arg: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<Val> {
    match arg {
        ArgValue::Int(i) => Some(Val::Int(*i)),
        ArgValue::Float(f) => Some(Val::Float(*f)),
        ArgValue::Str(s) => Some(Val::Str(s.clone())),
        ArgValue::Bool(b) => Some(Val::Bool(*b)),
        ArgValue::Null => Some(Val::Null),
        ArgValue::Array(items) => {
            // An unknown minor over a literal straddling the 8.3 next-int change
            // yields `None` here (ADR-0049 A12) — the keys are unproven, so the
            // singleton fact is dropped rather than built on a guessed key.
            let normalized = normalize_array(items, php_minor)?;
            let mut out = Vec::with_capacity(normalized.len());
            for (k, v) in normalized {
                let key = match k {
                    NormKey::Int(i) => VKey::Int(i),
                    NormKey::Str(s) => VKey::Str(s),
                };
                out.push((key, val_of(&v, php_minor)?));
            }
            Some(Val::Array(out))
        }
        ArgValue::Var(_)
        | ArgValue::Call(..)
        // Like every other carrier: a method call becomes a `Val` only by way of
        // its summary, which needs the walk this seam does not have (issue #386).
        | ArgValue::MethodCall { .. }
        | ArgValue::New(..)
        | ArgValue::Ternary { .. }
        | ArgValue::Coalesce(..)
        | ArgValue::Closure(_)
        | ArgValue::PropFetch { .. }
        | ArgValue::Clone(_)
        // An offset read is never a proven `Val` — the walk judges it separately
        // (ADR-0049 §7); it manufactures no fact here (the safe side).
        | ArgValue::OffsetRead { .. }
        // Like the carriers above, a concatenation is structural: it becomes a `Val`
        // only by way of `resolve_literal`, which needs the env this seam does not see.
        | ArgValue::Concat(..)
        // Likewise a value-position comparison (issue #260): it becomes a `Val`
        // only through `resolve_literal`/`eval_binary_fact`, which have the env.
        | ArgValue::Binary { .. }
        // A value-position `isset(…)` (issue #579) is decided against the env by
        // `eval_isset_fact`, which this seam does not see — so no `Val` here.
        | ArgValue::Isset(_)
        // A logical connective or a `!` (issue #625): decided by
        // `eval_logical_fact` / `eval_not_fact`, which have the env this seam
        // lacks — so no `Val` here either.
        | ArgValue::Logical { .. }
        | ArgValue::Not(_)
        // A cast (issue #626): its operand's value is only known to the env this
        // seam does not see, so `eval_cast_fact` / `resolve_literal` decide it —
        // no `Val` here. A cast of a LITERAL arrives through `resolve_literal`
        // already folded to its result.
        | ArgValue::Cast { .. }
        // Object-world values (ADR-0043): not domain `Val`s — unproven, == Other.
        | ArgValue::ClassConst(..)
        | ArgValue::EnumCase(..)
        // A global-constant fetch (issue #168): unproven here, == Other.
        | ArgValue::GlobalConst(..)
        | ArgValue::Other => None,
    }
}

/// The conversion seam **out of** the domain: a concrete [`Val`] back to the
/// trace IR's [`ArgValue`]. Total (the domain's `Val` is exactly the concrete
/// subset of `ArgValue`), so proven-value consumers (native truth table, folding
/// args, descent binding) keep receiving an `ArgValue` as before.
pub(crate) fn arg_of_val(v: &Val) -> ArgValue {
    match v {
        Val::Int(i) => ArgValue::Int(*i),
        Val::Float(f) => ArgValue::Float(*f),
        Val::Str(s) => ArgValue::Str(s.clone()),
        Val::Bool(b) => ArgValue::Bool(*b),
        Val::Null => ArgValue::Null,
        Val::Array(items) => ArgValue::Array(
            items
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        VKey::Int(i) => ArrayKey::Int(*i),
                        VKey::Str(s) => ArrayKey::Str(s.clone()),
                    };
                    (key, arg_of_val(v))
                })
                .collect(),
        ),
    }
}

/// Render a domain [`Val`] for a message/margin **byte-for-byte** identically to
/// the existing [`ArgValue::render`] (float `5.0` form, `['a', 'b']` arrays,
/// double-quoted scalars) — it simply routes through the shared renderer, so a
/// `Singleton` fact renders exactly as its `ArgValue` always did.
pub(crate) fn render_val(v: &Val) -> String {
    arg_of_val(v).render()
}

/// A domain `Singleton` fact from a literal/array [`ArgValue`], or `None` when
/// the value is not representable (a non-literal) — the fact is then dropped.
pub(crate) fn singleton_fact(arg: &ArgValue, php_minor: Option<(u16, u16)>) -> Option<Fact> {
    val_of(arg, php_minor).map(Fact::Singleton)
}

/// The deepest nesting [`array_literal_fact`] descends into before it stops
/// resolving element facts and leaves the slot unknown (issue #327).
///
/// It exists for the same reason the fold seam's depth bound does — to keep a
/// recursive walk over a source-controlled structure off an unbounded stack —
/// and takes the same value, so the two seams refuse the same literals.
const SHAPE_SEED_MAX_DEPTH: u8 = FOLD_ARRAY_MAX_DEPTH;

/// **The fact an array literal denotes when its elements are not all proven**
/// (issue #327) — the abstract half of the seeding ladder whose concrete half
/// is [`singleton_fact`].
///
/// [`val_of`] needs a [`Val`] per element and answers `None` on the first one it
/// cannot build, dropping the fact for the whole array — key set, entry count,
/// sealing, and every proven sibling all at once. That is sound but is most of
/// the array line's precision ceiling: `['p' => 1, 'q' => $s]` knew nothing
/// where the reference implementation knows `array{p: 1, q: string}`. Nothing
/// about the keys was ever in doubt — [`normalize_array`] resolves auto indices,
/// last-wins duplicates, and the next-int rule without inspecting values — so
/// keys, count, and sealing survive an unknown element; only that element's slot
/// does not.
///
/// Callers try the concrete path first (a fully-proven literal stays a
/// `Singleton`); this is the rung below, reached only when that failed, so the
/// result is always a [`Fact::Shape`].
///
/// Refuses: a poisoned scope (today's silence, unchanged), and an unresolvable
/// key set — [`normalize_array`] declining means the literal straddles the 8.3
/// next-int change on an unpinned minor (ADR-0049 A12); a guessed key would be
/// *wrong*, not wider, so the whole literal declines as `val_of` already does.
///
/// Stratum: `min` over element facts that contributed one (ADR-0061 §3); an
/// unknown slot contributes nothing.
pub(crate) fn array_literal_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    items: &[(ArrayKey, ArgValue)],
    env: &HashMap<String, Known>,
    poisoned: bool,
    store: Option<&Store>,
) -> Option<(Fact, Stratum)> {
    array_literal_fact_within(cx, folder, items, env, poisoned, store, SHAPE_SEED_MAX_DEPTH)
}

/// The depth-carrying body of [`array_literal_fact`]. At depth zero a nested
/// literal stops being descended into and its slot is left unknown, which
/// widens the shape and never misstates it.
fn array_literal_fact_within(
    cx: &Cx,
    folder: &mut dyn Folder,
    items: &[(ArrayKey, ArgValue)],
    env: &HashMap<String, Known>,
    poisoned: bool,
    store: Option<&Store>,
    depth: u8,
) -> Option<(Fact, Stratum)> {
    if poisoned || depth == 0 {
        return None;
    }
    // A key the source did not spell as a literal (issue #336, piece 3):
    // `normalize_array` declines the whole literal since an unknown key may be an
    // integer, moving every following `Auto` position. What survives is an
    // unsealed shape whose tail key is the array-key cast of the key expressions
    // and whose tail value is the join of the element values.
    if items.iter().any(|(k, _)| matches!(k, ArrayKey::Expr(_))) {
        return open_keyed_literal_fact(cx, folder, items, env, poisoned, store, depth);
    }
    let normalized = normalize_array(items, cx.php_minor)?;
    let mut entries: Vec<(VKey, Option<Fact>)> = Vec::with_capacity(normalized.len());
    let mut stratum = Stratum::Verified;
    for (key, value) in &normalized {
        let key = match key {
            NormKey::Int(i) => VKey::Int(*i),
            NormKey::Str(s) => VKey::Str(s.clone()),
        };
        // The element's own fact, by the same ladder any other argument-position
        // value takes. A nested, only-partly-proven literal recurses here rather
        // than dropping to unknown — the cliff this function removes, one level down.
        let slot = transfer_arg_known(cx, folder, value, env, store).or_else(|| match value {
            ArgValue::Array(inner) => array_literal_fact_within(
                cx, folder, inner, env, poisoned, store, depth - 1,
            ),
            _ => None,
        });
        match slot {
            Some((fact, s)) => {
                stratum = stratum.min(s);
                entries.push((key, Some(fact)));
            }
            None => entries.push((key, None)),
        }
    }
    Some((shape_fact(ShapeFact::from_witnessed_entries(&entries)), stratum))
}

/// The fact an array literal denotes when one of its **keys** is not a literal
/// (issue #336, piece 3) — `[$k => $v]`, `['a' => 1, f() => 2]`.
///
/// The key set is gone: an unknown key may be an integer, moving the
/// next-auto-index for every following `Auto` position, so no later entry has a
/// knowable key and even the entry count is uncertain — hence
/// [`normalize_array`] declines.
///
/// What survives is what the keys and values *are*: an unsealed shape, no
/// declared fields, tail key class the **array-key cast**
/// ([`steins_domain::Fact::array_key_cast`]) of the key expressions joined, tail
/// value the join of the element values. `non_empty` holds since a literal with
/// entries has at least one. So `array_key_first([$decimalIntString => null])`
/// answers `int` — a string spelling an integer the way PHP writes one back
/// still keys an integer.
fn open_keyed_literal_fact(
    cx: &Cx,
    folder: &mut dyn Folder,
    items: &[(ArrayKey, ArgValue)],
    env: &HashMap<String, Known>,
    poisoned: bool,
    store: Option<&Store>,
    depth: u8,
) -> Option<(Fact, Stratum)> {
    use steins_domain::{KeyClass, Tail};
    let mut stratum = Stratum::Verified;
    let mut keys: Option<Fact> = None;
    let mut values: Option<Fact> = None;
    let join_into = |acc: &mut Option<Fact>, f: Fact| {
        *acc = match acc.take() {
            None => Some(f),
            Some(prev) => prev.join(&f),
        };
    };
    for (k, v) in items {
        // The key's own fact, cast to what it becomes as a key: a written literal
        // key contributes itself, an unknown one its expression's fact via the cast.
        let key_fact = match k {
            ArrayKey::Int(i) => Some(Fact::Singleton(Val::Int(*i))),
            ArrayKey::Str(s) => Some(Fact::Singleton(Val::Str(s.clone()))),
            // An `Auto` key is an integer at an unknowable position.
            ArrayKey::Auto => Some(Fact::General { base: Base::Int, nullable: false }),
            ArrayKey::Expr(e) => {
                let (f, s) = transfer_arg_known(cx, folder, e, env, store)?;
                stratum = stratum.min(s);
                f.array_key_cast()
            }
        };
        join_into(&mut keys, key_fact?);
        let value_fact = transfer_arg_known(cx, folder, v, env, store)
            .map(|(f, s)| {
                stratum = stratum.min(s);
                f
            })
            .or_else(|| match v {
                ArgValue::Array(inner) => array_literal_fact_within(
                    cx, folder, inner, env, poisoned, store, depth - 1,
                )
                .map(|(f, s)| {
                    stratum = stratum.min(s);
                    f
                }),
                _ => None,
            });
        match value_fact {
            Some(f) => join_into(&mut values, f),
            // One unknown value makes the tail's value bound unknown, and the
            // tail says what EVERY undeclared entry satisfies.
            None => values = None,
        }
    }
    // The key class the tail can hold. A two-base union has none, so it takes
    // `array-key` — the second wall #336 records.
    let key = match &keys {
        Some(Fact::General { base: Base::Int, .. } | Fact::Refined { base: Base::Int, .. }) => {
            KeyClass::Int
        }
        Some(Fact::General { base: Base::String, .. } | Fact::Refined { base: Base::String, .. }) => {
            KeyClass::Str
        }
        Some(f) => match f.finite_members() {
            Some(vals) if vals.iter().all(|v| matches!(v, Val::Int(_))) => KeyClass::Int,
            Some(vals) if vals.iter().all(|v| matches!(v, Val::Str(_))) => KeyClass::Str,
            _ => KeyClass::ArrayKey,
        },
        None => KeyClass::ArrayKey,
    };
    let shape = ShapeFact::normalize(
        Vec::new(),
        Tail::Unsealed { key, value: values.map(Box::new) },
        Certainty::Maybe,
        !items.is_empty(),
        Vec::new(),
    );
    Some((shape_fact(shape), stratum))
}

/// The value fact a `::class` magic constant produces — one seam for both of its
/// forms (issue #236, on ADR-0043's resolution and issue #36's settlement that
/// the compiler resolves `::class` and it mints nothing at runtime).
///
/// - **Written** (`Foo::class`): the FQN string literal in the source's own
///   declared casing ([`Cx::class_fqn`] preserves it) — more precise than any
///   refinement, and what PHPStan asserts for the same expression.
/// - **Relative** (`self`/`parent`/`static::class`): the `class-string`
///   refinement. [`Cx::resolve_class_const`] refuses these because the
///   class-like resolves only to the index's lowercase-normalized FQN, while
///   `::class` yields the declared casing — emitting a literal would risk a
///   wrong-case string, so the refinement is what survives instead (predicate
///   [`StrPreds::CLASS_STRING`], the contextual one).
///
/// A closure scope refuses the relative forms: [`scope_class`] answers `None`
/// there for a lexical reason ([`class_scope_known`]), and `parent::class`
/// outside a class is a compile error, not a class-string.
pub(crate) fn class_const_class_fact(cx: &Cx, scope: &Scope, sc: &StaticClass, name: &str) -> Option<Fact> {
    if !name.eq_ignore_ascii_case("class") {
        return None;
    }
    match sc {
        // The written form: ADR-0043's own resolution, which preserves the
        // source casing, so the value lane gets the LITERAL rather than the
        // refinement. Strictly more precise, and what the oracle asserts too.
        StaticClass::Named(r) => Some(Fact::Singleton(Val::Str(PhpStr::from(
            cx.class_fqn(r).trim_start_matches('\\'),
        )))),
        StaticClass::SelfKw | StaticClass::Parent | StaticClass::Static => {
            (class_scope_known(scope) && scope_class(scope).is_some()).then(|| {
                Fact::refined(Base::String, Refinement::Str(StrPreds::CLASS_STRING.close()), false)
            })
        }
    }
}

/// The target of a proven closure value (ADR-0033): an anonymous closure/arrow
/// scope (by definition-site byte offset), or a first-class callable naming a free
/// function.
#[derive(Clone)]
pub(crate) enum ClosureTarget {
    /// An anonymous closure/arrow scope addressed by its `def_offset`.
    Scope(u32),
    /// A first-class callable of a named free function (`strtolower(...)`).
    Named(NameRef),
}

/// A proven closure value carried in the env (ADR-0033). Normal value discipline
/// applies: a reassignment/invalidation drops the whole [`Known`], so the closure
/// dies exactly like any other value. The by-value capture **snapshot** is taken
/// at closure-creation time (the definition-site env), which is the semantically
/// correct PHP by-value capture — a later mutation of the captured variable does
/// not change what the closure sees.
#[derive(Clone)]
pub(crate) struct ClosureVal {
    pub(crate) target: ClosureTarget,
    /// The by-value captured variable facts, snapshotted at creation with their
    /// trust stratum (issue #128 review). Seeding the descent without the stratum
    /// would launder an `Asserted` capture into a `Verified` summary premise.
    pub(crate) captures: Vec<(String, Fact, Stratum)>,
    /// The closure definition line, for descent provenance.
    pub(crate) def_line: u32,
}

/// Whether two closure targets denote the same closure (same anonymous scope, or
/// the same named function) — for join survival.
fn closure_target_eq(a: &ClosureTarget, b: &ClosureTarget) -> bool {
    match (a, b) {
        (ClosureTarget::Scope(x), ClosureTarget::Scope(y)) => x == y,
        (ClosureTarget::Named(x), ClosureTarget::Named(y)) => x.raw == y.raw && x.kind == y.kind,
        _ => false,
    }
}

/// A capture fact reduced to a [`BindingKey`]-comparable [`ArgValue`]: a concrete
/// `Singleton` becomes its value (so a snapshot of `1` and of `"abc"` key
/// distinctly); any abstract fact collapses to `Other` (still distinct from a
/// concrete snapshot, sound for memoization).
pub(crate) fn arg_of_fact_key(fact: &Fact) -> ArgValue {
    match fact {
        Fact::Singleton(v) => arg_of_val(v),
        _ => ArgValue::Other,
    }
}

/// An allocation identity — the key the heap is stored under (ADR-0036). Fresh
/// per `new`/`clone`; a variable holds one via [`Store::refs`] (its ObjRef).
pub(crate) type AllocId = u32;

/// A property's value-domain fact together with its trust stratum (ADR-0052 §5).
/// A prop written from an `Asserted` rvalue is `Asserted`; a prop read back out
/// (`$x = $o->p`) carries the stratum forward, so an assert cannot launder into a
/// proof-layer premise through the heap (the derivation clause — heap writes).
#[derive(Clone)]
pub(crate) struct PropFact {
    pub(crate) fact: Fact,
    pub(crate) stratum: Stratum,
}

/// A heap object (ADR-0036 object state): allocation-keyed, so aliases share it.
/// The `class` is fixed at construction and never swept; `class_exact` says whether
/// it is the *exact* runtime class or only a lower bound (see below); `props` are
/// the per-property value-domain facts.
#[derive(Clone)]
pub(crate) struct HeapObj {
    /// The class FQN (lowercase-normalized, as `classes_env` held). For an
    /// allocation-proven object (`new`, enum case, clone-of-exact) this is the exact
    /// runtime class; for a `$this` seed it is only a lower bound — the runtime
    /// object may be a descendant that inherited the method. `class_exact`
    /// distinguishes the two (audit G1, ADR-0036).
    pub(crate) class: String,
    /// Whether `class` is the exact runtime class (`true`) or a lower bound
    /// (`false`). A No-side conclusion (`is_a(class, T) = No`) is only sound when
    /// exact — with a lower bound the actual instance may be a `T` descendant.
    /// Yes-side conclusions hold for a lower bound too.
    pub(crate) class_exact: bool,
    /// Property facts keyed by property name (ADR-0035), each with its trust
    /// stratum (ADR-0052 §5).
    pub(crate) props: HashMap<String, PropFact>,
    /// Properties declared `readonly` — sweep-immune once established (ADR-0036).
    pub(crate) readonly: HashSet<String>,
    /// readonly props provably written on THIS path (for `readonly.reassigned`).
    pub(crate) ro_written: HashSet<String>,
    /// Whether this object has escaped (call arg, return, stored, captured).
    /// Escaped objects have non-readonly props swept by unknown calls; a
    /// purely-local object's props survive (ADR-0036 precision payoff).
    pub(crate) escaped: bool,
    /// The class-level generic parameterizations this object carries (ADR-0032 tier
    /// 3 + binding amendment, issue #295) — the same owner-keyed [`GenericCarry`]
    /// edges a `new Class(args)` expression proves, kept on the allocation so they
    /// survive a variable binding. Empty for every seeded object (ADR-0048 §3):
    /// parameters aren't seeded onto the heap today.
    ///
    /// Swept by a receiver method call ([`Self::sweep_targs`]) since a method may
    /// have replaced what a value carry states flowed into the constructor.
    pub(crate) targs: Vec<GenericCarry>,
}

impl HeapObj {
    /// A fresh heap object. `class_exact` defaults to `false` (a lower bound — the
    /// safe default); allocation-proven construction sites set it to `true`
    /// explicitly (`build_new_object`, exact `$this`/clone seeds).
    pub(crate) fn new(class: String) -> Self {
        HeapObj {
            class,
            class_exact: false,
            props: HashMap::new(),
            readonly: HashSet::new(),
            ro_written: HashSet::new(),
            escaped: false,
            targs: Vec::new(),
        }
    }

    /// Sweep the non-readonly props (an unknown/overridable call on an escaped or
    /// `$this` object may have mutated them). readonly props and the class survive.
    pub(crate) fn sweep_nonreadonly(&mut self) {
        self.props.retain(|name, _| self.readonly.contains(name));
    }

    /// Sweep the **value** generic carries (ADR-0032 binding amendment, issue #295):
    /// a method call on this object as receiver may have written the values a
    /// `new`-site carry recorded (`@phpstan-self-out self<U>`), so a stale value
    /// carry is a false positive, not a miss. Declared carries
    /// ([`GenericCarry::is_declared`]) state what the author wrote, which no call
    /// changes, so they survive like a `readonly` prop.
    pub(crate) fn sweep_targs(&mut self) {
        self.targs.retain(GenericCarry::is_declared);
    }

    /// The carries a **non-exact** object may still hand a reader (ADR-0032's
    /// 2026-08-16 amendment, issue #388): the declared ones, which is all such an
    /// object can hold — a value carry is minted only where an allocation proved
    /// one, and an allocation is exact. Written as a filter rather than an
    /// assertion so the rule reads the same as the sweep's.
    pub(crate) fn declared_targs(&self) -> Vec<GenericCarry> {
        self.targs.iter().filter(|c| c.is_declared()).cloned().collect()
    }
}

/// The object store threaded through the walk (ADR-0036). `refs` binds a variable
/// to an allocation id (its ObjRef);
/// `heap` maps ids to objects. Aliasing (`$b = $a`) copies the ref (shared id), so
/// a write through any alias is visible through all. A variable's exact-class fact
/// lives at `heap[refs[var]].class`.
#[derive(Clone, Default)]
pub(crate) struct Store {
    pub(crate) refs: HashMap<String, AllocId>,
    pub(crate) heap: HashMap<AllocId, HeapObj>,
    /// **Contract facts** (ADR-0052 §1): a variable's declared type as a lowered
    /// syntactic arm list, seeded at scope entry (§9) and narrowed by guards
    /// arm-wise (`instanceof`, `!== null`). Each arm carries its own trust stratum:
    /// native member list seeds `Verified`, a `@param` phpdoc refinement seeds
    /// `Asserted` (ADR-0037 trust order) — subtraction preserves it, so an
    /// `Asserted` arm never launders to `Verified`. Consumed only by the four §3
    /// consumers (arm filtering, `eval_instanceof` implication, catch matching,
    /// reserved-for-S6 declared-receiver lane); never by `call.on-null` proofs,
    /// arity, `call.undefined-method`, or binding descent.
    pub(crate) contract: HashMap<String, Vec<ContractArm>>,
    /// **Narrowed marks** (ADR-0088 §4's proven-narrowing rule, issue #428): the
    /// set of variables whose `contract` lane has had an actual guard subtraction
    /// land on the current path — an arm died or shrank, not merely "the lane
    /// exists". Set only by the three arm-subtraction functions
    /// ([`subtract_contract_lane`], [`subtract_pred_arms`], [`subtract_shape_arms`])
    /// when they change something; a fresh seed or reseed clears it (piggybacked on
    /// [`Store::unbind`], which already voids `contract` for the same reason).
    ///
    /// Exists because `contract_arms` alone cannot distinguish "narrowed to this
    /// residue" from "never touched, still the full seeded declaration" — both read
    /// as a non-empty arm list. A guard shape the arm lane cannot yet model (enum
    /// case identity, boolean literals — issue #429) leaves the lane at its full
    /// seed, indistinguishable from reachability without this bit, which is exactly
    /// the false-positive class this set exists to keep [`check_never_sentinel`]
    /// from manufacturing.
    ///
    /// [`subtract_contract_lane`]: crate::refine::subtract_contract_lane
    /// [`subtract_pred_arms`]: crate::predicates::subtract_pred_arms
    /// [`subtract_shape_arms`]: crate::shapes::subtract_shape_arms
    pub(crate) narrowed: HashSet<String>,
    /// **Class facts** (ADR-0052 §1): guard-derived is-a bounds on an
    /// object-holding variable, beside the heap's exact class. A positive
    /// `instanceof T` binds `T` into `yes`; negative binds into `no`. Deliberately
    /// weaker than exactness (ADR-0043) — not fed to exactness-gated consumers; a
    /// final-class `Member` is not treated as exactness in v1.
    pub(crate) members: HashMap<String, Member>,
    /// **Existence vouches** (ADR-0049 §4 guard-respect leg): symbols a positive
    /// `method_exists`/`function_exists`/`class_exists`… guard has vouched for on
    /// THIS branch. An absence-family emitter resolving to a vouched symbol stays
    /// silent even on its own `Absent` proof — firing would call the programmer a
    /// liar. Walk-local (ADR-0048): bound on the guarded branch clone, intersected
    /// at a join (so `if (method_exists(C,'m')) {} (new C)->m();` never silences
    /// the tail), and untouched by [`Self::unbind`]/[`Self::clear`] — a symbol's
    /// existence doesn't change on rebind or barrier.
    pub(crate) vouched: HashSet<Vouch>,
    /// **Same-expression guards** (issue #421's follow-up to #418): call
    /// expressions (`ArgValue::Call`/`MethodCall`) a `!== null`/`!== false`
    /// comparison or a bare truthy test has named on the branch where the
    /// non-null/non-false reading holds. The possibly-grade argument pair's
    /// `Call`/`MethodCall` premise reader declines when its own argument
    /// structurally equals one of these — the one operand shape neither
    /// [`collect_cmp_refine`] (`Var`-keyed) nor [`collect_shape_guards`]
    /// (`Offset`-keyed) can narrow, because a call has no binding to narrow: two
    /// textually-identical calls are two separate evaluations, so nothing here
    /// claims the ARGUMENT's own call returns what the GUARD's call proved —
    /// only that the guard is evidence the caller already reasoned about this
    /// exact expression, which is reason enough to stay silent rather than
    /// convict guarded code. Same walk-local discipline as `vouched`: bound on
    /// the branch clone, intersected at a join, untouched by
    /// [`Self::unbind`]/[`Self::clear`].
    ///
    /// [`collect_cmp_refine`]: crate::refine::collect_cmp_refine
    /// [`collect_shape_guards`]: crate::shapes::collect_shape_guards
    pub(crate) guarded_calls: Vec<ArgValue>,
}

/// A symbol a positive existence guard vouches for (ADR-0049 §4 guard-respect leg).
/// All names are lowercased — PHP class/function/method names are case-insensitive,
/// so the vouch matches the resolved emitter symbol case-blind.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Vouch {
    /// `method_exists(C, 'm')` vouched `C::m` — `class` is the receiver's FQN.
    Method { class: String, method: String },
    /// `function_exists('f')` vouched the function `f`.
    Function(String),
    /// `class_exists`/`interface_exists`/`trait_exists`/`enum_exists('N')` vouched `N`.
    Class(String),
}

/// One arm of a [`Store::contract`] lane: a declared-type alternative plus the
/// trust stratum it was seeded at (ADR-0052 §1/§5). The `ty` is the syntactic arm
/// judged arm-wise through steins-contract's single acceptance relation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContractArm {
    pub(crate) ty: steins_contract::ContractTy,
    pub(crate) stratum: Stratum,
}

/// Guard-derived is-a bounds on an object variable (ADR-0052 §1 `Member`): is-a
/// every class in `yes`, provably-not-is-a every class in `no`. FQNs are stored
/// lowercase-normalized (matching the is-a oracle's key). Bound at the `Verified`
/// stratum — a runtime `instanceof` executed on the live branch.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Member {
    pub(crate) yes: Vec<String>,
    pub(crate) no: Vec<String>,
}

impl Store {
    /// The exact class of the object `var` currently refers to, if any.
    pub(crate) fn class_of(&self, var: &str) -> Option<&str> {
        self.heap.get(self.refs.get(var)?).map(|o| o.class.as_str())
    }

    /// The class-level generic parameterizations the object `var` refers to carries
    /// (ADR-0032 binding amendment, issue #295). Empty slice for an unbound var.
    pub(crate) fn targs_of(&self, var: &str) -> &[GenericCarry] {
        self.obj_of(var).map_or(&[], |o| o.targs.as_slice())
    }

    /// Sweep the value generic carries of the object `var` refers to — the receiver
    /// half of [`HeapObj::sweep_targs`].
    pub(crate) fn sweep_targs(&mut self, var: &str) {
        if let Some(id) = self.refs.get(var).copied()
            && let Some(o) = self.heap.get_mut(&id)
        {
            o.sweep_targs();
        }
    }

    /// The allocation id `var` currently refers to.
    pub(crate) fn id_of(&self, var: &str) -> Option<AllocId> {
        self.refs.get(var).copied()
    }

    /// Whether `var` refers to an object whose `class` is the **exact** runtime
    /// class (audit G1). `false` for an unbound var and for any lower-bound object
    /// (a `$this` seed that is not provably exact) — the No-side gate.
    pub(crate) fn is_exact(&self, var: &str) -> bool {
        self.obj_of(var).is_some_and(|o| o.class_exact)
    }

    /// The object `var` currently refers to.
    pub(crate) fn obj_of(&self, var: &str) -> Option<&HeapObj> {
        self.heap.get(self.refs.get(var)?)
    }

    /// Whether `var` is bound to any object.
    pub(crate) fn is_bound(&self, var: &str) -> bool {
        self.refs.contains_key(var)
    }

    /// A property fact of the object `var` refers to (stratum-agnostic — used by
    /// contract-layer consumers, which accept `Asserted`).
    pub(crate) fn prop_fact(&self, var: &str, prop: &str) -> Option<&Fact> {
        self.obj_of(var)?.props.get(prop).map(|p| &p.fact)
    }

    /// The trust stratum of a property fact of the object `var` refers to, or
    /// `Verified` when there is no such prop (the neutral element of `min`).
    pub(crate) fn prop_stratum(&self, var: &str, prop: &str) -> Stratum {
        self.obj_of(var).and_then(|o| o.props.get(prop)).map_or(Stratum::Verified, |p| p.stratum)
    }

    /// Drop `var`'s ObjRef binding — the heap object survives (other aliases keep
    /// seeing it); `var` just forgets which object it held (ADR-0036: a pass-to-call
    /// may rebind `$var`, so the var→id link must die exactly as `classes_env`
    /// entries did, while the id lives on for its other aliases).
    pub(crate) fn unbind(&mut self, var: &str) {
        self.refs.remove(var);
        // Reassignment / invalidation also voids the guard-derived class facts and
        // the declared-type arm lane: a rebound `$var` no longer satisfies the
        // narrowed possibilities established for the old value (ADR-0052 §9 —
        // narrowing carriers are scope-local and die with the value they described).
        self.members.remove(var);
        self.contract.remove(var);
        // The narrowed mark describes THIS binding's guard history; a rebound var
        // starts a fresh one with none (issue #428).
        self.narrowed.remove(var);
    }

    /// Clear all bindings and the heap — a Barrier: nothing is reachable.
    pub(crate) fn clear(&mut self) {
        self.refs.clear();
        self.heap.clear();
        self.members.clear();
        self.contract.clear();
        self.narrowed.clear();
    }

    /// The narrowed declared-type arm lane of `var` (ADR-0052 §3, consumer (d) —
    /// the declared-receiver lane **reserved for S6** `phpdoc.undefined-method`).
    /// Built now so S6 consumes a stable accessor; N4 itself emits nothing from it.
    /// The returned arms are the seeded declared type minus every guard subtraction
    /// on the live branch (e.g. `{Guest}` after the else of `instanceof User` over
    /// `User|Guest`); each carries its stratum for the min-premise rule.
    pub(crate) fn contract_arms(&self, var: &str) -> Option<&[ContractArm]> {
        self.contract.get(var).map(Vec::as_slice).filter(|a| !a.is_empty())
    }

    /// Whether `var`'s declared lane has been **subtracted to nothing** on this
    /// branch (issue #429): the lane exists, every arm it held was `Verified`, and
    /// the guards on the way here deleted all of them — so no value of `var`
    /// reaches this point.
    ///
    /// Distinct from `contract_arms(var).is_none()`, which is the *absence* of a
    /// lane — the answer for an undeclared variable, an invalidated one, and an
    /// enum whose case set the absence discipline refused to complete. The two
    /// must not be confused: absence states nothing, and a consumer reading it as
    /// emptiness would claim an exhaustion nothing proved.
    pub(crate) fn contract_emptied(&self, var: &str) -> bool {
        self.contract.get(var).is_some_and(Vec::is_empty)
    }

    /// Whether `var`'s contract lane has had a **proven narrowing** land on the
    /// current path (issue #428) — an actual arm death/shrink, not merely a
    /// present lane. See [`Store::narrowed`] for why this is a separate bit from
    /// [`Store::contract_arms`] rather than derived from it.
    pub(crate) fn contract_narrowed(&self, var: &str) -> bool {
        self.narrowed.contains(var)
    }

    /// The class-membership fact of `var` (ADR-0052 §1 `Member`), if any guard bound
    /// one on this branch. Consumed only by [`eval_instanceof`] implication (§3b)
    /// and catch-arm matching — never the exactness-gated lanes.
    ///
    /// [`eval_instanceof`]: crate::cond::eval_instanceof
    pub(crate) fn member_of(&self, var: &str) -> Option<&Member> {
        self.members.get(var)
    }

    /// Record an existence vouch on this branch (ADR-0049 §4 guard-respect leg).
    pub(crate) fn vouch(&mut self, v: Vouch) {
        self.vouched.insert(v);
    }

    /// Record a same-expression call guard on this branch (issue #421). No dedup:
    /// the list is walk-local and small (one push per guard clause a statement's
    /// enclosing conditions carry), and `ArgValue`'s `Float` member has no `Eq`/
    /// `Hash` for a set to key on — a linear [`Self::expr_is_guarded`] scan is the
    /// cheaper honest answer than deriving one.
    pub(crate) fn guard_call(&mut self, v: ArgValue) {
        self.guarded_calls.push(v);
    }

    /// Whether `value` structurally equals a call this branch's guards named
    /// (issue #421) — the possibly-grade pair's own decline check.
    pub(crate) fn expr_is_guarded(&self, value: &ArgValue) -> bool {
        self.guarded_calls.iter().any(|g| g == value)
    }

    /// Whether a positive existence guard on this path vouched `class::method`
    /// (case-insensitively — the vouch stores lowercased names).
    pub(crate) fn vouches_method(&self, class: &str, method: &str) -> bool {
        self.vouched.contains(&Vouch::Method {
            class: class.to_ascii_lowercase(),
            method: method.to_ascii_lowercase(),
        })
    }

    /// Whether a positive `function_exists('f')` guard on this path vouched the
    /// function `f` (ADR-0049 §3 / FP-15 guard leg; case-insensitive — the vouch
    /// stores the lowercased, leading-`\`-stripped name). (`class.undefined` needs no
    /// twin here: its firing conditions — index Absent + boot not-found — are exactly
    /// what folds a `class_exists('X')` guard's branch dead, so dead-region pruning
    /// is that id's guard leg.)
    pub(crate) fn vouches_function(&self, fqn: &str) -> bool {
        self.vouched.contains(&Vouch::Function(fqn.trim_start_matches('\\').to_ascii_lowercase()))
    }

    /// Mark the object `var` refers to as escaped (if any).
    pub(crate) fn mark_escaped(&mut self, var: &str) {
        if let Some(id) = self.refs.get(var).copied()
            && let Some(o) = self.heap.get_mut(&id)
        {
            o.escaped = true;
        }
    }

    /// Sweep the `$this` object's non-readonly props and value carries (ADR-0057 C5) —
    /// what a call running with the same `$this` may have written behind this walk's
    /// back. A no-op where `$this` is unbound.
    pub(crate) fn sweep_this(&mut self) {
        if let Some(id) = self.refs.get("this").copied()
            && let Some(o) = self.heap.get_mut(&id)
        {
            o.sweep_nonreadonly();
            o.sweep_targs();
        }
    }

    /// Replace the object `var` refers to with a descent's `$this` snapshot
    /// (ADR-0057 C4, generalized by the 2026-08-17 amendment's D4): `props`,
    /// `readonly`, `ro_written` and `escaped` come from the callee's exit state, and
    /// `class`/`class_exact` are asserted rather than copied — no walk alters what
    /// class an allocation is, and the assert is what would catch a snapshot that got
    /// here from the wrong seed.
    ///
    /// **`targs` is the one field the snapshot does not speak for**, so the #295/#377
    /// carry sweep the statement already ran stands (D4). A property write is
    /// something the walk models and can therefore be believed about; a class-level
    /// carry is rewritten by `@phpstan-self-out self<U>`, which the walk models not at
    /// all — the callee's copy carries what came in and hands the same thing back,
    /// so restoring it would resurrect exactly the stale carry the ADR-0032 binding
    /// amendment closed, and in the direction that convicts correct code.
    ///
    /// A no-op where `var` is unbound: the statement's own effects may have dropped
    /// the binding between the descent and here, and a snapshot has no object of its
    /// own to bind to.
    pub(crate) fn copy_back(&mut self, var: &str, snapshot: &HeapObj) {
        let Some(id) = self.refs.get(var).copied() else { return };
        let Some(obj) = self.heap.get_mut(&id) else { return };
        debug_assert_eq!(
            obj.class, snapshot.class,
            "a `$this` snapshot must be the very allocation its call was made on",
        );
        obj.props = snapshot.props.clone();
        obj.readonly = snapshot.readonly.clone();
        obj.ro_written = snapshot.ro_written.clone();
        obj.escaped = snapshot.escaped;
    }

    /// Sweep every escaped object's non-readonly props (an unknown call ran that may
    /// mutate any escaped object). Non-escaped objects survive (ADR-0036 payoff).
    pub(crate) fn sweep_escaped(&mut self) {
        for o in self.heap.values_mut() {
            if o.escaped {
                o.sweep_nonreadonly();
            }
        }
    }
}

/// The **trust stratum** of a bound fact (ADR-0052 §5): whether it is fit to
/// premise a proof-layer finding. `Verified` facts come from a runtime-executed
/// test on the live branch (`===`, `is_int`, `instanceof`, ordering, truthiness) or
/// a native declaration seed. `assert($expr)` is `Verified` too: the 2026-07-25
/// owner ruling reads it as an unconditional throw-guard (`if (!$expr) throw`),
/// regardless of `zend.assertions`. `Asserted` facts come from docblock claims
/// (`@phpstan-assert` family) — a claim, not a proof; the bit is checked, not just
/// prose (the consumption rule requires all-Verified premises).
///
/// A derived fact's stratum is the minimum over every fact consumed in its
/// derivation (folds, array composition, heap writes, branch joins, descent
/// seeding all propagate `min(inputs)`), so `Asserted` can never launder into
/// `Verified` across a derivation step.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum Stratum {
    /// A runtime-executed test or a native declaration — fit for the proof layer.
    Verified,
    /// A docblock claim or an `assert($expr)` narrowing — never premises proof.
    Asserted,
}

impl Stratum {
    /// The weaker of two strata: `Asserted` dominates (the derivation clause). This
    /// is commutative and associative, so the min-stratum rule is order-independent
    /// (ADR-0048: no global ordering enters a fact).
    pub(crate) fn min(self, other: Stratum) -> Stratum {
        match (self, other) {
            (Stratum::Verified, Stratum::Verified) => Stratum::Verified,
            _ => Stratum::Asserted,
        }
    }
}

/// A proven local fact plus where it was established (for provenance). A closure
/// value has no scalar `fact` — it rides in [`Known::closure`] instead (ADR-0033).
#[derive(Clone)]
pub(crate) struct Known {
    /// The scalar/array value-domain fact, or `None` for a closure-only binding.
    pub(crate) fact: Option<Fact>,
    pub(crate) line: u32,
    pub(crate) bound: Option<String>,
    /// The proven closure value bound to this variable, if any (ADR-0033).
    pub(crate) closure: Option<ClosureVal>,
    /// The trust stratum of `fact` (ADR-0052 §5). `Verified` by default; an
    /// assert-derived or assert-laundered fact carries `Asserted`.
    pub(crate) stratum: Stratum,
}

impl Known {
    /// A plain value binding at the `Verified` stratum (native seeds, literal
    /// assignments, native-condition refinements — the common case).
    pub(crate) fn value(fact: Fact, line: u32, bound: Option<String>) -> Self {
        Known { fact: Some(fact), line, bound, closure: None, stratum: Stratum::Verified }
    }

    /// A plain value binding at an explicit stratum (derivation sites propagating
    /// `min(inputs)`, and the assert family binding `Asserted`).
    pub(crate) fn value_strat(fact: Fact, line: u32, bound: Option<String>, stratum: Stratum) -> Self {
        Known { fact: Some(fact), line, bound, closure: None, stratum }
    }

    /// A closure binding (no scalar fact; a closure is never assert-derived).
    pub(crate) fn closure(cv: ClosureVal, line: u32) -> Self {
        Known { fact: None, line, bound: None, closure: Some(cv), stratum: Stratum::Verified }
    }

    /// The single proven value, when the fact is a `Singleton` (converted back to
    /// the trace IR's [`ArgValue`]); `None` for every abstract or multi-valued
    /// layer (and for a closure-only binding) — those resolve no proven value.
    pub(crate) fn singleton(&self) -> Option<ArgValue> {
        match &self.fact {
            Some(Fact::Singleton(v)) => Some(arg_of_val(v)),
            _ => None,
        }
    }
}

/// A binding-descent key: the callee (by FQN-ish key) plus its bound params.
/// Each binding carries its trust [`Stratum`] so a Verified summary cannot be
/// replayed for an Asserted entry with the same value (issue #128 review).
/// Method descents may also carry a `this:` pseudo-binding for the exact
/// receiver (ADR-0075 §2.1); closure descents carry `use:{name}` captures.
pub(crate) type BindingKey = (String, Vec<(String, ArgValue, Stratum)>);

/// A **return-fact summary** (ADR-0057 amendment): the join, over a callee's
/// returning exits, of the returned expression's value-domain fact — a
/// legitimate query answer (ADR-0048 §2), a pure function of (callee CST, bound
/// entry state). It rides the same descent, the same [`BindingKey`] memo (now a
/// value map), and is consumed at the call-result binding as the value FLOOR above
/// the declared arms (A1).
///
/// The two components are independent (ADR-0057 T1 amendment B3): a heap summary
/// dies without touching the value summary and vice versa, and a summary exists
/// whenever EITHER does. They are also exclusive in practice — an object return
/// carries no value fact, the value domain having no object carrier (ADR-0035).
#[derive(Clone)]
pub(crate) struct ReturnSummary {
    /// The value-domain component (the T0 amendment): the joined returned-expression
    /// fact with its trust stratum.
    pub(crate) value: Option<SummaryValue>,
    /// The heap-object component (ADR-0057 §1, landed in T1): the joined snapshot of
    /// the allocation every returning exit hands back.
    pub(crate) heap: Option<HeapSummary>,
    /// The **`$this`** component (ADR-0057's 2026-08-17 same-`$this` amendment, D3):
    /// the joined snapshot of the callee's own `$this` at every exit, filled by any
    /// walk whose `$this` was seeded from a caller object — a constructor descent
    /// (C2), a same-`$this` call (D1), an exact `Receiver::Var` call (D2).
    ///
    /// Beside the other two rather than inside either: a constructor reads it where
    /// the site mints its object, and every other seeded walk reads it as the
    /// **copy-back** into the caller's own object, which is what lets a delegating
    /// constructor's and a fluent setter's writes survive the call (D4). The return
    /// value keeps riding `value`/`heap` untouched, which is the whole reason this is
    /// a third channel and not a re-purposed first one.
    pub(crate) this: Option<HeapSummary>,
}

/// The value-domain half of a [`ReturnSummary`]: the joined fact and its stratum
/// (`min` over exits, N2 — an `Asserted` exit drags the whole summary to `Asserted`).
#[derive(Clone)]
pub(crate) struct SummaryValue {
    pub(crate) fact: Fact,
    pub(crate) stratum: Stratum,
}

/// The heap-object component of a [`ReturnSummary`] (ADR-0057 §1): the value-object
/// snapshot every returning exit's allocation was taken as, joined per §2.4. The
/// caller **rebinds** it as a fresh allocation in its own heap — a copy, never a
/// shared identity ([`apply_assign`]'s heap rung).
///
/// The payload is a [`HeapObj`], not a parallel struct with the same six fields
/// (T1 amendment B1): §1's field list IS `HeapObj`'s, so the snapshot, the join and
/// the rebind all operate on the type the walk's heap already holds, and no field can
/// be added to `HeapObj` and silently forgotten by the crossing — which is exactly how
/// `targs` came to be missing from the ADR's own list.
///
/// [`apply_assign`]: crate::assign::apply_assign
#[derive(Clone)]
pub(crate) struct HeapSummary {
    /// The joined snapshot. Every field means what it means on the heap, with **one**
    /// re-reading at the boundary: `escaped` is **escaped-before-return** (§2.1) — the
    /// bit the callee's object carried one instant before the return's own
    /// escape-marking, so `false` says the return was the allocation's only exit and
    /// the caller holds the sole reference. The rebind copies it verbatim, so the
    /// re-reading costs no conversion.
    pub(crate) obj: HeapObj,
}

/// One returning exit's contribution to the summary join (A2/A3). A native-envelope
/// violating exit is DROPPED (a proven boundary TypeError, its value never reaches
/// the caller) — it is simply never recorded, so there is no variant for it.
pub(crate) enum ExitContribution {
    /// An informative exit within the declared envelope: its fact crosses with its
    /// stratum (a phpdoc-only violation crosses here — the walk truth, A2).
    Fact(Fact, Stratum),
    /// A factless returning exit: it contributes the declared value FLOOR (A3), the
    /// sound top within the envelope — `General{base}` degrades, never lies.
    Floor,
    /// An exit returning a **locally-held allocation** (ADR-0057 T1): the object's
    /// snapshot at the return point, `escaped` still meaning escaped-before-return.
    ///
    /// For the value join this is a [`Self::Floor`] and nothing else — an object is
    /// not a value (ADR-0035), so it degrades exactly as a factless exit does, which
    /// is what it WAS before T1. For the heap join it is the only contributing
    /// variant: any exit that is not one kills the heap summary (§2.5).
    Heap(Box<HeapObj>),
}

/// The per-descent summary-collection context threaded through [`WalkCx`] while a
/// callee body is walked for its return-fact summary. Holds the callee's native
/// return arms (the A2 drop oracle) and the accumulating exit list.
///
/// [`WalkCx`]: crate::walk::WalkCx
pub(crate) struct SummaryCtx {
    /// The callee's native return type lowered to contract arms — the drop test's
    /// oracle (A2): an exit fact every arm provably rejects is a boundary TypeError.
    pub(crate) native: Vec<ContractTy>,
    /// Each returning exit's contribution, in walk order (RefCell — pushed through
    /// the shared-immutable [`WalkCx`] as branches recurse).
    ///
    /// [`WalkCx`]: crate::walk::WalkCx
    pub(crate) exits: std::cell::RefCell<Vec<ExitContribution>>,
    /// The **`$this`** snapshot at each exit, `Some` exactly where this walk's `$this`
    /// was seeded from a caller object (ADR-0057 C2, generalized by the 2026-08-17
    /// amendment's D3): a constructor descent, a same-`$this` call, an exact
    /// `Receiver::Var` call. A bare `return;`, a value `return`, and the body's
    /// fall-through all contribute the snapshot; a `throw` contributes nothing, and an
    /// `Opaque` that `may_return` contributes the floor, which ends the component.
    ///
    /// A second list rather than a flavour on [`ExitContribution`]: the flavour is the
    /// descent's, and an ordinary method has a returned value to summarize on the
    /// other list at the very same exit.
    pub(crate) this_exits: Option<std::cell::RefCell<Vec<ExitContribution>>>,
}

impl SummaryCtx {
    /// Whether `fact` provably violates the native return envelope (A2): every native
    /// arm rejects it. With no native declaration there is nothing to violate.
    pub(crate) fn native_violates(&self, fact: &Fact) -> bool {
        !self.native.is_empty()
            && self
                .native
                .iter()
                .fold(Certainty::No, |acc, arm| acc.or(steins_contract::admits_fact(arm, fact)))
                .is_no()
    }
}

/// The state threaded down an interprocedural binding descent (Feature B). The memo
/// is a **value map** (ADR-0057 §3): a key's computed [`ReturnSummary`] is cached so
/// a memo hit REPLAYS it (a summary is a value, not a suppression bit) — legitimate
/// caching, since the summary is a pure function of the key's entry state.
pub(crate) struct Descent<'a> {
    pub(crate) provenance: &'a str,
    pub(crate) depth: usize,
    pub(crate) stack: &'a mut Vec<BindingKey>,
    pub(crate) memo: &'a mut HashMap<BindingKey, Option<ReturnSummary>>,
}

/// Join the fall-through envs of several live branches (ADR-0031/0035). Each
/// branch contributes ONE claim per name — its env value fact where it holds one,
/// else the lowering of its own contract-arm lane ([`lane_claim`], issue #589) —
/// and the claims fold through [`Fact::join`] (equal → Singleton; differing →
/// OneOf; overflow → dropped) at `min` over the contributing strata. A branch with
/// neither carrier, or whose lane does not lower, drops the name.
///
/// The cross-lane fallback exists because a guard moves the claim between lanes
/// mid-construct: `if ($i === 1) {}` over `@param 1|2` leaves the then branch's
/// knowledge in the value lane alone (`Refine::Exact` mints the singleton and
/// unbinds the arm lane) and the else branch's in the arm lane alone (the
/// subtraction's residue `2`; a phpdoc-only parameter never seeds an env fact), so
/// a lane-local join dropped BOTH carriers. Sound: the joined fact covers the
/// union of the per-branch claims, and each claim is that branch's own carrier —
/// the same over-approximation the join always computed, no longer blind to one of
/// the two lanes. Rebinding stays correct by construction: a rebound branch
/// contributes the NEW binding's carrier, so `if ($a === 1) { $a = 5; }` joins `5`
/// with the else lane's `2` into `2|5`, never resurrecting the pre-branch `1|2`.
///
/// The fallback fires only where the store join is about to DROP the lane (some
/// branch no longer carries it — the witness's then branch unbound its own). A
/// name whose lane survives in every branch, and one that is lane-only in every
/// branch, both stay out of the env: [`join_stores`]' arm union already carries
/// the whole claim there, arm-precise, and a value fact minted beside it would
/// outrank the lane at every fact read (ADR-0037) while holding only the arms'
/// blur.
pub(crate) fn join_envs(
    branches: Vec<(HashMap<String, Known>, Store)>,
) -> (HashMap<String, Known>, Store) {
    let mut it = branches.into_iter();
    let (first_env, first_classes) = it.next().expect("join_envs called with no branches");
    let rest: Vec<(HashMap<String, Known>, Store)> = it.collect();
    if rest.is_empty() {
        return (first_env, first_classes);
    }

    let mut env: HashMap<String, Known> = HashMap::new();
    for (name, k0) in &first_env {
        // A closure-only binding survives a join only when every branch binds the
        // SAME closure target (a differing/absent branch drops it — the safe side).
        let Some(cv0) = &k0.closure else { continue };
        let all_same = rest.iter().all(|(be, _)| {
            be.get(name)
                .and_then(|k| k.closure.as_ref())
                .is_some_and(|cv| closure_target_eq(&cv0.target, &cv.target))
        });
        if all_same {
            env.insert(name.clone(), Known::closure(cv0.clone(), k0.line));
        }
    }

    // The value join iterates the union of the branches' facted names, not the
    // first branch's env: under `!==` it is the FIRST fall-through branch whose
    // claim lives only in the arm lane (issue #589's mirror case). Only names
    // holding an env fact somewhere enter the set — a lane-only-everywhere name
    // is `join_stores`' to carry.
    let all: Vec<(&HashMap<String, Known>, &Store)> = std::iter::once((&first_env, &first_classes))
        .chain(rest.iter().map(|(be, bs)| (be, bs)))
        .collect();
    let mut names: HashSet<&String> = HashSet::new();
    for (be, _) in &all {
        names.extend(be.iter().filter(|(_, k)| k.fact.is_some()).map(|(n, _)| n));
    }
    for name in names {
        // A first-branch closure binding was ruled on above (kept or dropped);
        // a sibling's scalar fact must not resurrect the name as a value.
        if first_env.get(name).is_some_and(|k| k.closure.is_some()) {
            continue;
        }
        // The fallback rescues a claim the STORE join is about to drop. Where
        // every branch still carries the lane, `join_stores`' arm union is the
        // complete claim already — and arm-precise, where a value fact minted
        // here would be the arms' blur and would OUTRANK the lane at every fact
        // read (ADR-0037) — so a branch with no env fact contributes nothing
        // there, exactly as before this fix.
        let lane_survives = all.iter().all(|(_, bs)| bs.contract.contains_key(name.as_str()));
        let mut fact: Option<Fact> = None;
        // Derivation clause: a branch join takes `min` over the joined claims'
        // strata (Verified ⊔ Asserted ⇒ Asserted); `Verified` is min's neutral
        // element. Provenance (line/bound) comes from the first branch holding an
        // env fact — the names set guarantees one exists.
        let mut stratum = Stratum::Verified;
        let mut provenance: Option<(u32, Option<String>)> = None;
        let mut ok = true;
        for (be, bs) in &all {
            let (f, s) = match be.get(name).filter(|k| k.fact.is_some()) {
                Some(k) => {
                    if provenance.is_none() {
                        provenance = Some((k.line, k.bound.clone()));
                    }
                    (k.fact.clone().expect("filtered"), k.stratum)
                }
                None if lane_survives => {
                    ok = false;
                    break;
                }
                None => match lane_claim(bs, name) {
                    Some(claim) => claim,
                    None => {
                        ok = false;
                        break;
                    }
                },
            };
            stratum = stratum.min(s);
            fact = match fact.take() {
                None => Some(f),
                Some(prev) => match prev.join(&f) {
                    Some(joined) => Some(joined),
                    None => {
                        ok = false;
                        break;
                    }
                },
            };
        }
        if ok
            && let Some(fact) = fact
            && let Some((line, bound)) = provenance
        {
            env.insert(name.clone(), Known::value_strat(fact, line, bound, stratum));
        }
    }

    let rest_stores: Vec<&Store> = rest.iter().map(|(_, s)| s).collect();
    let store = join_stores(&first_classes, &rest_stores);
    (env, store)
}

/// The claim a branch's contract-arm lane makes for a name the branch holds no
/// env fact for (issue #589): the lane's arms lowered as one union through
/// [`steins_contract::to_fact`], at the lane's derived stratum — `Asserted` if any
/// arm is (the [`seed_refined_scalar_fact`] rule). `None` where the branch carries
/// no lane, an emptied one, or one the fact domain cannot express (class arms,
/// float arms, multi-array-arm unions) — the caller then drops the name, exactly
/// as an absent env fact always did.
///
/// [`seed_refined_scalar_fact`]: crate::refine::seed_refined_scalar_fact
/// [`seed_shape_fact`]: crate::refine::seed_shape_fact
fn lane_claim(store: &Store, name: &str) -> Option<(Fact, Stratum)> {
    let arms = store.contract_arms(name)?;
    // Two or more array arms stay in the arm lane (A-G3, the [`seed_shape_fact`]
    // rule): `to_fact`'s union fold would blur them into ONE shape, and the value
    // lane never holds the blur of a discrimination the arms still carry.
    if arms.iter().filter(|a| steins_contract::to_shape_fact(&a.ty).is_some()).count() >= 2 {
        return None;
    }
    let fact = steins_contract::to_fact(&ContractTy::Union(
        arms.iter().map(|a| a.ty.clone()).collect(),
    ))?;
    let stratum = if arms.iter().any(|a| a.stratum == Stratum::Asserted) {
        Stratum::Asserted
    } else {
        Stratum::Verified
    };
    Some((fact, stratum))
}

/// Join the heap stores of several fall-through branches (ADR-0036). A variable's
/// ObjRef survives only when every branch binds it to the SAME allocation id (a
/// pre-branch object keeps its id across the clones; a per-branch `new`/`clone`
/// gets a distinct id and so is dropped). A surviving object joins its props
/// member-wise (a prop survives only if present-and-joinable in every branch),
/// unions `escaped` (escaped anywhere → escaped), and intersects `ro_written` (a
/// readonly write counts only when proven on every joined path).
pub(crate) fn join_stores(first: &Store, rest: &[&Store]) -> Store {
    let mut refs: HashMap<String, AllocId> = HashMap::new();
    for (var, id) in &first.refs {
        if rest.iter().all(|s| s.refs.get(var) == Some(id)) {
            refs.insert(var.clone(), *id);
        }
    }
    let mut heap: HashMap<AllocId, HeapObj> = HashMap::new();
    // Join every id that survives via a ref (and any id present in all branches).
    let live_ids: HashSet<AllocId> = refs.values().copied().collect();
    for id in live_ids {
        let Some(o0) = first.heap.get(&id) else { continue };
        let others: Vec<&HeapObj> = rest.iter().filter_map(|s| s.heap.get(&id)).collect();
        if others.len() != rest.len() {
            continue; // not present in every branch — drop it
        }
        let mut joined = HeapObj::new(o0.class.clone());
        // A surviving id is the SAME allocation across every branch, so its class and
        // exactness bit are invariant — carry them from the first branch (audit G1).
        joined.class_exact = o0.class_exact;
        joined.readonly = o0.readonly.clone();
        // targs: an INTERSECTION (ADR-0032 binding amendment, issue #295) — a carry
        // survives only when every joined branch still carries it identically, so a
        // branch that swept it (a receiver call inside one arm of an `if`) erases it
        // for the successor. Order-independent, per the amendment's ADR-0048 §4 note.
        joined.targs = o0
            .targs
            .iter()
            .filter(|c| others.iter().all(|o| o.targs.contains(c)))
            .cloned()
            .collect();
        joined.escaped = o0.escaped || others.iter().any(|o| o.escaped);
        // ro_written: written on EVERY joined path.
        joined.ro_written = o0
            .ro_written
            .iter()
            .filter(|n| others.iter().all(|o| o.ro_written.contains(*n)))
            .cloned()
            .collect();
        // props: present-and-joinable in every branch, at `min` over strata
        // (derivation clause — a joined prop is Asserted if any branch's was).
        for (name, p0) in &o0.props {
            let mut fact = p0.fact.clone();
            let mut stratum = p0.stratum;
            let mut ok = true;
            for o in &others {
                match o.props.get(name) {
                    Some(kp) => match fact.join(&kp.fact) {
                        Some(j) => {
                            fact = j;
                            stratum = stratum.min(kp.stratum);
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    },
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                joined.props.insert(name.clone(), PropFact { fact, stratum });
            }
        }
        heap.insert(id, joined);
    }

    // Contract lane: a var survives the join only if present in every branch; its
    // arms are the union of the branches' surviving arms (any value live on ANY
    // path is possible after the merge), deduped, each arm keeping its own stratum
    // (an `Asserted` arm never launders to `Verified` through a join). Absent in any
    // branch → dropped to no-fact (sound: the successor simply carries no lane).
    let mut contract: HashMap<String, Vec<ContractArm>> = HashMap::new();
    for (var, arms0) in &first.contract {
        if !rest.iter().all(|s| s.contract.contains_key(var)) {
            continue;
        }
        let mut merged = arms0.clone();
        for s in rest {
            if let Some(arms) = s.contract.get(var) {
                merged.extend(arms.iter().cloned());
            }
        }
        dedup_contract_arms(&mut merged);
        contract.insert(var.clone(), merged);
    }

    // Narrowed marks (issue #428): the same INTERSECTION [`vouched`]/`members` take
    // — a var counts as narrowed after the join only where every branch proved a
    // subtraction landed on it. A var one branch narrowed and a sibling left
    // untouched is, after the merge, exactly a lane the merge cannot tell apart
    // from an unnarrowed one (its joined arms are the sibling's full seed at
    // best), so claiming it here would be the same manufactured-evidence shape
    // [`check_never_sentinel`] exists to refuse.
    let narrowed: HashSet<String> = first
        .narrowed
        .iter()
        .filter(|v| rest.iter().all(|s| s.narrowed.contains(*v)))
        .cloned()
        .collect();

    // Member lane: a var survives only if present in every branch; its `yes`/`no`
    // sets are the INTERSECTION across branches (a bound holds after the merge only
    // if it held on every path). An emptied Member is dropped (no-fact).
    let mut members: HashMap<String, Member> = HashMap::new();
    for (var, m0) in &first.members {
        let others: Vec<&Member> = rest.iter().filter_map(|s| s.members.get(var)).collect();
        if others.len() != rest.len() {
            continue;
        }
        let yes: Vec<String> =
            m0.yes.iter().filter(|c| others.iter().all(|o| o.yes.contains(c))).cloned().collect();
        let no: Vec<String> =
            m0.no.iter().filter(|c| others.iter().all(|o| o.no.contains(c))).cloned().collect();
        if !(yes.is_empty() && no.is_empty()) {
            members.insert(var.clone(), Member { yes, no });
        }
    }

    // Existence-vouch lane (ADR-0049 §4): a vouch survives the join only if EVERY
    // branch carried it — the intersection. A vouch bound on a guarded branch that
    // falls through must not leak onto a sibling path that was never guarded (so the
    // tail of `if (method_exists(C,'m')) {} (new C)->m();` still fires).
    let vouched: HashSet<Vouch> =
        first.vouched.iter().filter(|v| rest.iter().all(|s| s.vouched.contains(*v))).cloned().collect();

    // Same-expression call guards (issue #421): the same intersection as
    // `vouched`, for the same reason — a guard bound on one arm of a branch that
    // falls through must not silence the possibly-grade pair on a sibling path
    // that never tested the expression.
    let guarded_calls: Vec<ArgValue> = first
        .guarded_calls
        .iter()
        .filter(|g| rest.iter().all(|s| s.guarded_calls.iter().any(|o| o == *g)))
        .cloned()
        .collect();

    Store { refs, heap, contract, narrowed, members, vouched, guarded_calls }
}

/// Remove contract arms another surviving arm subsumes (`Certainty::Yes`) — the
/// stratified analogue of [`normalize::dedup_arms`]: on a subsumption tie the arm
/// with the **weaker** (min) stratum is kept, so a join can never raise an
/// `Asserted` possibility to `Verified` by dropping it in favor of a `Verified`
/// twin that denotes the same set (ADR-0052 §5 derivation clause).
pub(crate) fn dedup_contract_arms(arms: &mut Vec<ContractArm>) {
    let mut kept: Vec<ContractArm> = Vec::with_capacity(arms.len());
    for arm in arms.drain(..) {
        // Collapse a structurally-identical arm FIRST (`ty == ty`), keeping the min
        // stratum. This is the reflexive tie `subsumes`/`arm_eq` cannot prove for the
        // non-extensional arms (`StrOpaque`, `CallableTy`, `Opaque` — ADR-0038:
        // membership unmodeled, so `subsumes(x, x)` is `Maybe`). Exact structural
        // equality is a strictly stronger witness of same-denotation than mutual
        // subsumption, so keeping one is sound and loses no precision — and it stops a
        // branch-union from *doubling* a pile of identical opaque arms at every join.
        // Without it an `array`/`Closure` parameter threaded through a deeply nested
        // `if` tree grew to 2^depth copies of one arm (survey non-termination on
        // nextcloud `core/Migrations`).
        if let Some(k) =
            kept.iter_mut().find(|k| k.ty == arm.ty || normalize::arm_eq(&k.ty, &arm.ty))
        {
            k.stratum = k.stratum.min(arm.stratum);
            continue;
        }
        // Drop an arm strictly subsumed by a kept one; if it subsumes kept arms,
        // it replaces them (widening), inheriting the min stratum of all it covers.
        if kept.iter().any(|k| normalize::subsumes(&k.ty, &arm.ty).is_yes()) {
            continue;
        }
        let mut stratum = arm.stratum;
        kept.retain(|k| {
            if normalize::subsumes(&arm.ty, &k.ty).is_yes() {
                stratum = stratum.min(k.stratum);
                false
            } else {
                true
            }
        });
        kept.push(ContractArm { ty: arm.ty, stratum });
    }
    absorb_contract_arms(&mut kept);
    *arms = kept;
}

/// Run a contract-arm list to the interval-absorption fixpoint (issue #90), the
/// stratified analogue of the pass [`normalize::dedup_arms`] gained: `int<1, max>`
/// beside `0` is one denotation spelled two ways, and subsumption cannot collapse
/// it because neither arm covers the other. The merged arm takes the **min**
/// stratum of the pair (ADR-0052 §5): only as strongly held as the weaker of the
/// two claims it replaces, so a `Verified` twin can never lift an `Asserted` one.
///
/// Runs wherever an arm list is minted or joined, so the lane never *stores* the
/// two-armed spelling — the collapse is semantic, and the dump surface inherits
/// it rather than deciding it (ADR-0052 §4).
pub(crate) fn absorb_contract_arms(arms: &mut Vec<ContractArm>) {
    loop {
        let mut merged_at: Option<(usize, usize, ContractArm)> = None;
        'outer: for i in 0..arms.len() {
            for j in (i + 1)..arms.len() {
                if let Some(ty) = normalize::merge_int_arms(&arms[i].ty, &arms[j].ty) {
                    let stratum = arms[i].stratum.min(arms[j].stratum);
                    merged_at = Some((i, j, ContractArm { ty, stratum }));
                    break 'outer;
                }
            }
        }
        let Some((i, j, m)) = merged_at else { return };
        arms[i] = m;
        arms.remove(j);
    }
}
