//! The canonical abstract array fact (ADR-0062 §3 and Amendment A).
//!
//! One form covers every array truth (A-G1): plain `array` is the degenerate
//! shape (no fields, untyped unsealed tail), `array<K, V>` types the tail,
//! `list<T>` types the tail and pins `is_list` to `Yes`, `array{…}`/`list{…}`
//! declare fields. No array-`General` variant: the degenerate shape absorbs
//! ADR-0035's layer-4 array text.
//!
//! Declined by ADR ruling: `ContractTy` (slots are `Option<Box<Fact>>`,
//! A-G1a, to avoid inverting the contract-crate dependency; `None` is the
//! unknown floor); next-auto-index (ADR-0062 §3; append widens the tail
//! instead); and general meet (A-G7; narrowing is only S4's targeted
//! operators — [`ShapeFact::promote_present`], [`ShapeFact::mark_absent`],
//! [`ShapeFact::set_non_empty`], [`ShapeFact::set_is_list`],
//! [`ShapeFact::narrow_count`] — never a general ⊓).
//!
//! `is_list` is denotational, not syntactic (§3, RFC #14939): recomputed from
//! key structure by [`ShapeFact::normalize`]; a caller's flag may sharpen it,
//! never contradict it.
//!
//! Naming: the ADR's `VKey` is this crate's [`Key`] — one key vocabulary.

use crate::certainty::Certainty;
use crate::fact::Fact;
use crate::range::IntRange;
use crate::value::{Key, Val};

/// Field-width bound for a single shape (A-G6); PHPStan's `ARRAY_COUNT_LIMIT`,
/// imported as-is. Beyond it, a lift/seed degrades to the tail-only summary.
/// Distinct from the `OneOf` cap (whole-array union size); ADR-0062 §7
/// declines that union-degradation role here.
pub const SHAPE_WIDTH_LIMIT: usize = 256;

/// Whether a key is in the array, and on what evidence.
///
/// `Required.witnessed` is the presence stratum (§3, the Verified/Asserted
/// split): `true` = a runtime guard/observation established presence, `false`
/// = declared. Independent of the value stratum (A-G9); provenance only, it
/// never changes what the fact admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presence {
    /// The key is present.
    Required {
        /// A guard or observation established presence, rather than a docblock.
        witnessed: bool,
    },
    /// The key may or may not be present.
    Optional,
    /// The key is **proven** absent (post-`unset`, the false branch of `isset`).
    Absent,
}

impl Presence {
    /// `true` for either stratum of `Required`.
    #[must_use]
    pub const fn is_required(self) -> bool {
        matches!(self, Presence::Required { .. })
    }

    /// Presence join (A-G5): two `Required` stay required at the *lower*
    /// stratum; anything else that can differ becomes `Optional`; proven
    /// absence survives only when both sides prove it.
    #[must_use]
    pub const fn join(self, other: Presence) -> Presence {
        match (self, other) {
            (Presence::Required { witnessed: a }, Presence::Required { witnessed: b }) => {
                Presence::Required { witnessed: a && b }
            }
            (Presence::Absent, Presence::Absent) => Presence::Absent,
            _ => Presence::Optional,
        }
    }
}

/// The key contract of an unsealed tail. `ArrayKey` is PHP's `int|string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyClass {
    /// Integer keys only.
    Int,
    /// String keys only.
    Str,
    /// `array-key`: either.
    ArrayKey,
}

impl KeyClass {
    /// The class of one concrete key.
    #[must_use]
    pub const fn of_key(k: &Key) -> KeyClass {
        match k {
            Key::Int(_) => KeyClass::Int,
            Key::Str(_) => KeyClass::Str,
        }
    }

    /// Does this class admit the key?
    #[must_use]
    pub const fn admits_key(self, k: &Key) -> bool {
        matches!(
            (self, k),
            (KeyClass::ArrayKey, _) | (KeyClass::Int, Key::Int(_)) | (KeyClass::Str, Key::Str(_))
        )
    }

    /// Can this class supply an integer key?
    #[must_use]
    pub const fn admits_int(self) -> bool {
        matches!(self, KeyClass::Int | KeyClass::ArrayKey)
    }

    /// Join: unlike classes widen to `array-key`.
    #[must_use]
    pub fn join(self, other: KeyClass) -> KeyClass {
        if self == other { self } else { KeyClass::ArrayKey }
    }
}

/// What the shape says about keys it does not declare.
///
/// `Sealed` is PHPStan 2.2's default and is what makes isset-discrimination
/// sound (A-G3); an untyped `...` is `Unsealed { key: ArrayKey, value: None }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Tail {
    /// No undeclared key may be present.
    Sealed,
    /// Undeclared keys are admitted when they match `key` and their values
    /// match `value` (`None` = no knowledge, admits anything).
    Unsealed {
        /// Key contract for undeclared keys.
        key: KeyClass,
        /// Value fact for undeclared keys; `None` is the unknown floor.
        value: Option<Box<Fact>>,
    },
}

/// Which disjunction produced a cover (A-G8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CoverFlavor {
    /// From `isset(…) || isset(…)`: at least one key present **and non-null**.
    Isset,
    /// From `array_key_exists` disjunctions: at least one key present, value
    /// possibly null.
    KeyExists,
}

/// A disjunctive-presence fact: at least one of `keys` satisfies `flavor`.
///
/// Canonical covers have sorted, deduped keys with `len >= 2` — a singleton
/// cover *is* presence and [`ShapeFact::normalize`] promotes it rather than
/// carrying it (A-G8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cover {
    /// The covered keys: sorted, deduped, at least two.
    pub keys: Vec<Key>,
    /// What "satisfies" means for this cover.
    pub flavor: CoverFlavor,
}

impl Cover {
    /// A cover over `keys` with `flavor`; canonicalized by
    /// [`ShapeFact::normalize`], not here.
    #[must_use]
    pub fn new(keys: Vec<Key>, flavor: CoverFlavor) -> Cover {
        Cover { keys, flavor }
    }

    /// Does this cover's claim entail `other`'s? A cover over fewer keys is a
    /// stronger claim, and `Isset` (present non-null) entails `KeyExists`
    /// (present).
    #[must_use]
    fn subsumes(&self, other: &Cover) -> bool {
        let flavor_ok = self.flavor == other.flavor || self.flavor == CoverFlavor::Isset;
        flavor_ok && self.keys.iter().all(|k| other.keys.contains(k))
    }
}

/// One field of a shape: key, presence, and the value slot.
type Field = (Key, Presence, Option<Box<Fact>>);

/// The canonical abstract array fact, built through [`ShapeFact::normalize`]
/// (and the constructors that call it), which establishes:
///
/// * `fields` sorted by key ([`Key`] order: ints before strings), one per key;
/// * no `Absent` field under a `Sealed` tail (sealing already proves absence);
/// * `covers` a deterministic antichain, size ≥ 2, none containing a
///   `Required` key;
/// * `is_list` computed from key structure, sharpened but never contradicted
///   by the caller's flag;
/// * `non_empty` implied by any `Required` field or a `count_bound` floor ≥ 1;
/// * `count_bound` clamped non-negative, discharged into `Required` presence
///   under a `Sealed` tail the floor already exhausts (the exact-count pin,
///   issue #272);
/// * `order` absent by default — only [`ShapeFact::lift`] and other
///   observed-construction sites set it (issue #327).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShapeFact {
    /// Declared keys, sorted, one entry per key.
    pub fields: Vec<Field>,
    /// What undeclared keys may do.
    pub tail: Tail,
    /// Denotational `array_is_list` verdict (§3, RFC #14939).
    pub is_list: Certainty,
    /// The array is known to have at least one entry.
    pub non_empty: bool,
    /// Disjunctive-presence facts (A-G8).
    pub covers: Vec<Cover>,
    /// **The count accessory** (issue #272): an entry-count interval learned
    /// from a `count($x)` guard, independent of the key structure — met with
    /// the structural interval by [`ShapeFact::count_range`], so the two
    /// never disagree.
    pub count_bound: IntRange,
    /// **The order witness** (issue #327): the observed build-order key
    /// sequence, set only by [`ShapeFact::lift`] and literal-construction
    /// sites that saw the real build.
    ///
    /// Provenance, not extension — [`ShapeFact::admits`] never reads it, and
    /// two shapes differing only here admit the same values. It lets an
    /// order-dependent projection use a *realizable* order instead of the
    /// canonical sort `fields` uses (ADR-0062 §2) — the same role as
    /// `Presence::Required { witnessed }`. A *declared* order (e.g.
    /// `@param array{b: int, a: int}`) is never trusted this way (that is
    /// phpstan/phpstan#14940's false-positive class, ADR-0062 §7), so such a
    /// shape gets `None`.
    ///
    /// [`ShapeFact::normalize_counted`] always sets this `None`; any rebuild
    /// loses the witness. When present it is a permutation of the field keys
    /// under a `Sealed` tail; [`ShapeFact::with_order`] enforces both.
    pub order: Option<Vec<Key>>,
}

/// PHP's `array_is_list`: the keys are exactly `0..n-1`, in that order.
///
/// Sound **only** on the value lane, which is order-witnessed (ADR-0062 §2) —
/// this is the one place the domain reads insertion order.
#[must_use]
pub fn array_is_list(entries: &[(Key, Val)]) -> bool {
    keys_are_a_list(entries.iter().map(|(k, _)| k))
}

/// [`array_is_list`] over keys alone, for a caller that witnessed key order
/// but not every value (issue #327). Reads the sequence, not the set:
/// `[1 => 'a', 0 => 'b']` has a list's key set but is not one — why the order
/// witness cannot be reconstructed from the sorted `fields`.
#[must_use]
pub fn keys_are_a_list<'a, I: Iterator<Item = &'a Key>>(keys: I) -> bool {
    keys.enumerate().all(|(i, k)| match k {
        Key::Int(n) => i64::try_from(i).is_ok_and(|want| *n == want),
        Key::Str(_) => false,
    })
}

fn slot_admits(slot: &Option<Box<Fact>>, v: &Val) -> bool {
    match slot {
        None => true,
        Some(f) => f.admits(v),
    }
}

/// Join two value slots; unknown on either side stays unknown, and an
/// unrepresentable join degrades to unknown as well.
fn join_slots(a: &Option<Box<Fact>>, b: &Option<Box<Fact>>) -> Option<Box<Fact>> {
    match (a, b) {
        (Some(x), Some(y)) => x.join(y).map(Box::new),
        _ => None,
    }
}

/// A field present on one side only joins against the other side's tail bound:
/// an unsealed tail may already admit that key, a sealed one cannot.
fn join_slot_with_tail(slot: &Option<Box<Fact>>, other: &Tail) -> Option<Box<Fact>> {
    match other {
        Tail::Sealed => slot.clone(),
        Tail::Unsealed { value, .. } => join_slots(slot, value),
    }
}

fn join_tails(a: &Tail, b: &Tail) -> Tail {
    match (a, b) {
        (Tail::Sealed, Tail::Sealed) => Tail::Sealed,
        (Tail::Sealed, t) | (t, Tail::Sealed) => t.clone(),
        (
            Tail::Unsealed { key: ka, value: va },
            Tail::Unsealed { key: kb, value: vb },
        ) => Tail::Unsealed { key: ka.join(*kb), value: join_slots(va, vb) },
    }
}

impl ShapeFact {
    /// **The canonical constructor.** Every invariant listed on [`ShapeFact`]
    /// is established here; nothing else may build the struct literally.
    #[must_use]
    pub fn normalize(
        fields: Vec<Field>,
        tail: Tail,
        is_list: Certainty,
        non_empty: bool,
        covers: Vec<Cover>,
    ) -> ShapeFact {
        ShapeFact::normalize_counted(
            fields,
            tail,
            is_list,
            non_empty,
            covers,
            IntRange::NON_NEGATIVE,
        )
    }

