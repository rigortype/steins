/-!
# String refinement predicates

Ports `crates/steins-domain/src/preds.rs` (ADR-0035). The Rust type is a closed
`u8` bitset; the spec carries the same predicates as named `Bool` fields,
because every law below is then a finite `decide` rather than bit arithmetic.

The predicate *evaluator* (`StrPreds::of`, i.e. PHP's `is_numeric`, string
falsiness, the case-function identities and the array-key cast) is deliberately
NOT modelled here — it is a parameter of `SteinsDomain.Model`. Nothing in this
file, and nothing in the soundness proofs that rest on it, depends on what the
classifier decides about a given string.

One consequence of that parametricity is worth stating up front, because the
Rust side now depends on it: `decimalInt` and `nonDecimalInt` are *complementary*
under the real classifier, and this file cannot see that. A `StrPreds` is a
**conjunction** of predicates, so the set carrying both is a perfectly good
value of the type (it denotes ∅), and every law below holds for it. What no law
can say is that it is empty — that would be a statement about the classifier.
This is exactly the negation ceiling the Rust `admits` leg reports.
-/

namespace SteinsDomain

/-- A set of string predicates. Canonically closed under implication when
`Closed` holds; the type itself admits unclosed values, exactly as the Rust
`StrPreds` does (`StrPreds::NUMERIC` alone is a value of the type). -/
structure StrPreds where
  /-- `non-empty-string`: the value is not `""`. -/
  nonEmpty : Bool := false
  /-- `non-falsy-string`: the value is neither `""` nor `"0"`. -/
  nonFalsy : Bool := false
  /-- `numeric-string`: `is_numeric()` holds. -/
  numeric : Bool := false
  /-- `lowercase-string`: `strtolower()` leaves the value unchanged. -/
  lowercase : Bool := false
  /-- `uppercase-string`: `strtoupper()` leaves the value unchanged. -/
  uppercase : Bool := false
  /-- `decimal-int-string`: the string spells an integer the way PHP writes one
  back, so an array key made of it is cast to `int`. -/
  decimalInt : Bool := false
  /-- `non-decimal-int-string`: the complement of `decimalInt` within `string`. -/
  nonDecimalInt : Bool := false
  deriving DecidableEq, Repr, Inhabited

namespace StrPreds

/-- No knowledge — the General layer's content. -/
def empty : StrPreds := {}

def NON_EMPTY : StrPreds := { nonEmpty := true }
def NON_FALSY : StrPreds := { nonFalsy := true }
def NUMERIC : StrPreds := { numeric := true }
def LOWERCASE : StrPreds := { lowercase := true }
def UPPERCASE : StrPreds := { uppercase := true }
def DECIMAL_INT : StrPreds := { decimalInt := true }
def NON_DECIMAL_INT : StrPreds := { nonDecimalInt := true }

/-- True when no predicate is known. -/
def isEmpty (p : StrPreds) : Bool :=
  !p.nonEmpty && !p.nonFalsy && !p.numeric && !p.lowercase && !p.uppercase
    && !p.decimalInt && !p.nonDecimalInt

/-- The implication closure: `DecimalInt ⇒ Numeric ∧ Lowercase ∧ Uppercase`,
`NonFalsy ⇒ NonEmpty`, `Numeric ⇒ NonEmpty`.

`decimalInt`'s clauses come first so the single pass reaches the fixpoint: its
`numeric` consequent then feeds `Numeric ⇒ NonEmpty` in the same step, which is
the ordering the Rust `close` relies on too. The casing predicates entail
nothing themselves — `""` is lowercase, and `"1e5"`/`"1E5"` are numeric with
opposite casings — but `decimalInt` entails *both*, its alphabet (digits and
`-`) having no cased character. `nonDecimalInt` is a leaf in both directions. -/
def close (p : StrPreds) : StrPreds :=
  let q : StrPreds :=
    { p with numeric := p.numeric || p.decimalInt,
             lowercase := p.lowercase || p.decimalInt,
             uppercase := p.uppercase || p.decimalInt }
  { q with nonEmpty := q.nonEmpty || q.nonFalsy || q.numeric }

