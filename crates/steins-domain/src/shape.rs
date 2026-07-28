//! The canonical abstract array fact (ADR-0062 §3 and Amendment A).
//!
//! One form covers every array truth the fact lane can hold (A-G1): plain
//! `array` is the degenerate shape (no fields, an untyped unsealed tail),
//! `array<K, V>` types the tail, `list<T>` types the tail and pins `is_list`
//! to `Yes`, and `array{…}`/`list{…}` declare fields. There is deliberately
//! **no** array-`General` variant — the degenerate shape absorbs ADR-0035's
//! layer-4 array text.
//!
//! Three things this module does not do, each by ADR ruling:
//!
//! * **No `ContractTy`.** Field and tail value slots are recursive
//!   `Option<Box<Fact>>` (A-G1a): the contract crate depends on this one, so
//!   naming its vocabulary here would invert the dependency. `None` is the
//!   honest floor — "no fact", which is what a class-typed or callable-typed
//!   field lowers to.
//! * **No next-auto-index.** ADR-0062 §3 declines an abstract
//!   `nextAutoIndexes`; append widens the tail instead.
//! * **No general meet** (A-G7). Narrowing arrives later as targeted
//!   refinement operators.
//!
//! `is_list` is **denotational**, never syntactic (§3, RFC #14939): it is
//! recomputed from the key structure by [`ShapeFact::normalize`], and a
//! caller's own flag can only sharpen the computed answer, never contradict
//! it.
//!
//! Naming note: the ADR writes `VKey` for the field key; this crate already
//! owns that type as [`Key`], and the domain has exactly one key vocabulary.

use crate::certainty::Certainty;
use crate::fact::Fact;
use crate::value::{Key, Val};

/// Field-width bound for a single shape (A-G6).
///
/// This is PHPStan's `ARRAY_COUNT_LIMIT`, imported as-is rather than invented:
/// lifting or seeding a shape with more fields than this degrades to the
/// tail-only summary. Orthogonal to the `OneOf` cap, which governs how many
/// *whole arrays* stay finite; ADR-0062 §7 declines the union-degradation role
/// this constant plays upstream.
pub const SHAPE_WIDTH_LIMIT: usize = 256;

/// Whether a key is in the array, and on what evidence.
///
/// The `witnessed` bit on `Required` is the **presence stratum** (§3): the
/// domain-local form of the Verified/Asserted split. `true` means a runtime
/// guard or observation established presence; `false` means presence is
/// declared. Presence stratum and value stratum are independent (A-G9), and
/// the bit never changes what the fact *admits* — it is provenance, not
/// extension.
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

/// The canonical abstract array fact.
///
/// Built through [`ShapeFact::normalize`] (and the constructors that call it),
/// which is what establishes the invariants:
///
/// * `fields` sorted by key, one entry per key (ints before strings, each
///   ascending — the [`Key`] order);
/// * no `Absent` field under a `Sealed` tail (sealing already proves absence);
/// * `covers` an antichain of key sets of size ≥ 2, none containing a
///   `Required` key, sorted deterministically;
/// * `is_list` denotationally computed from the key structure, sharpened but
///   never contradicted by the caller's flag;
/// * `non_empty` implied by any `Required` field.
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
}