    /// [`ShapeFact::normalize`] with a **count accessory** (issue #272).
    /// `count_bound` clamps to the non-negative ints (dropped if nothing
    /// survives — widening, ADR-0052 §2); a floor ≥1 implies `non_empty`; and
    /// under a `Sealed` tail whose declared keys the floor exhausts, every
    /// declared key becomes `Required` (the exact-count pin), at
    /// `witnessed: false` unless already witnessed (A-G9). No pin under
    /// `Unsealed` — a floor there bounds count only, not keys.
    #[must_use]
    pub fn normalize_counted(
        mut fields: Vec<Field>,
        tail: Tail,
        is_list: Certainty,
        non_empty: bool,
        covers: Vec<Cover>,
        count_bound: IntRange,
    ) -> ShapeFact {
        let count_bound =
            count_bound.intersect(IntRange::NON_NEGATIVE).unwrap_or(IntRange::NON_NEGATIVE);
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        fields.dedup_by(|a, b| a.0 == b.0);

        // A singleton cover is presence, not a disjunction, so it's promoted;
        // an empty cover claims nothing and is dropped (widening).
        let mut kept: Vec<Cover> = Vec::new();
        for mut c in covers {
            c.keys.sort();
            c.keys.dedup();
            match c.keys.len() {
                0 => {}
                1 => promote_present(&mut fields, &c.keys[0], &tail),
                _ => kept.push(c),
            }
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));

        // A sealed tail already proves absence; an `Absent` field is redundant.
        if matches!(tail, Tail::Sealed) {
            fields.retain(|(_, p, _)| !matches!(p, Presence::Absent));
        }

        // Exact-count pin: no room left for an absent declared key; `declared`
        // is exactly the array's admitted key set (post-retain, above).
        if matches!(tail, Tail::Sealed)
            && i64::try_from(fields.len()).is_ok_and(|declared| count_bound.lo() >= declared)
        {
            for (_, p, _) in &mut fields {
                let keep = matches!(*p, Presence::Required { witnessed: true });
                *p = Presence::Required { witnessed: keep };
            }
        }

        // A cover with a `Required` member is discharged.
        kept.retain(|c| !c.keys.iter().any(|k| field_of(&fields, k).is_some_and(|(_, p, _)| p.is_required())));
        let covers = antichain(kept);

        let non_empty = non_empty
            || count_bound.lo() >= 1
            || fields.iter().any(|(_, p, _)| p.is_required());
        let computed = compute_is_list(&fields, &tail, non_empty);
        let is_list = sharpen_is_list(computed, is_list);

