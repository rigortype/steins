/-!
# String refinement predicates

Ports `crates/steins-domain/src/preds.rs` (ADR-0035). The Rust type is a closed
`u8` bitset; the spec carries the same three predicates as named `Bool` fields,
because every law below is then a 64-case `decide` rather than bit arithmetic.

The predicate *evaluator* (`StrPreds::of`, i.e. PHP's `is_numeric` and string
falsiness) is deliberately NOT modelled here — it is a parameter of
`SteinsDomain.Model`. Nothing in this file, and nothing in the soundness proofs
that rest on it, depends on what the classifier decides about a given string.
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
  deriving DecidableEq, Repr, Inhabited

namespace StrPreds

/-- No knowledge — the General layer's content. -/
def empty : StrPreds := {}

def NON_EMPTY : StrPreds := { nonEmpty := true }
def NON_FALSY : StrPreds := { nonFalsy := true }
def NUMERIC : StrPreds := { numeric := true }
def LOWERCASE : StrPreds := { lowercase := true }
def UPPERCASE : StrPreds := { uppercase := true }

/-- True when no predicate is known. -/
def isEmpty (p : StrPreds) : Bool :=
  !p.nonEmpty && !p.nonFalsy && !p.numeric && !p.lowercase && !p.uppercase

/-- The implication closure: `NonFalsy ⇒ NonEmpty`, `Numeric ⇒ NonEmpty`. The
casing predicates add no clause in either direction — `""` is lowercase, and
`"1e5"`/`"1E5"` are numeric with opposite casings, so neither entails nor is
entailed by anything here. -/
def close (p : StrPreds) : StrPreds :=
  { p with nonEmpty := p.nonEmpty || p.nonFalsy || p.numeric }

/-- Set union, then closure (as in Rust `union`). -/
def union (p q : StrPreds) : StrPreds :=
  close ⟨p.nonEmpty || q.nonEmpty, p.nonFalsy || q.nonFalsy, p.numeric || q.numeric,
    p.lowercase || q.lowercase, p.uppercase || q.uppercase⟩

/-- Set intersection (no closure step — see `inter_closed`). -/
def inter (p q : StrPreds) : StrPreds :=
  ⟨p.nonEmpty && q.nonEmpty, p.nonFalsy && q.nonFalsy, p.numeric && q.numeric,
    p.lowercase && q.lowercase, p.uppercase && q.uppercase⟩

/-- Whether every predicate in `q` is present in `p` (Rust `contains_all`). -/
def containsAll (p q : StrPreds) : Bool :=
  (!q.nonEmpty || p.nonEmpty) && (!q.nonFalsy || p.nonFalsy) && (!q.numeric || p.numeric)
    && (!q.lowercase || p.lowercase) && (!q.uppercase || p.uppercase)

/-- A predicate set is closed when the implications have been applied. -/
def Closed (p : StrPreds) : Prop := p.close = p

instance (p : StrPreds) : Decidable p.Closed := inferInstanceAs (Decidable (_ = _))

/-! ## Closure

Every law below is decided by exhausting the `Bool` fields: five predicates
means 32 sets, so a two-argument law is 1,024 cases and a three-argument one
32,768, and `decide` settles each. This is the whole reason the spec keeps the
predicates as named fields instead of porting the `u8` bitset.

The one law this does *not* scale to is `inter_mono`, whose four arguments are
2^20 cases — a kernel reduction large enough to be a build hazard rather than a
proof. It is discharged structurally instead, from the meet laws directly, which
is the better proof anyway: it says the monotonicity is a consequence of `inter`
being the greatest lower bound, not of there being finitely many predicates. -/

theorem close_idem (p : StrPreds) : p.close.close = p.close := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

theorem closed_close (p : StrPreds) : p.close.Closed := close_idem p

theorem close_containsAll (p : StrPreds) : p.close.containsAll p = true := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

/-- The claim the Rust `intersect` doc comment makes: closure is preserved by
intersection of closed sets, because the implications are Horn clauses over
positive literals. -/
theorem inter_closed {p q : StrPreds} (hp : p.Closed) (hq : q.Closed) :
    (p.inter q).Closed := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem union_closed (p q : StrPreds) : (p.union q).Closed := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