/// PHP's `array_is_list`: the keys are exactly `0..n-1`, in that order.
///
/// Sound **only** on the value lane, which is order-witnessed (ADR-0062 §2) —
/// this is the one place the domain reads insertion order.
#[must_use]
pub fn array_is_list(entries: &[(Key, Val)]) -> bool {
    entries.iter().enumerate().all(|(i, (k, _))| match k {
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
        mut fields: Vec<Field>,
        tail: Tail,
        is_list: Certainty,
        non_empty: bool,
        covers: Vec<Cover>,
    ) -> ShapeFact {
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        fields.dedup_by(|a, b| a.0 == b.0);

        // A singleton cover is not a disjunction — it *is* presence, so it is
        // promoted rather than dropped. An empty cover claims nothing
        // satisfiable; dropping it widens, which is the safe side.
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

        // A sealed tail already proves absence, so an `Absent` field carries no
        // information the shape does not have twice.
        if matches!(tail, Tail::Sealed) {
            fields.retain(|(_, p, _)| !matches!(p, Presence::Absent));
        }

        // A cover with a `Required` member is discharged.
        kept.retain(|c| !c.keys.iter().any(|k| field_of(&fields, k).is_some_and(|(_, p, _)| p.is_required())));
        let covers = antichain(kept);

        let computed = compute_is_list(&fields, &tail, non_empty);
        let is_list = sharpen_is_list(computed, is_list);
        let non_empty = non_empty || fields.iter().any(|(_, p, _)| p.is_required());

        ShapeFact { fields, tail, is_list, non_empty, covers }
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

    /// Does this shape already establish `c` (A-G8)?
    ///
    /// Either it carries a cover whose claim entails `c`, or one of `c`'s keys
    /// is `Required` here. **Deviation, deliberate:** a `Required` key
    /// discharges a `KeyExists` cover unconditionally, but an `Isset` cover
    /// only when the field's slot proves the value non-null — a required key
    /// whose value may be null does not make `isset` true, and keeping a cover
    /// the operand does not satisfy would make the join reject values the
    /// operand admits.
    #[must_use]
    pub fn implies_cover(&self, c: &Cover) -> bool {
        self.covers.iter().any(|own| own.subsumes(c))
            || c.keys.iter().any(|k| self.key_implies(k, c.flavor))
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
    /// `Singleton` value slot, the tail seals, and `is_list` is the real
    /// [`array_is_list`] verdict. This is where order-witnessed-ness is
    /// honestly lost.
    ///
    /// A nested array lifts to a `Singleton` slot rather than a nested shape:
    /// the singleton is strictly more precise.
    ///
    /// Beyond [`SHAPE_WIDTH_LIMIT`] fields the lift degrades to the tail-only
    /// summary (A-G6).
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

        ShapeFact::normalize(
            fields,
            join_tails(&self.tail, &other.tail),
            Certainty::all_of([self.is_list, other.is_list]),
            self.non_empty && other.non_empty,
            covers,
        )
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

fn field_of<'a>(fields: &'a [Field], k: &Key) -> Option<&'a Field> {
    fields.iter().find(|(fk, _, _)| fk == k)
}

/// A singleton cover is presence: promote the key to
/// `Required { witnessed: true }`. A proven-`Absent` key (or a key a sealed
/// tail forbids) contradicts the cover; dropping the cover widens, which is
/// the safe side.
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

/// A caller may know `is_list` more precisely than the key structure does
/// (a lifted literal knows its real order); it may never contradict it, and
/// on a contradiction the computed value wins.
fn sharpen_is_list(computed: Certainty, given: Certainty) -> Certainty {
    match computed {
        Certainty::Maybe => given,
        c => c,
    }
}

/// **Denotational `is_list`** (§3, RFC #14939).
///
/// `Yes` iff *every* admitted value passes [`array_is_list`], `No` iff *none*
/// does, `Maybe` otherwise. Because `array{…}` is an order-agnostic key set,
/// a shape whose realizable key sets can have two members admits both
/// insertion orders, so only one of them is a list — which is why
/// `array{0: T, 1: U}` is `Maybe` and not `Yes`.
fn compute_is_list(fields: &[Field], tail: &Tail, non_empty: bool) -> Certainty {
    let present: Vec<&Key> = fields
        .iter()
        .filter(|(_, p, _)| !matches!(p, Presence::Absent))
        .map(|(k, _, _)| k)
        .collect();
    // Every admitted value is a list iff no undeclared key can appear and at
    // most one key — key `0` — can, so no permutation is realizable.
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
        // Nothing is required: the empty array is a list, unless `non_empty`
        // forbids it — in which case key `0` has to come from somewhere.
        return !non_empty || tail_supplies_int || available(0);
    }
    if tail_supplies_int {
        return true;
    }
    // Filling `0..=max` needs at least that many declared keys; the cheap
    // bound keeps a sparse required key like `array{1000000: T}` from walking
    // a million candidates.
    let Ok(span) = usize::try_from(max) else { return false };
    if span.saturating_add(1) > fields.len() {
        return false;
    }
    (0..=max).all(available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::IntRange;
    use crate::value::Base;

    fn k(i: i64) -> Key {
        Key::Int(i)
    }

    fn ks(s: &str) -> Key {
        Key::Str(s.to_owned())
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

    // ------------------------------------------------------------------
    // The denotational is_list table (ADR-0062 §3, RFC #14939)
    // ------------------------------------------------------------------

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
        // The cheap bound: a million candidate keys are never enumerated.
        assert_eq!(sealed(vec![(k(1_000_000), req(), int_slot(2))]).is_list, Certainty::No);
    }

    #[test]
    fn a_caller_flag_sharpens_maybe_but_never_contradicts() {
        // `list<T>`: typed tail + isList Yes (A-G1).
        let list_of = ShapeFact::normalize(
            Vec::new(),
            Tail::Unsealed { key: KeyClass::Int, value: slot(Fact::General { base: Base::Int, nullable: false }) },
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(list_of.is_list, Certainty::Yes);
        // A contradicting flag loses to the computed verdict.
        let contradicted = ShapeFact::normalize(
            vec![(ks("a"), req(), int_slot(1))],
            Tail::Sealed,
            Certainty::Yes,
            false,
            Vec::new(),
        );
        assert_eq!(contradicted.is_list, Certainty::No);
    }

    // ------------------------------------------------------------------
    // normalize invariants
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // admits
    // ------------------------------------------------------------------

    #[test]
    fn admits_required_and_optional_fields() {
        let s = sealed(vec![
            (ks("a"), req(), int_slot(1)),
            (ks("b"), Presence::Optional, int_slot(2)),
        ]);
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1))])));
        assert!(s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(2))])));
        // missing required
        assert!(!s.admits(&arr(vec![(ks("b"), Val::Int(2))])));
        // optional present with the wrong value
        assert!(!s.admits(&arr(vec![(ks("a"), Val::Int(1)), (ks("b"), Val::Int(9))])));
    }

    #[test]
    fn admits_an_unknown_slot_accepts_any_value() {
        let s = sealed(vec![(ks("a"), req(), None)]);
        assert!(s.admits(&arr(vec![(ks("a"), Val::Null)])));
        assert!(s.admits(&arr(vec![(ks("a"), Val::Array(vec![]))])));
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
        // The tail-key gap ADR-0062 §1 measured, closed here:
        // `array{a: int, ...<string, int>}` must reject `['a' => 1, 9 => 2]`.
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
        // present-but-null satisfies KeyExists, never Isset.
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

    // ------------------------------------------------------------------
    // lift
    // ------------------------------------------------------------------

    #[test]
    fn lift_makes_every_entry_required_and_witnessed() {
        let entries = arr(vec![(k(0), Val::Int(1)), (k(1), Val::Str("x".to_owned()))]);
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

    // ------------------------------------------------------------------
    // join
    // ------------------------------------------------------------------

    #[test]
    fn join_sealed_disjoint_keys_absorbs_into_optionality() {
        // A-G5's worked example: Sealed{a} ⊔ Sealed{b} = {a?, b?} + Sealed.
        let a = ShapeFact::lift(&arr(vec![(ks("a"), Val::Int(1))]));
        let b = ShapeFact::lift(&arr(vec![(ks("b"), Val::Int(2))]));
        let j = a.join(&b);
        assert_eq!(j.tail, Tail::Sealed);
        assert_eq!(j.field(&ks("a")).map(|(_, p, _)| *p), Some(Presence::Optional));
        assert_eq!(j.field(&ks("b")).map(|(_, p, _)| *p), Some(Presence::Optional));
        // Neither key is required any more, but both operands were non-empty,
        // so the join still knows at least one of them is there.
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
        // Both sides proving absence keeps it proven.
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

        // A Required member implies the cover, so the join keeps it.
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
        let b = arr(vec![(k(0), Val::Str("x".to_owned()))]);
        let j = ShapeFact::lift(&a).join(&ShapeFact::lift(&b));
        assert!(j.admits(&a), "join lost the left operand");
        assert!(j.admits(&b), "join lost the right operand");
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