        // `order: None` unconditionally — the drop discipline's enforcement
        // point (every derived shape is built here).
        ShapeFact { fields, tail, is_list, non_empty, covers, count_bound, order: None }
    }

    /// Attach an **order witness** (issue #327): the observed build order.
    ///
    /// Refuses (returns unwitnessed) unless the tail is `Sealed` (an unsealed
    /// tail admits keys the sequence can't state) and `order` is a permutation
    /// of the field keys — same length, every key present, no duplicates. This
    /// guards against a caller installing a witness it never actually observed.
    #[must_use]
    pub fn with_order(mut self, order: Vec<Key>) -> ShapeFact {
        let consistent = matches!(self.tail, Tail::Sealed)
            && order.len() == self.fields.len()
            && self.fields.iter().all(|(k, _, _)| order.contains(k))
            && !order.iter().enumerate().any(|(i, k)| order[..i].contains(k));
        if consistent {
            self.order = Some(order);
        }
        self
    }

    /// The observed key sequence, when witnessed and every field is `Required`
    /// (issue #328) — an optional field makes length variable, so no single
    /// sequence would describe every admitted value.
    #[must_use]
    pub fn witnessed_order(&self) -> Option<&[Key]> {
        let order = self.order.as_deref()?;
        self.fields.iter().all(|(_, p, _)| p.is_required()).then_some(order)
    }

    /// The degenerate shape: plain `array` (A-G1). No fields, an untyped
    /// unsealed tail, nothing decided.
    #[must_use]
    pub fn plain_array() -> ShapeFact {
        ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    /// The declared field for `k`, if any.
    #[must_use]
    pub fn field(&self, k: &Key) -> Option<&Field> {
        field_of(&self.fields, k)
    }

    /// Does this shape admit some non-empty array? Over-approximated on the
    /// permissive side, which pushes [`Fact::truthy`] toward `Maybe`.
    #[must_use]
    pub fn can_be_non_empty(&self) -> bool {
        matches!(self.tail, Tail::Unsealed { .. })
            || self.fields.iter().any(|(_, p, _)| !matches!(p, Presence::Absent))
    }

    /// Does this shape already establish `c` (A-G8)? Either a carried cover
    /// entails `c`, or one of `c`'s keys is `Required` here — a `Required` key
    /// discharges `KeyExists` unconditionally but `Isset` only when the slot
    /// proves non-null (a nullable required value doesn't make `isset` true).
    #[must_use]
    pub fn implies_cover(&self, c: &Cover) -> bool {
        self.covers.iter().any(|own| own.subsumes(c))
            || c.keys.iter().any(|k| self.key_implies(k, c.flavor))
    }

    /// **Cover recording** (A-G8, S5): the true branch of
    /// `isset($x['a']) || isset($x['b'])` — "at least one of `keys` satisfies
    /// `flavor`", nothing more. Canonicalized by [`ShapeFact::normalize`]: a
    /// singleton promotes to `Required { witnessed: true }` instead of being
    /// stored, a cover with an already-`Required` key drops, and covers stay a
    /// deterministic antichain.
    ///
    /// Sets `non_empty`: a cover claims some key present, so no admitted array
    /// is empty. [`ShapeFact::mark_absent`] re-derives the flag so it never
    /// disagrees with the covers.
    #[must_use]
    pub fn record_cover(&self, keys: Vec<Key>, flavor: CoverFlavor) -> ShapeFact {
        let mut covers = self.covers.clone();
        let claims_a_key = keys.len() > 1;
        covers.push(Cover::new(keys, flavor));
        ShapeFact::normalize_counted(
            self.fields.clone(),
            self.tail.clone(),
            self.is_list,
            self.non_empty || claims_a_key,
            covers,
            self.count_bound,
        )
    }

    /// **Cover discharge** (A-G11): does some cover prove `key` present, given
    /// every *other* member of that cover is in `absent_keys`? This is what a
    /// `??` right-arm consumes once its left arms fail `isset`.
    ///
    /// The returned [`CoverFlavor`] is the claim, not the verdict:
    /// [`CoverFlavor::Isset`] discharges unconditionally (present and
    /// non-null); [`CoverFlavor::KeyExists`] leaves value nullability to the
    /// caller, who must confirm each `absent_keys` member's slot
    /// ([`ShapeFact::field`]) is non-nullable before trusting a present-null
    /// member as "fell through". `Isset` wins when both claims are available.
    #[must_use]
    pub fn cover_proves(&self, key: &Key, absent_keys: &[Key]) -> Option<CoverFlavor> {
        self.covers
            .iter()
            .filter(|c| c.keys.contains(key))
            .filter(|c| c.keys.iter().all(|k| k == key || absent_keys.contains(k)))
            .map(|c| c.flavor)
            .min()
    }

    fn key_implies(&self, k: &Key, flavor: CoverFlavor) -> bool {
        match self.field(k) {
            Some((_, p, slot)) if p.is_required() => match flavor {
                CoverFlavor::KeyExists => true,
                CoverFlavor::Isset => slot.as_ref().is_some_and(|f| f.is_null().is_no()),
            },
            _ => false,
        }
    }

    /// **Lift** an order-witnessed array value into the abstract stratum
    /// (A-G5): every entry becomes `Required { witnessed: true }` with a
    /// `Singleton` slot, the tail seals, `is_list` is the real
    /// [`array_is_list`] verdict. A nested array lifts to a `Singleton` slot
    /// (strictly more precise than a nested shape).
    ///
    /// Beyond [`SHAPE_WIDTH_LIMIT`] fields it degrades to the tail-only
    /// summary (A-G6), losing the order witness (no keys left to sequence);
    /// below the limit the observed order rides along as
    /// [`ShapeFact::order`] (issue #327).
    #[must_use]
    pub fn lift(entries: &[(Key, Val)]) -> ShapeFact {
        let is_list = Certainty::from_bool(array_is_list(entries));
        let non_empty = !entries.is_empty();
        if entries.len() > SHAPE_WIDTH_LIMIT {
            return ShapeFact::normalize(
                Vec::new(),
                tail_summary(entries.iter().map(|(k, v)| (k, v))),
                is_list,
                non_empty,
                Vec::new(),
            );
        }
        let fields = entries
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Presence::Required { witnessed: true },
                    Some(Box::new(Fact::Singleton(v.clone()))),
                )
            })
            .collect();
        ShapeFact::normalize(fields, Tail::Sealed, is_list, non_empty, Vec::new())
            .with_order(entries.iter().map(|(k, _)| k.clone()).collect())
    }

    /// **The partially-known literal** (issue #327): [`ShapeFact::lift`]'s
    /// sibling for when construction was observed but some *values* were not
    /// — takes `None` for an unproven entry's slot. Keys were still observed,
    /// so each field is `Required { witnessed: true }`, the tail is `Sealed`,
    /// and the order rides along as the witness. Keeps
    /// `['p' => 1, 'q' => $s]` from collapsing: an unknown value costs only
    /// that value, not the key set or sibling entries.
    ///
    /// Beyond [`SHAPE_WIDTH_LIMIT`] entries it degrades to the tail-only
    /// summary like `lift`, and any unknown slot makes that summary's value
    /// bound unknown too.
    #[must_use]
    pub fn from_witnessed_entries(entries: &[(Key, Option<Fact>)]) -> ShapeFact {
        let is_list = Certainty::from_bool(keys_are_a_list(entries.iter().map(|(k, _)| k)));
        let non_empty = !entries.is_empty();
        if entries.len() > SHAPE_WIDTH_LIMIT {
            return ShapeFact::normalize(
                Vec::new(),
                slot_tail_summary(entries),
                is_list,
                non_empty,
                Vec::new(),
            );
        }
        let fields = entries
            .iter()
            .map(|(k, f)| {
                (k.clone(), Presence::Required { witnessed: true }, f.clone().map(Box::new))
            })
            .collect();
        ShapeFact::normalize(fields, Tail::Sealed, is_list, non_empty, Vec::new())
            .with_order(entries.iter().map(|(k, _)| k.clone()).collect())
    }

    /// Extensional membership: does this shape admit the array `entries`?
    #[must_use]
    pub fn admits(&self, entries: &[(Key, Val)]) -> bool {
        for (k, p, slot) in &self.fields {
            let found = entries.iter().find(|(ek, _)| ek == k).map(|(_, v)| v);
            match p {
                // The witness bit is provenance, not extension.
                Presence::Required { .. } => match found {
                    None => return false,
                    Some(v) => {
                        if !slot_admits(slot, v) {
                            return false;
                        }
                    }
                },
                Presence::Optional => {
                    if let Some(v) = found
                        && !slot_admits(slot, v)
                    {
                        return false;
                    }
                }
                Presence::Absent => {
                    if found.is_some() {
                        return false;
                    }
                }
            }
        }
        for (k, v) in entries {
            if self.fields.iter().any(|(fk, _, _)| fk == k) {
                continue;
            }
            match &self.tail {
                Tail::Sealed => return false,
                Tail::Unsealed { key, value } => {
                    if !key.admits_key(k) || !slot_admits(value, v) {
                        return false;
                    }
                }
            }
        }
        match self.is_list {
            Certainty::Yes if !array_is_list(entries) => return false,
            Certainty::No if array_is_list(entries) => return false,
            _ => {}
        }
        if self.non_empty && entries.is_empty() {
            return false;
        }
        // The count accessory is extensional (issue #272): outside the
        // learned interval, not admitted, whatever the keys say.
        match i64::try_from(entries.len()) {
            Ok(n) if !self.count_bound.contains(n) => return false,
            _ => {}
        }
        self.covers.iter().all(|c| {
            c.keys.iter().any(|k| match entries.iter().find(|(ek, _)| ek == k) {
                None => false,
                Some((_, v)) => match c.flavor {
                    CoverFlavor::Isset => *v != Val::Null,
                    CoverFlavor::KeyExists => true,
                },
            })
        })
    }

    /// **The entry count every admitted array can have** (ADR-0062 §4's
    /// `count($x)` row), as an inclusive interval: floor is one per
    /// `Required` field (floored at 1 when `non_empty` or a cover forbids
    /// empty); ceiling is a `Sealed` tail's declared, non-`Absent` keys
    /// (mirrors PHPStan's one exact-size case) or unbounded under `Unsealed`.
    /// `lo == hi` is the exact-size case. The learned `count_bound`
    /// (issue #272) meets the structural interval here; a contradicting bound
    /// is no-fact (ADR-0052 §2), so the structural answer stands.
    #[must_use]
    pub fn count_range(&self) -> IntRange {
        let required = self.fields.iter().filter(|(_, p, _)| p.is_required()).count();
        let declared = self.fields.iter().filter(|(_, p, _)| !matches!(p, Presence::Absent)).count();
        let floor_one = self.non_empty || !self.covers.is_empty();
        let lo = i64::try_from(required).unwrap_or(i64::MAX).max(i64::from(floor_one));
        let hi = match self.tail {
            Tail::Sealed => i64::try_from(declared).unwrap_or(i64::MAX),
            Tail::Unsealed { .. } => i64::MAX,
        };
        // `lo <= hi` holds by construction; the fallback just keeps this total.
        let structural = IntRange::new(lo, hi).unwrap_or(IntRange::NON_NEGATIVE);
        structural.intersect(self.count_bound).unwrap_or(structural)
    }

    /// **Join** (A-G5): field-wise, with the tail absorbing the key-set
    /// difference. `Sealed{a} ⊔ Sealed{b} = {a?, b?} + Sealed`.
    #[must_use]
    pub fn join(&self, other: &ShapeFact) -> ShapeFact {
        let mut keys: Vec<Key> = self.fields.iter().map(|(k, _, _)| k.clone()).collect();
        for (k, _, _) in &other.fields {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        keys.sort();

        let mut fields: Vec<Field> = Vec::with_capacity(keys.len());
        for k in keys {
            let entry = match (self.field(&k), other.field(&k)) {
                (Some((_, pa, sa)), Some((_, pb, sb))) => (pa.join(*pb), join_slots(sa, sb)),
                (Some((_, _, sa)), None) => {
                    (Presence::Optional, join_slot_with_tail(sa, &other.tail))
                }
                (None, Some((_, _, sb))) => {
                    (Presence::Optional, join_slot_with_tail(sb, &self.tail))
                }
                // `k` came from one of the two field lists.
                (None, None) => (Presence::Optional, None),
            };
            fields.push((k, entry.0, entry.1));
        }

        // A cover survives only when *both* sides imply it.
        let covers: Vec<Cover> = self
            .covers
            .iter()
            .chain(other.covers.iter())
            .filter(|c| self.implies_cover(c) && other.implies_cover(c))
            .cloned()
            .collect();

        ShapeFact::normalize_counted(
            fields,
            join_tails(&self.tail, &other.tail),
            Certainty::all_of([self.is_list, other.is_list]),
            self.non_empty && other.non_empty,
            covers,
            // Accessory joins by hull; a side with `NON_NEGATIVE` (learned
            // nothing) absorbs the other.
            self.count_bound.hull(other.count_bound),
        )
    }

    // ------------------------------------------------------------------
    // Narrowing operators (ADR-0062 S4; A-G7's targeted refinements — see the
    // module doc's "No general meet").
    //
    // Each is a narrowing: result admits every receiver-admitted array that
    // also satisfies the guard, widening (never bottoming) where the exact
    // meet isn't representable — noted at each such site.
    //
    // All route through `normalize`, re-deriving `is_list`/`non_empty`/covers.
    // `promote_present`, `set_non_empty`, `set_is_list` may still carry the
    // receiver's `is_list` (it only sharpens a computed `Maybe`, §3) because
    // each returns a subset of the receiver's denotation; `mark_absent` is the
    // exception, explained at its own site.
    // ------------------------------------------------------------------

    /// **Presence promotion** (ADR-0062 §4's guard row, #51 L3): the true
    /// branch of `isset($x[k])` (`strip_null`) or `array_key_exists(k, $x)`
    /// (no strip). `k` becomes `Required { witnessed }` — presence stratum
    /// moves, value stratum doesn't (A-G9); `strip_null` also drops `null`
    /// since `isset` is false on present-null. `witnessed` is the guard's own
    /// stratum (ADR-0058): a runtime guard passes `true`, a docblock-only
    /// claim (e.g. a userland `@phpstan-assert` helper) passes `false` —
    /// provenance only, never a second narrowing semantics.
    ///
    /// An undeclared `k` becomes a field only if the tail can supply it
    /// (rejected by `Sealed` or a non-admitting `Unsealed` key class), in
    /// which case the shape returns unchanged — the one inexact case here.
    #[must_use]
    pub fn promote_present(&self, k: &Key, strip_null: bool, witnessed: bool) -> ShapeFact {
        let mut fields = self.fields.clone();
        match fields.iter_mut().find(|(fk, _, _)| fk == k) {
            Some((_, p, slot)) => {
                // Never lowers an already-witnessed presence (join mins
                // strata; re-promotion must not).
                let keep = matches!(*p, Presence::Required { witnessed: true });
                *p = Presence::Required { witnessed: witnessed || keep };
                if strip_null {
                    *slot = strip_null_slot(slot);
                }
            }
            None => match &self.tail {
                // Runtime-impossible against this shape; widen rather than
                // claim it unsoundly.
                Tail::Sealed => return self.clone(),
                Tail::Unsealed { key: class, value } => {
                    if !class.admits_key(k) {
                        return self.clone();
                    }
                    let slot = if strip_null { strip_null_slot(value) } else { value.clone() };
                    fields.push((k.clone(), Presence::Required { witnessed }, slot));
                }
            },
        }
        ShapeFact::normalize_counted(
            fields,
            self.tail.clone(),
            self.is_list,
            self.non_empty,
            self.covers.clone(),
            // As a guard the count is unchanged; a write may grow past the
            // ceiling, so that call site relaxes it first
            // ([`ShapeFact::relax_count_ceiling`]).
            self.count_bound,
        )
    }

    /// **Drop a learned count ceiling** (issue #272), keeping the floor. The
    /// one operator that widens: `$x[k] = v` can only add or overwrite an
    /// entry, so "at least n" survives but "at most n" doesn't.
    ///
    /// Also drops the order witness by hand, since this is the sole
    /// construction that bypasses [`ShapeFact::normalize_counted`] — the write
    /// it models may append a key the sequence doesn't mention (issue #327).
    #[must_use]
    pub fn relax_count_ceiling(&self) -> ShapeFact {
        let relaxed = IntRange::new(self.count_bound.lo(), i64::MAX)
            .unwrap_or(IntRange::NON_NEGATIVE);
        ShapeFact { count_bound: relaxed, order: None, ..self.clone() }
    }

    /// **The count narrowing** (issue #272): meet the learned count interval
    /// with `want` (the branch's `count($x)` comparison). Widens rather than
    /// bottoms on an empty meet, like the rest of this block
    /// ([`ShapeFact::count_range`] states the rule once).
    #[must_use]
    pub fn narrow_count(&self, want: IntRange) -> ShapeFact {
        let met = self.count_bound.intersect(want).unwrap_or(self.count_bound);
        ShapeFact::normalize_counted(
            self.fields.clone(),
            self.tail.clone(),
            self.is_list,
            self.non_empty,
            self.covers.clone(),
            met,
        )
    }

    /// **Proven absence**: `unset($x[k])`, and the false branch of
    /// `array_key_exists(k, $x)`. `k` becomes [`Presence::Absent`]; under a
    /// `Sealed` tail `normalize` drops the field (sealing already proves it).
    ///
    /// Two laws, stronger governs: as a *guard* it need only keep what the
    /// receiver admits without `k`; as `unset` it must keep `v \ {k}` for
    /// every admitted `v`. The `unset` law forces re-derivation: `is_list`
    /// recomputed (carrying it is unsound — `array{a: string}` is `No`, but
    /// removing `a` leaves `[]`, a list; §4's row) and `non_empty` dropped
    /// (can't tell a declaration's `non-empty` from the just-removed key).
    /// Covers containing `k` are killed, not shrunk (A-G8's invalidation law).
    #[must_use]
    pub fn mark_absent(&self, k: &Key) -> ShapeFact {
        let mut fields = self.fields.clone();
        match fields.iter_mut().find(|(fk, _, _)| fk == k) {
            Some((_, p, slot)) => {
                *p = Presence::Absent;
                *slot = None;
            }
            None => match &self.tail {
                // Sealed already proves it; nothing to record.
                Tail::Sealed => return self.clone(),
                Tail::Unsealed { .. } => {
                    fields.push((k.clone(), Presence::Absent, None));
                }
            },
        }
        let covers: Vec<Cover> =
            self.covers.iter().filter(|c| !c.keys.contains(k)).cloned().collect();
        // Floor drops (same reason as `non_empty`); ceiling survives — removal
        // can't push count above an already-respected bound.
        let count_bound =
            IntRange::new(0, self.count_bound.hi()).unwrap_or(IntRange::NON_NEGATIVE);
        ShapeFact::normalize_counted(
            fields,
            self.tail.clone(),
            Certainty::Maybe,
            false,
            covers,
            count_bound,
        )
    }

    /// **`non_empty` set**: the true branch of `if ($x)` on an array base.
    #[must_use]
    pub fn set_non_empty(&self) -> ShapeFact {
        ShapeFact::normalize_counted(
            self.fields.clone(),
            self.tail.clone(),
            self.is_list,
            true,
            self.covers.clone(),
            self.count_bound,
        )
    }

    /// **The `is_list` flag flip** (RFC #14939's C1): `array_is_list($x)`
    /// narrows to [`Certainty::Yes`]/[`Certainty::No`] — a pure flag flip, no
    /// structural surgery.
    ///
    /// `normalize` still owns the verdict: a contradicting flag loses (§3),
    /// soundly — a computed `No` means the true branch's meet is empty.
    #[must_use]
    pub fn set_is_list(&self, want: Certainty) -> ShapeFact {
        ShapeFact::normalize_counted(
            self.fields.clone(),
            self.tail.clone(),
            want,
            self.non_empty,
            self.covers.clone(),
            self.count_bound,
        )
    }
}

/// Drop `null` from a value slot. `None` stays unknown; a slot that is
/// exactly `null` degrades to unknown, not an empty fact (no bottom in this
/// domain — widening is safe).
fn strip_null_slot(slot: &Option<Box<Fact>>) -> Option<Box<Fact>> {
    slot.as_deref().and_then(strip_null_fact).map(Box::new)
}

fn strip_null_fact(f: &Fact) -> Option<Fact> {
    match f {
        Fact::Singleton(Val::Null) => None,
        Fact::Singleton(_) => Some(f.clone()),
        Fact::OneOf(vals) => {
            Fact::from_vals(vals.iter().filter(|v| **v != Val::Null).cloned().collect())
        }
        Fact::Refined { base, refinement, .. } => Some(Fact::refined(*base, *refinement, false)),
        Fact::General { base, .. } => Some(Fact::General { base: *base, nullable: false }),
        // The arms are untouched: `null` sits beside them, not inside one.
        Fact::Union { arms, .. } => Fact::union(arms.clone(), false),
        Fact::Shape { shape, .. } => {
            Some(Fact::Shape { shape: shape.clone(), nullable: false })
        }
    }
}