/-- Set union, then closure (as in Rust `union`). -/
def union (p q : StrPreds) : StrPreds :=
  close ⟨p.nonEmpty || q.nonEmpty, p.nonFalsy || q.nonFalsy, p.numeric || q.numeric,
    p.lowercase || q.lowercase, p.uppercase || q.uppercase,
    p.decimalInt || q.decimalInt, p.nonDecimalInt || q.nonDecimalInt⟩

/-- Set intersection (no closure step — see `inter_closed`). -/
def inter (p q : StrPreds) : StrPreds :=
  ⟨p.nonEmpty && q.nonEmpty, p.nonFalsy && q.nonFalsy, p.numeric && q.numeric,
    p.lowercase && q.lowercase, p.uppercase && q.uppercase,
    p.decimalInt && q.decimalInt, p.nonDecimalInt && q.nonDecimalInt⟩

/-- Whether every predicate in `q` is present in `p` (Rust `contains_all`). -/
def containsAll (p q : StrPreds) : Bool :=
  (!q.nonEmpty || p.nonEmpty) && (!q.nonFalsy || p.nonFalsy) && (!q.numeric || p.numeric)
    && (!q.lowercase || p.lowercase) && (!q.uppercase || p.uppercase)
    && (!q.decimalInt || p.decimalInt) && (!q.nonDecimalInt || p.nonDecimalInt)

/-- A predicate set is closed when the implications have been applied. -/
def Closed (p : StrPreds) : Prop := p.close = p

instance (p : StrPreds) : Decidable p.Closed := inferInstanceAs (Decidable (_ = _))

/-! ## Closure

Every law below is decided by exhausting the `Bool` fields: seven predicates
means 128 sets, so a one-argument law is 128 cases and a two-argument one
16,384, and `decide` settles each. This is the whole reason the spec keeps the
predicates as named fields instead of porting the `u8` bitset.

The two-argument exhaustion grew 16× when the array-key-cast pair landed, so the
laws that use it carry an explicit `maxHeartbeats` — the kernel reduction is
still small, the default budget simply is not sized for it. The three-argument
laws were already past that boundary before this pair existed and are proved
from `containsAll_iff` instead; that route does not grow with the predicate
count, which is why it is the one every new predicate rides for free. -/

set_option maxHeartbeats 1000000 in
theorem close_idem (p : StrPreds) : p.close.close = p.close := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

theorem closed_close (p : StrPreds) : p.close.Closed := close_idem p

set_option maxHeartbeats 1000000 in
theorem close_containsAll (p : StrPreds) : p.close.containsAll p = true := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

set_option maxHeartbeats 4000000 in
/-- The claim the Rust `intersect` doc comment makes: closure is preserved by
intersection of closed sets, because the implications are Horn clauses over
positive literals. The `decimalInt` clauses are Horn too — one positive
antecedent, positive consequents — so nothing about this argument changed when
they were added. -/
theorem inter_closed {p q : StrPreds} (hp : p.Closed) (hq : q.Closed) :
    (p.inter q).Closed := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

set_option maxHeartbeats 4000000 in
theorem union_closed (p q : StrPreds) : (p.union q).Closed := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

/-! ## `containsAll` is a partial order, `inter` its meet

The three-argument laws are 2^21 cases, far past what `decide` reaches, so they
are proved from a *characterization* instead: the bitwise subset test is
field-wise implication (`containsAll_iff`, itself a two-argument `decide`), and
the order laws are then the order laws of `→` and `&&` on each field. Same
theorems, and the proofs no longer grow with the predicate count. -/