/-! ## `containsAll` is a partial order, `inter` its meet

The three-argument laws are 2^15 cases, which `decide` reaches but only past the
heartbeat limit, so they are proved from a *characterization* instead: the
bitwise subset test is field-wise implication (`containsAll_iff`, itself a
two-argument `decide`), and the order laws are then the order laws of `→` and
`&&` on each field. Same theorems, and the proofs no longer grow with the
predicate count. -/

/-- The bitset subset test, read as field-wise implication. Two arguments, so
`decide` still settles it, and every three-argument law below reduces to it. -/
theorem containsAll_iff {p q : StrPreds} :
    p.containsAll q = true ↔
      ((q.nonEmpty = true → p.nonEmpty = true) ∧ (q.nonFalsy = true → p.nonFalsy = true) ∧
       (q.numeric = true → p.numeric = true) ∧ (q.lowercase = true → p.lowercase = true) ∧
       (q.uppercase = true → p.uppercase = true)) := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem containsAll_refl (p : StrPreds) : p.containsAll p = true := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

theorem containsAll_trans {p q r : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll r = true) :
    p.containsAll r = true := by
  rw [containsAll_iff] at h₁ h₂ ⊢
  exact ⟨h₁.1 ∘ h₂.1, h₁.2.1 ∘ h₂.2.1, h₁.2.2.1 ∘ h₂.2.2.1, h₁.2.2.2.1 ∘ h₂.2.2.2.1,
    h₁.2.2.2.2 ∘ h₂.2.2.2.2⟩

theorem containsAll_antisymm {p q : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll p = true) : p = q := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem inter_containsAll_left (p q : StrPreds) : p.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem inter_containsAll_right (p q : StrPreds) : q.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem inter_comm (p q : StrPreds) : p.inter q = q.inter p := by
  obtain ⟨a, b, c, la, ua⟩ := p; obtain ⟨d, e, f, lb, ub⟩ := q
  revert a b c la ua d e f lb ub; decide

theorem inter_assoc (p q r : StrPreds) : (p.inter q).inter r = p.inter (q.inter r) := by
  simp [inter, Bool.and_assoc]

theorem inter_self (p : StrPreds) : p.inter p = p := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

/-- The greatest-lower-bound property: anything below both is below the meet. -/
theorem containsAll_inter {p q r : StrPreds}
    (h₁ : p.containsAll r = true) (h₂ : q.containsAll r = true) :
    (p.inter q).containsAll r = true := by
  rw [containsAll_iff] at h₁ h₂ ⊢
  exact ⟨fun h => by simp [inter, h₁.1 h, h₂.1 h],
    fun h => by simp [inter, h₁.2.1 h, h₂.2.1 h],
    fun h => by simp [inter, h₁.2.2.1 h, h₂.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.1 h, h₂.2.2.2.1 h],
    fun h => by simp [inter, h₁.2.2.2.2 h, h₂.2.2.2.2 h]⟩

/-- Widening a value set intersects the members' summaries, so the result is
below every member — the direction `summarize` needs to stay sound.

Proved from the meet laws rather than by `decide`: with five predicates the
four-argument exhaustion is 2^20 cases. -/
theorem inter_mono {p q p' q' : StrPreds}
    (hp : p.containsAll p' = true) (hq : q.containsAll q' = true) :
    (p.inter q).containsAll (p'.inter q') = true :=
  containsAll_inter (containsAll_trans hp (inter_containsAll_left p' q'))
    (containsAll_trans hq (inter_containsAll_right p' q'))

/-- An empty predicate set constrains nothing. -/
theorem containsAll_empty (p : StrPreds) : p.containsAll empty = true := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

theorem isEmpty_iff (p : StrPreds) : p.isEmpty = true ↔ p = empty := by
  obtain ⟨a, b, c, la, ua⟩ := p; revert a b c la ua; decide

end StrPreds
end SteinsDomain
