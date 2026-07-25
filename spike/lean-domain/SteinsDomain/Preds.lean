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
  deriving DecidableEq, Repr, Inhabited

namespace StrPreds

/-- No knowledge — the General layer's content. -/
def empty : StrPreds := {}

def NON_EMPTY : StrPreds := { nonEmpty := true }
def NON_FALSY : StrPreds := { nonFalsy := true }
def NUMERIC : StrPreds := { numeric := true }

/-- True when no predicate is known. -/
def isEmpty (p : StrPreds) : Bool := !p.nonEmpty && !p.nonFalsy && !p.numeric

/-- The implication closure: `NonFalsy ⇒ NonEmpty`, `Numeric ⇒ NonEmpty`. -/
def close (p : StrPreds) : StrPreds :=
  { p with nonEmpty := p.nonEmpty || p.nonFalsy || p.numeric }

/-- Set union, then closure (as in Rust `union`). -/
def union (p q : StrPreds) : StrPreds :=
  close ⟨p.nonEmpty || q.nonEmpty, p.nonFalsy || q.nonFalsy, p.numeric || q.numeric⟩

/-- Set intersection (no closure step — see `inter_closed`). -/
def inter (p q : StrPreds) : StrPreds :=
  ⟨p.nonEmpty && q.nonEmpty, p.nonFalsy && q.nonFalsy, p.numeric && q.numeric⟩

/-- Whether every predicate in `q` is present in `p` (Rust `contains_all`). -/
def containsAll (p q : StrPreds) : Bool :=
  (!q.nonEmpty || p.nonEmpty) && (!q.nonFalsy || p.nonFalsy) && (!q.numeric || p.numeric)

/-- A predicate set is closed when the implications have been applied. -/
def Closed (p : StrPreds) : Prop := p.close = p

instance (p : StrPreds) : Decidable p.Closed := inferInstanceAs (Decidable (_ = _))

/-! ## Closure

Every law below is decided by exhausting the `Bool` fields: three predicates
means eight sets, so a two-argument law is 64 closed cases and `decide` settles
it. This is the whole reason the spec keeps the predicates as named fields
instead of porting the `u8` bitset. -/

theorem close_idem (p : StrPreds) : p.close.close = p.close := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

theorem closed_close (p : StrPreds) : p.close.Closed := close_idem p

theorem close_containsAll (p : StrPreds) : p.close.containsAll p = true := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

/-- The claim the Rust `intersect` doc comment makes: closure is preserved by
intersection of closed sets, because the implications are Horn clauses over
positive literals. -/
theorem inter_closed {p q : StrPreds} (hp : p.Closed) (hq : q.Closed) :
    (p.inter q).Closed := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

theorem union_closed (p q : StrPreds) : (p.union q).Closed := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

/-! ## `containsAll` is a partial order, `inter` its meet -/

theorem containsAll_refl (p : StrPreds) : p.containsAll p = true := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

theorem containsAll_trans {p q r : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll r = true) :
    p.containsAll r = true := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q; obtain ⟨g, i, j⟩ := r
  revert a b c d e f g i j; decide

theorem containsAll_antisymm {p q : StrPreds}
    (h₁ : p.containsAll q = true) (h₂ : q.containsAll p = true) : p = q := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

theorem inter_containsAll_left (p q : StrPreds) : p.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

theorem inter_containsAll_right (p q : StrPreds) : q.containsAll (p.inter q) = true := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

theorem inter_comm (p q : StrPreds) : p.inter q = q.inter p := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  revert a b c d e f; decide

theorem inter_assoc (p q r : StrPreds) : (p.inter q).inter r = p.inter (q.inter r) := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q; obtain ⟨g, i, j⟩ := r
  revert a b c d e f g i j; decide

theorem inter_self (p : StrPreds) : p.inter p = p := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

/-- The greatest-lower-bound property: anything below both is below the meet. -/
theorem containsAll_inter {p q r : StrPreds}
    (h₁ : p.containsAll r = true) (h₂ : q.containsAll r = true) :
    (p.inter q).containsAll r = true := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q; obtain ⟨g, i, j⟩ := r
  revert a b c d e f g i j; decide

/-- Widening a value set intersects the members' summaries, so the result is
below every member — the direction `summarize` needs to stay sound. -/
theorem inter_mono {p q p' q' : StrPreds}
    (hp : p.containsAll p' = true) (hq : q.containsAll q' = true) :
    (p.inter q).containsAll (p'.inter q') = true := by
  obtain ⟨a, b, c⟩ := p; obtain ⟨d, e, f⟩ := q
  obtain ⟨g, i, j⟩ := p'; obtain ⟨k, l, m⟩ := q'
  revert a b c d e f g i j k l m; decide

/-- An empty predicate set constrains nothing. -/
theorem containsAll_empty (p : StrPreds) : p.containsAll empty = true := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

theorem isEmpty_iff (p : StrPreds) : p.isEmpty = true ↔ p = empty := by
  obtain ⟨a, b, c⟩ := p; revert a b c; decide

end StrPreds
end SteinsDomain