set_option maxHeartbeats 4000000 in
/-- The bitset subset test, read as field-wise implication. Two arguments, so
`decide` still settles it, and every three-argument law below reduces to it. -/
theorem containsAll_iff {p q : StrPreds} :
    p.containsAll q = true ↔
      ((q.nonEmpty = true → p.nonEmpty = true) ∧ (q.nonFalsy = true → p.nonFalsy = true) ∧
       (q.numeric = true → p.numeric = true) ∧ (q.lowercase = true → p.lowercase = true) ∧
       (q.uppercase = true → p.uppercase = true) ∧
       (q.decimalInt = true → p.decimalInt = true) ∧
       (q.nonDecimalInt = true → p.nonDecimalInt = true)) := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

set_option maxHeartbeats 1000000 in
theorem containsAll_refl (p : StrPreds) : p.containsAll p = true := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

theorem containsAll_trans {p q r : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll r = true) :
    p.containsAll r = true := by
  rw [containsAll_iff] at h₁ h₂ ⊢
  exact ⟨h₁.1 ∘ h₂.1, h₁.2.1 ∘ h₂.2.1, h₁.2.2.1 ∘ h₂.2.2.1, h₁.2.2.2.1 ∘ h₂.2.2.2.1,
    h₁.2.2.2.2.1 ∘ h₂.2.2.2.2.1, h₁.2.2.2.2.2.1 ∘ h₂.2.2.2.2.2.1,
    h₁.2.2.2.2.2.2 ∘ h₂.2.2.2.2.2.2⟩

set_option maxHeartbeats 4000000 in
theorem containsAll_antisymm {p q : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll p = true) : p = q := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

set_option maxHeartbeats 4000000 in
theorem inter_containsAll_left (p q : StrPreds) : p.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

set_option maxHeartbeats 4000000 in
theorem inter_containsAll_right (p q : StrPreds) : q.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

set_option maxHeartbeats 4000000 in
theorem inter_comm (p q : StrPreds) : p.inter q = q.inter p := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; obtain ⟨e, f, g, lb, ub, d', nd'⟩ := q
  revert a b c la ua d nd e f g lb ub d' nd'; decide

theorem inter_assoc (p q r : StrPreds) : (p.inter q).inter r = p.inter (q.inter r) := by
  simp [inter, Bool.and_assoc]

set_option maxHeartbeats 1000000 in
theorem inter_self (p : StrPreds) : p.inter p = p := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

set_option maxHeartbeats 1000000 in
/-- The greatest-lower-bound property: anything below both is below the meet. -/
theorem containsAll_inter {p q r : StrPreds}
    (h₁ : p.containsAll r = true) (h₂ : q.containsAll r = true) :
    (p.inter q).containsAll r = true := by
  rw [containsAll_iff] at h₁ h₂ ⊢
  exact ⟨fun h => by simp [inter, h₁.1 h, h₂.1 h],
    fun h => by simp [inter, h₁.2.1 h, h₂.2.1 h],
    fun h => by simp [inter, h₁.2.2.1 h, h₂.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.1 h, h₂.2.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.2.1 h, h₂.2.2.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.2.2.1 h, h₂.2.2.2.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.2.2.2 h, h₂.2.2.2.2.2.2 h]⟩

/-- Widening a value set intersects the members' summaries, so the result is
below every member — the direction `summarize` needs to stay sound.

Proved from the meet laws rather than by `decide`: with seven predicates the
four-argument exhaustion is 2^28 cases. -/
theorem inter_mono {p q p' q' : StrPreds}
    (hp : p.containsAll p' = true) (hq : q.containsAll q' = true) :
    (p.inter q).containsAll (p'.inter q') = true :=
  containsAll_inter (containsAll_trans hp (inter_containsAll_left p' q'))
    (containsAll_trans hq (inter_containsAll_right p' q'))

set_option maxHeartbeats 1000000 in
/-- An empty predicate set constrains nothing. -/
theorem containsAll_empty (p : StrPreds) : p.containsAll empty = true := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

set_option maxHeartbeats 1000000 in
theorem isEmpty_iff (p : StrPreds) : p.isEmpty = true ↔ p = empty := by
  obtain ⟨a, b, c, la, ua, d, nd⟩ := p; revert a b c la ua d nd; decide

end StrPreds
end SteinsDomain