/// The tail-only summary an over-wide array degrades to (A-G6).
fn tail_summary<'a, I: Iterator<Item = (&'a Key, &'a Val)>>(entries: I) -> Tail {
    let mut key: Option<KeyClass> = None;
    let mut vals: Vec<Val> = Vec::new();
    for (k, v) in entries {
        let c = KeyClass::of_key(k);
        key = Some(key.map_or(c, |acc| acc.join(c)));
        vals.push(v.clone());
    }
    Tail::Unsealed {
        key: key.unwrap_or(KeyClass::ArrayKey),
        value: Fact::from_vals(vals).map(Box::new),
    }
}

/// [`tail_summary`] for fact-valued entries that may be unknown (issue #327).
/// One unknown slot makes the whole bound unknown — the tail states what
/// *every* undeclared entry satisfies.
fn slot_tail_summary(entries: &[(Key, Option<Fact>)]) -> Tail {
    let mut key: Option<KeyClass> = None;
    let mut value: Option<Fact> = None;
    let mut all_known = true;
    for (k, slot) in entries {
        let c = KeyClass::of_key(k);
        key = Some(key.map_or(c, |acc| acc.join(c)));
        match (slot, all_known) {
            (Some(f), true) => {
                value = match value {
                    None => Some(f.clone()),
                    Some(acc) => acc.join(f),
                };
                all_known = value.is_some();
            }
            _ => all_known = false,
        }
    }
    Tail::Unsealed {
        key: key.unwrap_or(KeyClass::ArrayKey),
        value: all_known.then_some(value).flatten().map(Box::new),
    }
}

fn field_of<'a>(fields: &'a [Field], k: &Key) -> Option<&'a Field> {
    fields.iter().find(|(fk, _, _)| fk == k)
}

/// A singleton cover is presence: promote the key to
/// `Required { witnessed: true }`. A proven-`Absent` key (or a sealed-tail
/// rejection) contradicts the cover, so it's dropped instead (widening).
fn promote_present(fields: &mut Vec<Field>, k: &Key, tail: &Tail) {
    match fields.iter_mut().find(|(fk, _, _)| fk == k) {
        Some((_, p @ (Presence::Optional | Presence::Required { .. }), _)) => {
            *p = Presence::Required { witnessed: true };
        }
        Some(_) => {}
        None => {
            if matches!(tail, Tail::Unsealed { .. }) {
                fields.push((k.clone(), Presence::Required { witnessed: true }, None));
            }
        }
    }
}

/// Keep only the minimal covers, deterministically ordered.
fn antichain(mut covers: Vec<Cover>) -> Vec<Cover> {
    covers.sort();
    covers.dedup();
    let mut out: Vec<Cover> = Vec::with_capacity(covers.len());
    for c in &covers {
        if covers.iter().any(|o| o != c && o.subsumes(c)) {
            continue;
        }
        out.push(c.clone());
    }
    out
}

/// A caller's flag may sharpen `is_list` (e.g. a lifted literal's real
/// order); never contradict it — the computed value wins on conflict.
fn sharpen_is_list(computed: Certainty, given: Certainty) -> Certainty {
    match computed {
        Certainty::Maybe => given,
        c => c,
    }
}

/// **Denotational `is_list`** (§3, RFC #14939). `Yes` iff every admitted
/// value passes [`array_is_list`], `No` iff none does, `Maybe` otherwise.
/// `array{…}` is order-agnostic, so ≥2 keys admit multiple insertion orders —
/// why `array{0: T, 1: U}` is `Maybe`, not `Yes`.
fn compute_is_list(fields: &[Field], tail: &Tail, non_empty: bool) -> Certainty {
    let present: Vec<&Key> = fields
        .iter()
        .filter(|(_, p, _)| !matches!(p, Presence::Absent))
        .map(|(k, _, _)| k)
        .collect();
    // A list iff no undeclared key can appear and at most key `0` is present.
    if matches!(tail, Tail::Sealed)
        && (present.is_empty() || (present.len() == 1 && *present[0] == Key::Int(0)))
    {
        return Certainty::Yes;
    }
    if list_is_admitted(fields, tail, non_empty) { Certainty::Maybe } else { Certainty::No }
}

/// Is *some* admitted value a list? The witness is the minimal key set
/// containing every `Required` key and filling the gaps below the largest of
/// them.
fn list_is_admitted(fields: &[Field], tail: &Tail, non_empty: bool) -> bool {
    let tail_supplies_int = matches!(tail, Tail::Unsealed { key, .. } if key.admits_int());
    let available = |i: i64| {
        fields
            .iter()
            .any(|(k, p, _)| *k == Key::Int(i) && !matches!(p, Presence::Absent))
    };

    let mut max = -1i64;
    for (k, _, _) in fields.iter().filter(|(_, p, _)| p.is_required()) {
        match k {
            // A required string key can never be part of a list.
            Key::Str(_) => return false,
            Key::Int(i) => {
                if *i < 0 {
                    return false;
                }
                max = max.max(*i);
            }
        }
    }

    if max < 0 {
        // Nothing required: the empty array is a list unless `non_empty`
        // forbids it, in which case key `0` has to come from somewhere.
        return !non_empty || tail_supplies_int || available(0);
    }
    if tail_supplies_int {
        return true;
    }
    // Cheap bound: filling `0..=max` needs that many declared keys — avoids
    // walking sparse gaps like `array{1000000: T}`.
    let Ok(span) = usize::try_from(max) else { return false };
    if span.saturating_add(1) > fields.len() {
        return false;
    }
    (0..=max).all(available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Base;

    fn k(i: i64) -> Key {
        Key::Int(i)
    }

    fn ks(s: &str) -> Key {
        Key::Str(s.into())
    }

    fn req() -> Presence {
        Presence::Required { witnessed: false }
    }

    fn slot(f: Fact) -> Option<Box<Fact>> {
        Some(Box::new(f))
    }

    fn int_slot(i: i64) -> Option<Box<Fact>> {
        slot(Fact::Singleton(Val::Int(i)))
    }

    fn sealed(fields: Vec<Field>) -> ShapeFact {
        ShapeFact::normalize(fields, Tail::Sealed, Certainty::Maybe, false, Vec::new())
    }

    fn arr(entries: Vec<(Key, Val)>) -> Vec<(Key, Val)> {
        entries
    }

    // The denotational is_list table (ADR-0062 §3, RFC #14939)

    #[test]
    fn is_list_row_empty_shape_is_yes() {
        assert_eq!(sealed(vec![]).is_list, Certainty::Yes);
    }

    #[test]
    fn is_list_row_single_zero_required_is_yes() {
        assert_eq!(sealed(vec![(k(0), req(), int_slot(1))]).is_list, Certainty::Yes);
    }

    #[test]
    fn is_list_row_single_zero_optional_is_yes() {
        assert_eq!(
            sealed(vec![(k(0), Presence::Optional, int_slot(1))]).is_list,
            Certainty::Yes
        );
    }

    #[test]
    fn is_list_row_two_sequential_keys_is_maybe() {
        // Two realizable insertion orders; only one of them is a list.
        assert_eq!(
            sealed(vec![(k(0), req(), int_slot(1)), (k(1), req(), int_slot(2))]).is_list,
            Certainty::Maybe
        );
    }

    #[test]
    fn is_list_row_optional_string_key_is_maybe() {
        assert_eq!(
            sealed(vec![(ks("a"), Presence::Optional, int_slot(1))]).is_list,
            Certainty::Maybe
        );
    }

    #[test]
    fn is_list_row_required_string_key_is_no() {
        assert_eq!(sealed(vec![(ks("a"), req(), int_slot(1))]).is_list, Certainty::No);
    }

    #[test]
    fn is_list_row_gapped_required_int_key_is_no() {
        assert_eq!(sealed(vec![(k(1), req(), int_slot(2))]).is_list, Certainty::No);
    }

    #[test]
    fn is_list_gap_fillable_by_an_optional_key_is_maybe() {
        // `array{0?: T, 1: U}` admits `[0 => …, 1 => …]`, which is a list.
        assert_eq!(
            sealed(vec![(k(0), Presence::Optional, int_slot(1)), (k(1), req(), int_slot(2))])
                .is_list,
            Certainty::Maybe
        );
    }

    #[test]
    fn is_list_unsealed_tail_is_never_yes() {
        let s = ShapeFact::normalize(
            vec![(k(0), req(), int_slot(1))],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(s.is_list, Certainty::Maybe);
    }

    #[test]
    fn is_list_gap_fillable_by_an_int_tail_is_maybe() {
        let s = ShapeFact::normalize(
            vec![(k(1), req(), int_slot(2))],
            Tail::Unsealed { key: KeyClass::Int, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(s.is_list, Certainty::Maybe);
    }

    #[test]
    fn is_list_gap_unfillable_by_a_string_tail_is_no() {
        let s = ShapeFact::normalize(
            vec![(k(1), req(), int_slot(2))],
            Tail::Unsealed { key: KeyClass::Str, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(s.is_list, Certainty::No);
    }

    #[test]
    fn is_list_negative_required_key_is_no() {
        assert_eq!(sealed(vec![(k(-1), req(), int_slot(2))]).is_list, Certainty::No);
    }

    #[test]
    fn is_list_non_empty_with_only_a_string_optional_is_no() {
        let s = ShapeFact::normalize(
            vec![(ks("a"), Presence::Optional, int_slot(1))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(s.is_list, Certainty::No);
    }

    #[test]
    fn is_list_plain_array_is_maybe() {
        assert_eq!(ShapeFact::plain_array().is_list, Certainty::Maybe);
    }

    #[test]
    fn is_list_sparse_required_key_does_not_walk_the_gap() {
        assert_eq!(sealed(vec![(k(1_000_000), req(), int_slot(2))]).is_list, Certainty::No);
    }

    #[test]
    fn a_caller_flag_sharpens_maybe_but_never_contradicts() {
        let list_of = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(Fact::General { base: Base::Int, nullable: false }) },
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(list_of.is_list, Certainty::Yes);
        let contradicted = ShapeFact::normalize(
            vec![(ks("a"), req(), int_slot(1))],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(contradicted.is_list, Certainty::No);
    }

    // normalize invariants

    #[test]
    fn normalize_sorts_fields_ints_before_strings() {
        let s = sealed(vec![
            (ks("b"), req(), None),
            (k(2), req(), None),
            (ks("a"), req(), None),
            (k(-1), req(), None),
        ]);
        let keys: Vec<&Key> = s.fields.iter().map(|(k, _, _)| k).collect();
        assert_eq!(keys, vec![&k(-1), &k(2), &ks("a"), &ks("b")]);
    }

    #[test]
    fn normalize_keeps_one_entry_per_key() {
        let s = sealed(vec![(k(0), req(), int_slot(1)), (k(0), Presence::Optional, int_slot(2))]);
        assert_eq!(s.fields.len(), 1);
        assert_eq!(s.fields[0].1, req());
    }

    #[test]
    fn normalize_drops_absent_fields_under_a_sealed_tail() {
        let s = sealed(vec![(k(0), req(), None), (ks("gone"), Presence::Absent, None)]);
        assert_eq!(s.fields.len(), 1);
        assert!(s.field(&ks("gone")).is_none());
    }

    #[test]
    fn normalize_keeps_absent_fields_under_an_unsealed_tail() {
        let s = ShapeFact::normalize(
            vec![(ks("gone"), Presence::Absent, None)],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(s.field(&ks("gone")).map(|(_, p, _)| *p), Some(Presence::Absent));
    }

    #[test]
    fn normalize_promotes_a_singleton_cover_to_presence() {
        let s = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, int_slot(1))],
            vec![Cover::new(vec![ks("a")], CoverFlavor::Isset)],
        );
        assert!(s.covers.is_empty());
        assert_eq!(
            s.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
    }

    #[test]
    fn normalize_drops_a_cover_containing_a_required_key() {
        let s = sealed_with_covers(
            vec![(ks("a"), req(), None), (ks("b"), Presence::Optional, None)],
            vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::KeyExists)],
        );
        assert!(s.covers.is_empty());
    }

    #[test]
    fn normalize_keeps_covers_an_antichain() {
        let s = sealed_with_covers(
            vec![
                (ks("a"), Presence::Optional, None),
                (ks("b"), Presence::Optional, None),
                (ks("c"), Presence::Optional, None),
            ],
            vec![
                Cover::new(vec![ks("a"), ks("b"), ks("c")], CoverFlavor::KeyExists),
                Cover::new(vec![ks("b"), ks("a")], CoverFlavor::KeyExists),
            ],
        );
        assert_eq!(s.covers, vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::KeyExists)]);
    }

    #[test]
    fn normalize_prefers_the_isset_cover_over_the_same_keyexists_cover() {
        let s = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, None), (ks("b"), Presence::Optional, None)],
            vec![
                Cover::new(vec![ks("a"), ks("b")], CoverFlavor::KeyExists),
                Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset),
            ],
        );
        assert_eq!(s.covers, vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset)]);
    }

    #[test]
    fn normalize_dedupes_and_sorts_cover_keys() {
        let s = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, None), (ks("b"), Presence::Optional, None)],
            vec![Cover::new(vec![ks("b"), ks("a"), ks("b")], CoverFlavor::Isset)],
        );
        assert_eq!(s.covers[0].keys, vec![ks("a"), ks("b")]);
    }

    #[test]
    fn normalize_implies_non_empty_from_a_required_field() {
        assert!(sealed(vec![(k(0), req(), None)]).non_empty);
        assert!(!sealed(vec![(k(0), Presence::Optional, None)]).non_empty);
    }

    fn sealed_with_covers(fields: Vec<Field>, covers: Vec<Cover>) -> ShapeFact {
        ShapeFact::normalize(fields, Tail::Sealed, Certainty::Maybe, false, covers)
    }

    // admits

    #[test]
    fn admits_required_and_optional_fields() {
        let s = sealed(vec![
            (ks("a"), req(), int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(2))])));
        assert!(!s.admits(&arr(vec![(ks("b"), Val::Int(2))])));
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(9))])));
    }

    #[test]
    fn admits_an_unknown_slot_accepts_any_value() {
        let s = sealed(vec![(ks("a"), req(), None)]);
        assert!(s.admits(&arr(vec![(ks("a"), Val::Null)])));
        assert!(s.admits(&arr(vec![(ks("a"), Val::Array(vec![]))])));
    }

    // The count accessory (issue #272)

    /// `array<array-key, mixed>` with a learned count interval.
    fn counted(lo: i64, hi: i64) -> ShapeFact {
        ShapeFact::plain_array().narrow_count(IntRange::new(lo, hi).expect("ordered"))
    }

    #[test]
    fn narrow_count_meets_and_is_read_through_count_range() {
        assert_eq!(counted(2, 5).count_range(), IntRange::new(2, 5).expect("ordered"));
        // A second guard meets with the first rather than replacing it.
        assert_eq!(
            counted(2, 5).narrow_count(IntRange::new(3, 9).expect("ordered")).count_range(),
            IntRange::new(3, 5).expect("ordered")
        );
    }

    #[test]
    fn a_floor_of_one_is_non_empty_and_nothing_else() {
        let s = counted(1, i64::MAX);
        assert!(s.non_empty);
        assert!(s.fields.is_empty());
        assert!(!counted(0, 3).non_empty);
    }

    #[test]
    fn the_accessory_is_clamped_to_the_non_negative_ints() {
        assert_eq!(
            ShapeFact::plain_array().narrow_count(IntRange::FULL).count_bound,
            IntRange::NON_NEGATIVE
        );
        assert_eq!(counted(i64::MIN, 3).count_range(), IntRange::new(0, 3).expect("ordered"));
    }

    #[test]
    fn a_contradicting_count_widens_rather_than_bottoming() {
        // A guard claiming five contradicts the exact two entries; structural stands.
        let two = sealed(vec![(ks("a"), req(), None), (ks("b"), req(), None)]);
        assert_eq!(two.narrow_count(IntRange::point(5)).count_range(), IntRange::point(2));
    }

    #[test]
    fn a_sealed_shape_pins_its_optional_keys_once_the_floor_exhausts_them() {
        let s = ShapeFact::normalize(
            vec![(k(0), req(), None), (k(1), Presence::Optional, None)],
            Tail::Sealed,
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        let pinned = s.narrow_count(IntRange::new(2, i64::MAX).expect("ordered"));
        assert!(pinned.field(&k(1)).expect("declared").1.is_required());
        assert_eq!(pinned.count_range(), IntRange::point(2));
        // An unsealed tail pins nothing: a floor bounds count only, not keys.
        let open = counted(2, i64::MAX);
        assert!(open.fields.is_empty());
    }

    #[test]
    fn the_accessory_joins_by_hull_and_a_side_that_learned_nothing_absorbs() {
        assert_eq!(
            counted(1, 3).join(&counted(5, 8)).count_range(),
            IntRange::new(1, 8).expect("ordered")
        );
        assert_eq!(
            counted(1, 3).join(&ShapeFact::plain_array()).count_range(),
            IntRange::NON_NEGATIVE
        );
    }

    #[test]
    fn the_accessory_is_extensional_in_admits() {
        let s = counted(2, 3);
        assert!(!s.admits(&arr(vec![(k(0), Val::Int(1))])));
        assert!(s.admits(&arr(vec![(k(0), Val::Int(1)), (k(1), Val::Int(2))])));
        assert!(!s.admits(&arr(vec![
            (k(0), Val::Int(1)),
            (k(1), Val::Int(2)),
            (k(2), Val::Int(3)),
            (k(3), Val::Int(4)),
        ])));
    }

    #[test]
    fn a_write_drops_the_ceiling_and_an_unset_drops_the_floor() {
        let s = counted(2, 3);
        assert_eq!(
            s.relax_count_ceiling().count_range(),
            IntRange::new(2, i64::MAX).expect("ordered")
        );
        assert_eq!(s.mark_absent(&ks("gone")).count_range(), IntRange::new(0, 3).expect("ordered"));
        assert_eq!(
            s.promote_present(&ks("a"), false, true).count_range(),
            IntRange::new(2, 3).expect("ordered")
        );
    }

    #[test]
    fn admits_absent_field_rejects_the_key() {
        let s = ShapeFact::normalize(
            vec![(ks("a"), Presence::Absent, None)],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert!(s.admits(&arr(vec![(ks("b"), Val::Int(1))])));
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1))])));
    }

    #[test]
    fn admits_sealed_rejects_undeclared_keys() {
        let s = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("x"), Val::Int(0))])));
    }

    #[test]
    fn admits_unsealed_checks_the_tail_key_class_and_value() {
        // `array{a: int, ...<string, int>}` rejects `['a' => 1, 9 => 2]` (ADR-0062 §1).
        let s = ShapeFact::normalize(
            vec![(ks("a"), req(), slot(Fact::General { base: Base::Int, nullable: false }))],
            Tail::Unsealed {
                key: KeyClass::Str,
                value: slot(Fact::General { base: Base::Int, nullable: false }),
            },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(2))])));
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1)), (k(9), Val::Int(2))])));
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Null)])));
    }

    #[test]
    fn admits_enforces_the_is_list_verdict() {
        let yes = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: None },
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert!(yes.admits(&arr(vec![(k(0), Val::Int(1)), (k(1), Val::Int(2))])));
        assert!(!yes.admits(&arr(vec![(k(1), Val::Int(2)), (k(0), Val::Int(1))])));

        let no = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::No,
            false,
            Vec::new(),
        );
        assert!(!no.admits(&arr(vec![(k(0), Val::Int(1))])));
        assert!(no.admits(&arr(vec![(ks("a"), Val::Int(1))])));
    }

    #[test]
    fn admits_enforces_non_empty() {
        let s = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert!(!s.admits(&[]));
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1))])));
    }

    #[test]
    fn admits_enforces_covers_by_flavor() {
        let fields = vec![
            (ks("a"), Presence::Optional, None),
            (ks("b"), Presence::Optional, None),
        ];
        let isset = sealed_with_covers(
            fields.clone(),
            vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset)],
        );
        let exists = sealed_with_covers(
            fields,
            vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::KeyExists)],
        );
        assert!(!isset.admits(&[]));
        assert!(!exists.admits(&[]));
        assert!(isset.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(exists.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(!isset.admits(&arr(vec![(ks("a"), Val::Null)])));
        assert!(exists.admits(&arr(vec![(ks("a"), Val::Null)])));
    }

    #[test]
    fn admits_ignores_the_presence_witness_bit() {
        let witnessed = sealed(vec![(ks("a"), Presence::Required { witnessed: true }, int_slot(1))]);
        let declared = sealed(vec![(ks("a"), Presence::Required { witnessed: false }, int_slot(1))]);
        let v = arr(vec![(ks("a"), Val::Int(1))]);
        assert_eq!(witnessed.admits(&v), declared.admits(&v));
    }

    // count_range (ADR-0062 §4)

    #[test]
    fn count_of_a_sealed_all_required_shape_is_exact() {
        let s = sealed(vec![(ks("x"), req(), int_slot(1)), (ks("y"), req(), int_slot(2))]);
        assert_eq!(s.count_range(), IntRange::new(2, 2).expect("ordered"));
    }

    #[test]
    fn count_of_the_empty_sealed_shape_is_exactly_zero() {
        assert_eq!(sealed(vec![]).count_range(), IntRange::point(0));
    }

    #[test]
    fn count_of_a_sealed_shape_with_optionals_spans_required_to_declared() {
        let s = sealed(vec![
            (ks("a"), req(), int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        assert_eq!(s.count_range(), IntRange::new(1, 2).expect("ordered"));
    }

    #[test]
    fn count_of_an_unsealed_shape_tops_out_at_the_domain_max() {
        let s = ShapeFact::normalize(
            vec![(ks("a"), req(), int_slot(1))],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(s.count_range(), IntRange::POSITIVE);
        assert_eq!(ShapeFact::plain_array().count_range(), IntRange::NON_NEGATIVE);
    }

    #[test]
    fn count_of_a_non_empty_optional_only_shape_floors_at_one() {
        // `non-empty-array{a?: T}` admits exactly `['a' => …]`.
        let s = ShapeFact::normalize(
            vec![(ks("a"), Presence::Optional, int_slot(1))],
            Tail::Sealed,
            Certainty::Maybe,
            true,
            Vec::new(),
        );
        assert_eq!(s.count_range(), IntRange::point(1));
    }

    #[test]
    fn count_floor_respects_a_cover() {
        let s = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, None), (ks("b"), Presence::Optional, None)],
            vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset)],
        );
        assert_eq!(s.count_range(), IntRange::new(1, 2).expect("ordered"));
    }

    // lift

    #[test]
    fn lift_makes_every_entry_required_and_witnessed() {
        let entries = arr(vec![(k(0), Val::Int(1)), (k(1), Val::Str("x".into()))]);
        let s = ShapeFact::lift(&entries);
        assert_eq!(s.tail, Tail::Sealed);
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(s.non_empty);
        assert!(s.covers.is_empty());
        assert!(s.fields.iter().all(|(_, p, _)| *p == Presence::Required { witnessed: true }));
        assert!(s.admits(&entries));
    }

    #[test]
    fn lift_keeps_a_nested_array_as_a_singleton_slot() {
        let inner = Val::Array(vec![(k(0), Val::Int(7))]);
        let s = ShapeFact::lift(&arr(vec![(ks("a"), inner.clone())]));
        assert_eq!(s.field(&ks("a")).and_then(|(_, _, v)| v.clone()), Some(Box::new(Fact::Singleton(inner))));
    }

    #[test]
    fn lift_of_the_empty_array_is_the_empty_shape() {
        let s = ShapeFact::lift(&[]);
        assert!(s.fields.is_empty());
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(!s.non_empty);
        assert!(s.admits(&[]));
    }

    #[test]
    fn lift_beyond_the_width_limit_degrades_to_a_tail_summary() {
        let entries: Vec<(Key, Val)> = (0..=(SHAPE_WIDTH_LIMIT as i64))
            .map(|i| (Key::Int(i), Val::Int(i)))
            .collect();
        let s = ShapeFact::lift(&entries);
        assert!(s.fields.is_empty(), "the width bound degrades to the tail");
        assert!(matches!(s.tail, Tail::Unsealed { key: KeyClass::Int, .. }));
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(s.non_empty);
        assert!(s.admits(&entries));
    }

    #[test]
    fn lift_at_the_width_limit_still_keeps_fields() {
        let entries: Vec<(Key, Val)> =
            (0..(SHAPE_WIDTH_LIMIT as i64)).map(|i| (Key::Int(i), Val::Int(i))).collect();
        let s = ShapeFact::lift(&entries);
        assert_eq!(s.fields.len(), SHAPE_WIDTH_LIMIT);
    }

    // from_witnessed_entries (issue #327)

    #[test]
    fn an_unknown_value_costs_that_value_and_nothing_else() {
        // `['p' => 1, 'q' => $s]` where `$s` is a plain `string` parameter.
        let s = ShapeFact::from_witnessed_entries(&[
            (ks("p"), Some(Fact::Singleton(Val::Int(1)))),
            (ks("q"), Some(Fact::General { base: Base::String, nullable: false })),
        ]);
        assert_eq!(s.tail, Tail::Sealed);
        assert!(s.non_empty);
        assert_eq!(s.count_range(), IntRange::point(2));
        assert_eq!(s.is_list, Certainty::No);
        assert!(s.fields.iter().all(|(_, p, _)| *p == Presence::Required { witnessed: true }));
        assert!(s.admits(&arr(vec![(ks("p"), Val::Int(1)), (ks("q"), Val::Str("x".into()))])));
        assert!(!s.admits(&arr(vec![(ks("p"), Val::Int(2)), (ks("q"), Val::Str("x".into()))])));
    }

    #[test]
    fn a_slot_nothing_proved_is_unknown_not_absent() {
        let s = ShapeFact::from_witnessed_entries(&[(ks("k"), None)]);
        assert_eq!(s.field(&ks("k")).map(|(_, p, _)| *p), Some(Presence::Required { witnessed: true }));
        assert_eq!(s.field(&ks("k")).and_then(|(_, _, v)| v.clone()), None);
        assert!(s.admits(&arr(vec![(ks("k"), Val::Int(1))])));
        assert!(s.admits(&arr(vec![(ks("k"), Val::Null)])));
        assert!(!s.admits(&[]));
        assert_eq!(s.count_range(), IntRange::point(1));
    }

    #[test]
    fn is_list_reads_the_witnessed_sequence_not_the_key_set() {
        let listy = ShapeFact::from_witnessed_entries(&[(k(0), None), (k(1), None)]);
        assert_eq!(listy.is_list, Certainty::Yes);
        assert_eq!(listy.witnessed_order(), Some([k(0), k(1)].as_slice()));
        // Same key set, reverse order — not a list; sorted `fields` can't tell these apart.
        let backwards = ShapeFact::from_witnessed_entries(&[(k(1), None), (k(0), None)]);
        assert_eq!(backwards.is_list, Certainty::No);
        assert_eq!(backwards.witnessed_order(), Some([k(1), k(0)].as_slice()));
    }

    #[test]
    fn an_over_wide_literal_degrades_to_the_tail_summary() {
        let entries: Vec<(Key, Option<Fact>)> = (0..=(SHAPE_WIDTH_LIMIT as i64))
            .map(|i| (Key::Int(i), Some(Fact::Singleton(Val::Int(i)))))
            .collect();
        let s = ShapeFact::from_witnessed_entries(&entries);
        assert!(s.fields.is_empty());
        assert!(matches!(s.tail, Tail::Unsealed { key: KeyClass::Int, .. }));
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(s.non_empty);
        assert_eq!(s.order, None);
    }

    #[test]
    fn one_unknown_slot_makes_the_over_wide_tail_value_unknown() {
        let mut entries: Vec<(Key, Option<Fact>)> = (0..=(SHAPE_WIDTH_LIMIT as i64))
            .map(|i| (Key::Int(i), Some(Fact::Singleton(Val::Int(i)))))
            .collect();
        entries[3].1 = None;
        let s = ShapeFact::from_witnessed_entries(&entries);
        assert!(matches!(s.tail, Tail::Unsealed { value: None, .. }));
    }

    #[test]
    fn the_empty_literal_is_the_empty_shape() {
        let s = ShapeFact::from_witnessed_entries(&[]);
        assert!(s.fields.is_empty());
        assert_eq!(s.tail, Tail::Sealed);
        assert_eq!(s.is_list, Certainty::Yes);
        assert!(!s.non_empty);
        assert!(s.admits(&[]));
    }

    #[test]
    fn a_fully_proven_literal_agrees_with_lift() {
        let entries = arr(vec![(ks("b"), Val::Int(1)), (ks("a"), Val::Str("x".into()))]);
        let slots: Vec<(Key, Option<Fact>)> =
            entries.iter().map(|(k, v)| (k.clone(), Some(Fact::Singleton(v.clone())))).collect();
        assert_eq!(ShapeFact::from_witnessed_entries(&slots), ShapeFact::lift(&entries));
    }

    // the order witness (issue #327)

    #[test]
    fn lift_records_the_order_it_saw_not_the_canonical_one() {
        // Order is the REVERSE of canonical sort — why the witness exists apart from `fields`.
        let s = ShapeFact::lift(&arr(vec![(ks("b"), Val::Int(1)), (ks("a"), Val::Int(2))]));
        assert_eq!(s.fields.iter().map(|(k, _, _)| k.clone()).collect::<Vec<_>>(), vec![ks("a"), ks("b")]);
        assert_eq!(s.witnessed_order(), Some([ks("b"), ks("a")].as_slice()));
    }

    #[test]
    fn a_shape_that_witnessed_nothing_has_no_order() {
        // Declared order means nothing about runtime order (phpstan/phpstan#14940).
        let s = sealed(vec![
            (ks("b"), Presence::Required { witnessed: false }, int_slot(1)),
            (ks("a"), Presence::Required { witnessed: false }, int_slot(2)),
        ]);
        assert_eq!(s.order, None);
        assert_eq!(s.witnessed_order(), None);
    }

    #[test]
    fn every_derived_shape_loses_the_witness() {
        let a = ShapeFact::lift(&arr(vec![(ks("b"), Val::Int(1)), (ks("a"), Val::Int(2))]));
        let b = ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(2)), (ks("b"), Val::Int(1))]));
        // Every rebuild drops the witness, agreeing or not.
        assert_eq!(a.join(&b).order, None);
        assert_eq!(a.join(&a).order, None);
        assert_eq!(a.set_non_empty().order, None);
        assert_eq!(a.mark_absent(&ks("a")).order, None);
        assert_eq!(a.promote_present(&ks("a"), false, true).order, None);
        assert_eq!(a.set_is_list(Certainty::No).order, None);
        // Bypasses `normalize_counted` by hand — models a write that can append a key.
        assert_eq!(a.relax_count_ceiling().order, None);
    }

    #[test]
    fn an_inconsistent_witness_is_refused_rather_than_stored() {
        let s = sealed(vec![(ks("a"), Presence::Required { witnessed: true }, int_slot(1))]);
        assert_eq!(s.clone().with_order(vec![ks("zzz")]).order, None);
        assert_eq!(s.clone().with_order(vec![ks("a"), ks("a")]).order, None);
        assert_eq!(s.clone().with_order(Vec::new()).order, None);
        // An unsealed tail has keys outside any sequence, so there is none.
        let open = ShapeFact::normalize(
            vec![(ks("a"), Presence::Required { witnessed: true }, int_slot(1))],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::No,
            true,
            Vec::new(),
        );
        assert_eq!(open.with_order(vec![ks("a")]).order, None);
        assert_eq!(s.with_order(vec![ks("a")]).order, Some(vec![ks("a")]));
    }

    #[test]
    fn an_optional_field_has_no_single_sequence() {
        // `list{int, 1?: string}`: 1 or 2 entries — witness kept, `witnessed_order` declines.
        let s = ShapeFact::normalize(
            vec![
                (k(0), Presence::Required { witnessed: true }, int_slot(1)),
                (k(1), Presence::Optional, int_slot(2)),
            ],
            Tail::Sealed,
            Certainty::Yes,
            true,
            Vec::new(),
        )
        .with_order(vec![k(0), k(1)]);
        assert!(s.order.is_some());
        assert_eq!(s.witnessed_order(), None);
    }

    #[test]
    fn the_witness_is_extensionally_inert() {
        // Two shapes differing only in the witness admit the same values —
        // `admits` ignores it.
        let entries = arr(vec![(ks("b"), Val::Int(1)), (ks("a"), Val::Int(2))]);
        let witnessed = ShapeFact::lift(&entries);
        let mut bare = witnessed.clone();
        bare.order = None;
        assert!(witnessed.admits(&entries));
        assert!(bare.admits(&entries));
        assert_eq!(witnessed.count_range(), bare.count_range());
        assert_eq!(witnessed.is_list, bare.is_list);
    }

    // join

    #[test]
    fn join_sealed_disjoint_keys_absorbs_into_optionality() {
        let a = ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(1))]));
        let b = ShapeFact::lift(&arr(vec![(ks("b"), Val::Int(2))]));
        let j = a.join(&b);
        assert_eq!(j.tail, Tail::Sealed);
        assert_eq!(j.field(&ks("a")).map(|(_, p, _)| *p), Some(Presence::Optional));
        assert_eq!(j.field(&ks("b")).map(|(_, p, _)| *p), Some(Presence::Optional));
        // Neither key required now, but both were non-empty — join still knows one is there.
        assert!(j.non_empty);
        assert!(!j.admits(&[]));
        assert!(j.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(j.admits(&arr(vec![(ks("b"), Val::Int(2))])));
    }

    #[test]
    fn join_required_on_both_sides_keeps_the_lower_stratum() {
        let a = sealed(vec![(ks("a"), Presence::Required { witnessed: true }, int_slot(1))]);
        let b = sealed(vec![(ks("a"), Presence::Required { witnessed: false }, int_slot(2))]);
        let j = a.join(&b);
        assert_eq!(
            j.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: false })
        );
    }

    #[test]
    fn join_absent_with_required_is_optional() {
        let a = ShapeFact::normalize(
            vec![(ks("a"), Presence::Absent, None)],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        let b = ShapeFact::normalize(
            vec![(ks("a"), req(), int_slot(1))],
            Tail::Unsealed { key: KeyClass::ArrayKey, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(a.join(&b).field(&ks("a")).map(|(_, p, _)| *p), Some(Presence::Optional));
        assert_eq!(a.join(&a).field(&ks("a")).map(|(_, p, _)| *p), Some(Presence::Absent));
    }

    #[test]
    fn join_slots_join_and_unknown_absorbs() {
        let a = sealed(vec![(ks("a"), req(), int_slot(1))]);
        let b = sealed(vec![(ks("a"), req(), int_slot(2))]);
        let j = a.join(&b);
        assert_eq!(
            j.field(&ks("a")).and_then(|(_, _, s)| s.clone()),
            Some(Box::new(Fact::OneOf(vec![Val::Int(1), Val::Int(2)])))
        );
        let unknown = sealed(vec![(ks("a"), req(), None)]);
        assert_eq!(a.join(&unknown).field(&ks("a")).and_then(|(_, _, s)| s.clone()), None);
    }

    #[test]
    fn join_tails_follow_a_g5() {
        let sealed_a = sealed(vec![(k(0), req(), int_slot(1))]);
        let unsealed = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Str, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert_eq!(sealed_a.join(&sealed_a).tail, Tail::Sealed);
        assert!(matches!(sealed_a.join(&unsealed).tail, Tail::Unsealed { key: KeyClass::Str, .. }));
        let int_tail = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: None },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        assert!(matches!(
            unsealed.join(&int_tail).tail,
            Tail::Unsealed { key: KeyClass::ArrayKey, .. }
        ));
    }

    #[test]
    fn join_one_sided_key_meets_the_other_tail_bound() {
        let a = sealed(vec![(ks("a"), req(), int_slot(1))]);
        let b = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed {
                key: KeyClass::Str,
                value: slot(Fact::refined(
                    Base::Int,
                    crate::fact::Refinement::Int(IntRange::new(5, 9).expect("ordered")),
                    false,
                )),
            },
            Certainty::Maybe,
            false,
            Vec::new(),
        );
        let j = a.join(&b);
        let s = j.field(&ks("a")).and_then(|(_, _, s)| s.clone()).expect("slot");
        assert!(s.admits(&Val::Int(1)) && s.admits(&Val::Int(7)));
    }

    #[test]
    fn join_is_list_is_the_certainty_join() {
        let list = ShapeFact::lift(&arr(vec![(k(0), Val::Int(1))]));
        let map = ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(1))]));
        assert_eq!(list.join(&list).is_list, Certainty::Yes);
        assert_eq!(map.join(&map).is_list, Certainty::No);
        assert_eq!(list.join(&map).is_list, Certainty::Maybe);
    }

    #[test]
    fn join_non_empty_is_conjunctive() {
        let a = ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(1))]));
        let empty = ShapeFact::lift(&[]);
        assert!(a.join(&a).non_empty);
        assert!(!a.join(&empty).non_empty);
    }

    #[test]
    fn join_keeps_a_cover_only_when_both_sides_imply_it() {
        let cover = Cover::new(vec![ks("a"), ks("b")], CoverFlavor::KeyExists);
        let with = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, None), (ks("b"), Presence::Optional, None)],
            vec![cover.clone()],
        );
        let without = sealed(vec![
            (ks("a"), Presence::Optional, None),
            (ks("b"), Presence::Optional, None),
        ]);
        assert_eq!(with.join(&with).covers, vec![cover.clone()]);
        assert!(with.join(&without).covers.is_empty());

        let required_a = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert!(required_a.implies_cover(&cover));
        assert_eq!(with.join(&required_a).covers, vec![cover]);
    }

    #[test]
    fn a_required_nullable_key_does_not_imply_an_isset_cover() {
        let cover = Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset);
        let unknown_slot = sealed(vec![(ks("a"), req(), None)]);
        assert!(!unknown_slot.implies_cover(&cover));
        let non_null = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert!(non_null.implies_cover(&cover));
        let nullable = sealed(vec![(
            ks("a"),
            req(),
            slot(Fact::General { base: Base::Int, nullable: true }),
        )]);
        assert!(!nullable.implies_cover(&cover));
    }

    #[test]
    fn join_admits_both_operands_denotations() {
        let a = arr(vec![(k(0), Val::Int(1)), (k(1), Val::Int(2))]);
        let b = arr(vec![(k(0), Val::Str("x".into()))]);
        let j = ShapeFact::lift(&a).join(&ShapeFact::lift(&b));
        assert!(j.admits(&a), "join lost the left operand");
        assert!(j.admits(&b), "join lost the right operand");
    }

    // Narrowing operators (S4)

    fn unsealed(fields: Vec<Field>, key: KeyClass, value: Option<Box<Fact>>) -> ShapeFact {
        ShapeFact::normalize(
            fields,
            Tail::Unsealed { key, value },
            Certainty::Maybe,
            false,
            Vec::new(),
        )
    }

    #[test]
    fn promote_present_witnesses_an_optional_field() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        let n = s.promote_present(&ks("a"), true, true);
        assert_eq!(
            n.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
        assert_eq!(n.field(&ks("b")).map(|(_, p, _)| *p), Some(Presence::Optional));
        assert!(n.non_empty);
    }

    #[test]
    fn promote_present_strips_null_only_for_the_isset_flavor() {
        let nullable = slot(Fact::General { base: Base::String, nullable: true });
        let s = sealed(vec![(ks("a"), Presence::Optional, nullable.clone())]);
        let isset = s.promote_present(&ks("a"), true, true);
        assert_eq!(
            isset.field(&ks("a")).and_then(|(_, _, v)| v.clone()),
            slot(Fact::General { base: Base::String, nullable: false })
        );
        let exists = s.promote_present(&ks("a"), false, true);
        assert_eq!(exists.field(&ks("a")).and_then(|(_, _, v)| v.clone()), nullable);
    }

    #[test]
    fn promote_present_carries_the_guards_own_presence_stratum() {
        // ADR-0058: a runtime guard witnesses the key; a docblock-only claim
        // (e.g. a userland `@phpstan-assert` helper) promotes at the
        // declared stratum — provenance only (§3).
        let s = sealed(vec![(ks("a"), Presence::Optional, int_slot(1))]);
        let declared = s.promote_present(&ks("a"), true, false);
        assert_eq!(
            declared.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: false })
        );
        let witnessed = s.promote_present(&ks("a"), true, true);
        assert_eq!(
            witnessed.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
        let v = arr(vec![(ks("a"), Val::Int(1))]);
        assert_eq!(declared.admits(&v), witnessed.admits(&v), "the bit never extends the fact");
        assert_eq!(
            witnessed.promote_present(&ks("a"), true, false).field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
    }

    #[test]
    fn promote_present_of_an_exactly_null_slot_degrades_to_unknown() {
        // `isset` is impossible (no non-null value); unknown is the widening side.
        let s = sealed(vec![(ks("a"), Presence::Optional, slot(Fact::Singleton(Val::Null)))]);
        assert_eq!(s.promote_present(&ks("a"), true, true).field(&ks("a")).and_then(|(_, _, v)| v.clone()), None);
    }

    #[test]
    fn promote_present_adds_an_undeclared_key_from_the_tail_bound() {
        let s = unsealed(
            Vec::new(),
            KeyClass::Str,
            slot(Fact::General { base: Base::Int, nullable: true }),
        );
        let n = s.promote_present(&ks("a"), true, true);
        assert_eq!(
            n.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
        assert_eq!(
            n.field(&ks("a")).and_then(|(_, _, v)| v.clone()),
            slot(Fact::General { base: Base::Int, nullable: false })
        );
        assert!(n.admits(&arr(vec![(ks("a"), Val::Int(1))])));
    }

    #[test]
    fn promote_present_is_a_no_op_where_the_key_cannot_exist() {
        let s = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert_eq!(s.promote_present(&ks("zz"), true, true), s);
        let t = unsealed(Vec::new(), KeyClass::Int, None);
        assert_eq!(t.promote_present(&ks("a"), true, true), t);
    }

    #[test]
    fn promote_present_discharges_a_cover_containing_the_key() {
        let s = sealed_with_covers(
            vec![(ks("a"), Presence::Optional, int_slot(1)), (ks("b"), Presence::Optional, int_slot(2))],
            vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset)],
        );
        assert_eq!(s.covers.len(), 1);
        assert!(s.promote_present(&ks("a"), true, true).covers.is_empty());
    }

    #[test]
    fn promote_present_keeps_a_declared_list_flag() {
        let l = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed {
                key: KeyClass::Int,
                value: slot(Fact::General { base: Base::String, nullable: false }),
            },
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(l.promote_present(&k(0), true, true).is_list, Certainty::Yes);
    }

    #[test]
    fn mark_absent_under_a_sealed_tail_drops_the_field() {
        let s = sealed(vec![
            (ks("a"), req(), slot(Fact::General { base: Base::String, nullable: false })),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        let n = s.mark_absent(&ks("a"));
        assert!(n.field(&ks("a")).is_none(), "sealed already proves the absence");
        assert!(!n.admits(&arr(vec![(ks("a"), Val::Str("x".into()))])));
        // `unset` on `['a' => 'x']` leaves `[]`, a list; `is_list = No` must not survive it.
        assert_eq!(s.is_list, Certainty::No);
        assert_eq!(n.is_list, Certainty::Maybe);
        assert!(n.admits(&[]));
    }

    #[test]
    fn mark_absent_under_an_unsealed_tail_records_the_field() {
        let s = unsealed(Vec::new(), KeyClass::ArrayKey, None);
        let n = s.mark_absent(&ks("a"));
        assert_eq!(n.field(&ks("a")).map(|(_, p, _)| *p), Some(Presence::Absent));
        assert!(!n.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(n.admits(&arr(vec![(ks("b"), Val::Int(1))])));
    }

    #[test]
    fn mark_absent_drops_non_empty_and_re_derives_it() {
        let s = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert!(s.non_empty);
        assert!(!s.mark_absent(&ks("a")).non_empty);
        let two = sealed(vec![(ks("a"), req(), int_slot(1)), (ks("b"), req(), int_slot(2))]);
        assert!(two.mark_absent(&ks("a")).non_empty);
    }

    #[test]
    fn mark_absent_kills_covers_containing_the_key() {
        let s = sealed_with_covers(
            vec![
                (ks("a"), Presence::Optional, int_slot(1)),
                (ks("b"), Presence::Optional, int_slot(2)),
                (ks("c"), Presence::Optional, int_slot(3)),
            ],
            vec![
                Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset),
                Cover::new(vec![ks("b"), ks("c")], CoverFlavor::Isset),
            ],
        );
        assert_eq!(s.covers.len(), 2);
        assert_eq!(
            s.mark_absent(&ks("a")).covers,
            vec![Cover::new(vec![ks("b"), ks("c")], CoverFlavor::Isset)]
        );
    }

    #[test]
    fn mark_absent_is_a_no_op_on_an_undeclared_key_of_a_sealed_shape() {
        let s = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert_eq!(s.mark_absent(&ks("zz")), s);
    }

    #[test]
    fn mark_absent_recomputes_is_list() {
        let s = sealed(vec![(k(0), req(), int_slot(1)), (k(1), req(), int_slot(2))]);
        assert_eq!(s.is_list, Certainty::Maybe);
        assert_eq!(s.mark_absent(&k(1)).is_list, Certainty::Yes);
        assert_eq!(s.mark_absent(&k(0)).is_list, Certainty::No);
    }

    #[test]
    fn set_non_empty_rejects_the_empty_array() {
        let s = sealed(vec![(ks("a"), Presence::Optional, int_slot(1))]);
        assert!(s.admits(&[]));
        let n = s.set_non_empty();
        assert!(n.non_empty);
        assert!(!n.admits(&[]));
        // Now exactly one entry (`if ($t) { count($t) }` → `1`).
        assert_eq!(n.count_range(), IntRange::point(1));
    }

    #[test]
    fn set_non_empty_recomputes_is_list() {
        let s = sealed(vec![(k(0), Presence::Optional, int_slot(1))]);
        assert_eq!(s.is_list, Certainty::Yes);
        assert_eq!(s.set_non_empty().is_list, Certainty::Yes);
        let m = sealed(vec![(ks("a"), Presence::Optional, int_slot(1))]);
        assert_eq!(m.is_list, Certainty::Maybe);
        assert_eq!(m.set_non_empty().is_list, Certainty::No);
    }

    #[test]
    fn set_is_list_flips_the_flag_both_ways() {
        // `array<int, string>`: computed verdict is Maybe, so the flag decides.
        let s = unsealed(
            Vec::new(),
            KeyClass::Int,
            slot(Fact::General { base: Base::String, nullable: false }),
        );
        assert_eq!(s.is_list, Certainty::Maybe);
        assert_eq!(s.set_is_list(Certainty::Yes).is_list, Certainty::Yes);
        assert_eq!(s.set_is_list(Certainty::No).is_list, Certainty::No);
        let yes = s.set_is_list(Certainty::Yes);
        assert!(yes.admits(&arr(vec![(k(0), Val::Str("x".into()))])));
        assert!(!yes.admits(&arr(vec![(k(1), Val::Str("x".into()))])));
    }

    #[test]
    fn set_is_list_never_contradicts_the_computed_verdict() {
        // Required string key can never be a list; flag loses soundly — meet is empty.
        let s = sealed(vec![(ks("a"), req(), int_slot(1))]);
        assert_eq!(s.set_is_list(Certainty::Yes).is_list, Certainty::No);
    }

    fn operator_shape_universe() -> Vec<ShapeFact> {
        vec![
            sealed(vec![]),
            sealed(vec![(ks("a"), Presence::Optional, int_slot(1))]),
            sealed(vec![(ks("a"), req(), int_slot(1)), (ks("b"), Presence::Optional, int_slot(2))]),
            sealed(vec![(k(0), req(), int_slot(1)), (k(1), req(), int_slot(2))]),
            sealed(vec![(k(0), Presence::Optional, int_slot(1))]),
            unsealed(Vec::new(), KeyClass::ArrayKey, None),
            unsealed(vec![(ks("a"), Presence::Optional, None)], KeyClass::Str, None),
            unsealed(Vec::new(), KeyClass::Int, slot(Fact::General { base: Base::Int, nullable: true })),
            sealed(vec![(ks("a"), Presence::Optional, slot(Fact::General { base: Base::Int, nullable: true }))]),
            sealed(vec![(ks("a"), req(), slot(Fact::General { base: Base::String, nullable: false }))]),
            ShapeFact::plain_array(),
        ]
    }

    fn operator_array_universe() -> Vec<Vec<(Key, Val)>> {
        vec![
            vec![],
            vec![(ks("a"), Val::Int(1))],
            vec![(ks("a"), Val::Null)],
            vec![(ks("a"), Val::Str("x".into()))],
            vec![(ks("b"), Val::Int(2))],
            vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(2))],
            vec![(k(0), Val::Int(1))],
            vec![(k(0), Val::Int(1)), (k(1), Val::Int(2))],
            vec![(k(1), Val::Int(2)), (k(0), Val::Int(1))],
        ]
    }

    const OPERATOR_KEYS: [&str; 2] = ["a", "b"];

    fn operator_key_universe() -> Vec<Key> {
        OPERATOR_KEYS.iter().map(|s| ks(s)).chain([k(0), k(1)]).collect()
    }

    /// The narrowing law, checked over the operator vectors: everything the
    /// receiver admits that satisfies the guard survives the operator.
    #[test]
    fn narrowing_operators_admit_every_guard_satisfying_member() {
        let shapes = operator_shape_universe();
        let arrays = operator_array_universe();
        let keys = operator_key_universe();
        for s in &shapes {
            for v in &arrays {
                if !s.admits(v) {
                    continue;
                }
                let entry = |key: &Key| v.iter().find(|(ek, _)| ek == key).map(|(_, val)| val);
                for key in &keys {
                    // isset: present and non-null.
                    if entry(key).is_some_and(|val| *val != Val::Null) {
                        assert!(
                            s.promote_present(key, true, true).admits(v),
                            "promote_present(isset) lost {v:?} from {s:?}"
                        );
                    }
                    // array_key_exists: present.
                    if entry(key).is_some() {
                        assert!(
                            s.promote_present(key, false, true).admits(v),
                            "promote_present(exists) lost {v:?} from {s:?}"
                        );
                    } else {
                        assert!(
                            s.mark_absent(key).admits(v),
                            "mark_absent lost {v:?} from {s:?}"
                        );
                    }
                }
                if !v.is_empty() {
                    assert!(s.set_non_empty().admits(v), "set_non_empty lost {v:?} from {s:?}");
                }
                let want = Certainty::from_bool(array_is_list(v));
                assert!(
                    s.set_is_list(want).admits(v),
                    "set_is_list({want:?}) lost {v:?} from {s:?}"
                );
            }
        }
    }

    /// `mark_absent`'s second law (`unset($x[k])`): admits `v \ {k}` for every
    /// receiver-admitted `v` — why `is_list`/`non_empty` are re-derived, not carried.
    #[test]
    fn mark_absent_admits_every_receiver_member_minus_the_key() {
        for s in &operator_shape_universe() {
            for v in &operator_array_universe() {
                if !s.admits(v) {
                    continue;
                }
                for key in &operator_key_universe() {
                    let removed: Vec<(Key, Val)> =
                        v.iter().filter(|(ek, _)| ek != key).cloned().collect();
                    assert!(
                        s.mark_absent(key).admits(&removed),
                        "unset({key:?}) on {v:?} left {removed:?}, rejected by {s:?}"
                    );
                }
            }
        }
    }

    // record_cover / cover_proves (A-G8 recording, A-G11 discharge) — S5

    #[test]
    fn record_cover_stores_a_two_key_disjunction() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ])
        .record_cover(vec![ks("b"), ks("a")], CoverFlavor::Isset);
        assert_eq!(s.covers, vec![Cover::new(vec![ks("a"), ks("b")], CoverFlavor::Isset)]);
        // Claim implies it: at least one key present, so no admitted array is empty.
        assert!(s.non_empty);
    }

    /// The S2 invariant via the S5 constructor: a singleton is presence, not a disjunction.
    #[test]
    fn record_cover_promotes_a_singleton_instead_of_storing_it() {
        let s = sealed(vec![(ks("a"), Presence::Optional, int_slot(1))])
            .record_cover(vec![ks("a")], CoverFlavor::Isset);
        assert!(s.covers.is_empty());
        assert_eq!(
            s.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
    }

    /// Recording composes with S4: a cover whose key a guard already promoted
    /// normalizes away rather than being carried as a weaker twin.
    #[test]
    fn record_cover_normalizes_away_against_an_already_required_key() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ])
        .promote_present(&ks("a"), true, true)
        .record_cover(vec![ks("a"), ks("b")], CoverFlavor::Isset);
        assert!(s.covers.is_empty());
        assert_eq!(
            s.field(&ks("a")).map(|(_, p, _)| *p),
            Some(Presence::Required { witnessed: true })
        );
    }

    #[test]
    fn record_cover_keeps_three_keys_as_one_cover() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
            (ks("c"), Presence::Optional, int_slot(3)),
        ])
        .record_cover(vec![ks("a"), ks("b"), ks("c")], CoverFlavor::Isset);
        assert_eq!(s.covers.len(), 1);
        assert_eq!(s.covers[0].keys, vec![ks("a"), ks("b"), ks("c")]);
    }

    #[test]
    fn cover_proves_the_last_unrefuted_member() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ])
        .record_cover(vec![ks("a"), ks("b")], CoverFlavor::Isset);
        assert_eq!(s.cover_proves(&ks("b"), &[ks("a")]), Some(CoverFlavor::Isset));
        assert_eq!(s.cover_proves(&ks("a"), &[ks("b")]), Some(CoverFlavor::Isset));
        // Nothing refuted yet: the disjunction proves neither member alone.
        assert_eq!(s.cover_proves(&ks("b"), &[]), None);
        // A key the cover does not mention is never proved by it.
        assert_eq!(s.cover_proves(&ks("c"), &[ks("a")]), None);
    }

    #[test]
    fn cover_proves_needs_every_other_member_refuted() {
        let s = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
            (ks("c"), Presence::Optional, int_slot(3)),
        ])
        .record_cover(vec![ks("a"), ks("b"), ks("c")], CoverFlavor::Isset);
        assert_eq!(s.cover_proves(&ks("c"), &[ks("a")]), None);
        assert_eq!(s.cover_proves(&ks("c"), &[ks("a"), ks("b")]), Some(CoverFlavor::Isset));
    }

    #[test]
    fn cover_proves_reports_the_flavor_and_prefers_isset() {
        let base = sealed(vec![
            (ks("a"), Presence::Optional, int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        let exists = base.record_cover(vec![ks("a"), ks("b")], CoverFlavor::KeyExists);
        assert_eq!(exists.cover_proves(&ks("b"), &[ks("a")]), Some(CoverFlavor::KeyExists));
        // Both claims present: stronger reported; antichain already dropped the weaker.
        let both = exists.record_cover(vec![ks("a"), ks("b")], CoverFlavor::Isset);
        assert_eq!(both.cover_proves(&ks("b"), &[ks("a")]), Some(CoverFlavor::Isset));
    }

    /// The S5 recording law: an array satisfying the disjunction survives the recording.
    #[test]
    fn record_cover_admits_every_member_satisfying_the_disjunction() {
        let pair: Vec<Key> = OPERATOR_KEYS.iter().map(|s| ks(s)).collect();
        for s in &operator_shape_universe() {
            for v in &operator_array_universe() {
                if !s.admits(v) {
                    continue;
                }
                let entry = |key: &Key| v.iter().find(|(ek, _)| ek == key).map(|(_, val)| val);
                let isset = pair.iter().any(|k| entry(k).is_some_and(|val| *val != Val::Null));
                let exists = pair.iter().any(|k| entry(k).is_some());
                if isset {
                    assert!(
                        s.record_cover(pair.clone(), CoverFlavor::Isset).admits(v),
                        "record_cover(isset) lost {v:?} from {s:?}"
                    );
                }
                if exists {
                    assert!(
                        s.record_cover(pair.clone(), CoverFlavor::KeyExists).admits(v),
                        "record_cover(exists) lost {v:?} from {s:?}"
                    );
                }
            }
        }
    }

    /// The discharge law A-G11 rests on: `cover_proves` answering means the key is
    /// present whenever `absent` members are absent-or-null (Isset) / absent (KeyExists).
    #[test]
    fn cover_proves_only_when_the_key_is_really_present() {
        let pair: Vec<Key> = OPERATOR_KEYS.iter().map(|s| ks(s)).collect();
        for s in &operator_shape_universe() {
            for flavor in [CoverFlavor::Isset, CoverFlavor::KeyExists] {
                let covered = s.record_cover(pair.clone(), flavor);
                for v in &operator_array_universe() {
                    if !covered.admits(v) {
                        continue;
                    }
                    let entry =
                        |key: &Key| v.iter().find(|(ek, _)| ek == key).map(|(_, val)| val);
                    let Some(got) = covered.cover_proves(&ks("b"), &[ks("a")]) else {
                        continue;
                    };
                    // Premise: left `??` arms fell through — absent-or-null for
                    // `isset`, absent for `array_key_exists` (given the
                    // caller's non-nullable check).
                    let fell_through = match got {
                        CoverFlavor::Isset => entry(&ks("a")).is_none_or(|val| *val == Val::Null),
                        CoverFlavor::KeyExists => entry(&ks("a")).is_none(),
                    };
                    if !fell_through {
                        continue;
                    }
                    assert!(
                        entry(&ks("b")).is_some(),
                        "cover_proves({got:?}) claimed 'b' present in {v:?} under {covered:?}"
                    );
                    if got == CoverFlavor::Isset {
                        assert!(
                            entry(&ks("b")) != Some(&Val::Null),
                            "isset-cover claimed 'b' non-null in {v:?} under {covered:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn join_is_commutative_on_the_worked_cases() {
        let cases = [
            (
                ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(1))])),
                ShapeFact::lift(&arr(vec![(ks("b"), Val::Int(2))])),
            ),
            (ShapeFact::lift(&arr(vec![(k(0), Val::Int(1))])), ShapeFact::plain_array()),
            (ShapeFact::lift(&[]), ShapeFact::plain_array()),
        ];
        for (a, b) in cases {
            assert_eq!(a.join(&b), b.join(&a));
        }
    }
}
