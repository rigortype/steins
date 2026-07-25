/-!
# The trinary judgment

Ports `crates/steins-domain/src/certainty.rs` (ADR-0031: PHPStan's TrinaryLogic
and Rigor's Certainty are the same lattice; Steins has exactly one).

What is proved here that the Rust side only spot-checks with a table test:
Kleene strong logic really is commutative, associative, and De Morgan-dual, and
`allOf` really is the "every member" quantifier — including the deliberate
`maybe` on the empty list, which is where a vacuous `yes` would leak a
false positive.
-/

namespace SteinsDomain

/-- `Yes` / `No` / `Maybe`. -/
inductive Certainty where
  | yes
  | no
  | maybe
  deriving DecidableEq, Repr, Inhabited

namespace Certainty

/-- Three-valued conjunction (Kleene strong logic). Clause order mirrors the
Rust `match`: a single `No` decides, two `Yes` decide, everything else is the
honest middle. -/
def and : Certainty → Certainty → Certainty
  | .no, _ => .no
  | _, .no => .no
  | .yes, .yes => .yes
  | _, _ => .maybe

/-- Three-valued disjunction (Kleene strong logic). -/
def or : Certainty → Certainty → Certainty
  | .yes, _ => .yes
  | _, .yes => .yes
  | .no, .no => .no
  | _, _ => .maybe

/-- Three-valued negation: `maybe` is its own negation. -/
def not : Certainty → Certainty
  | .yes => .no
  | .no => .yes
  | .maybe => .maybe

/-- Lift a decided boolean. -/
def ofBool (b : Bool) : Certainty := if b then .yes else .no

/-- Fold judgments about *every* member of a collection: all `yes` → `yes`,
all `no` → `no`, anything mixed or `maybe` → `maybe`.

The empty list is `maybe`, not `yes`: an empty set decides nothing, and the
vacuous-truth reading is exactly the shape that manufactures a proof-layer
finding out of no evidence. -/
def allOf : List Certainty → Certainty
  | [] => .maybe
  | c :: cs => if c = .maybe then .maybe else if cs.all (· == c) then c else .maybe

/-! ## Kleene laws -/

theorem and_comm (a b : Certainty) : a.and b = b.and a := by
  cases a <;> cases b <;> rfl

theorem or_comm (a b : Certainty) : a.or b = b.or a := by
  cases a <;> cases b <;> rfl

theorem and_assoc (a b c : Certainty) : (a.and b).and c = a.and (b.and c) := by
  cases a <;> cases b <;> cases c <;> rfl

theorem or_assoc (a b c : Certainty) : (a.or b).or c = a.or (b.or c) := by
  cases a <;> cases b <;> cases c <;> rfl

theorem not_not (a : Certainty) : a.not.not = a := by
  cases a <;> rfl

theorem de_morgan_and (a b : Certainty) : (a.and b).not = a.not.or b.not := by
  cases a <;> cases b <;> rfl

theorem de_morgan_or (a b : Certainty) : (a.or b).not = a.not.and b.not := by
  cases a <;> cases b <;> rfl

/-- `maybe` never promotes: no combination of evidence turns undecided inputs
into a decided answer. This is the Rigor discipline the doc comment claims. -/
theorem maybe_absorbs (a : Certainty) :
    (a ≠ .no → a.and .maybe = .maybe) ∧ (a ≠ .yes → a.or .maybe = .maybe) := by
  cases a <;> simp [and, or]

/-! ## `allOf` is the universal quantifier -/

/-- The empty fold decides nothing. -/
theorem allOf_nil : allOf [] = .maybe := rfl

@[simp] theorem allOf_cons (c : Certainty) (cs : List Certainty) :
    allOf (c :: cs) =
      if c = .maybe then .maybe else if cs.all (· == c) then c else .maybe := rfl

/-- The decided verdicts of `allOf` are exactly the uniform non-empty
collections — for either decided target. -/
theorem allOf_eq_iff (target : Certainty) (htarget : target ≠ .maybe) (cs : List Certainty) :
    allOf cs = target ↔ cs ≠ [] ∧ ∀ c ∈ cs, c = target := by
  cases cs with
  | nil => simp [allOf, Ne.symm htarget]
  | cons c cs =>
    simp only [allOf_cons, ne_eq, reduceCtorEq, not_false_eq_true, true_and,
      List.mem_cons, forall_eq_or_imp]
    by_cases hc : c = .maybe
    · subst hc
      constructor
      · intro h; exact absurd h.symm htarget
      · intro ⟨h, _⟩; exact absurd h.symm htarget
    · rw [if_neg hc]
      by_cases hall : (cs.all (· == c)) = true
      · rw [if_pos hall]
        constructor
        · intro h
          subst h
          exact ⟨rfl, fun d hd => by simpa using List.all_eq_true.mp hall d hd⟩
        · intro ⟨h, _⟩; exact h
      · rw [if_neg hall]
        constructor
        · intro h; exact absurd h.symm htarget
        · intro ⟨hchead, htail⟩
          exfalso
          refine hall (List.all_eq_true.mpr fun d hd => ?_)
          simp [hchead, htail d hd]

theorem allOf_eq_yes_iff (cs : List Certainty) :
    allOf cs = .yes ↔ cs ≠ [] ∧ ∀ c ∈ cs, c = .yes :=
  allOf_eq_iff .yes (by simp) cs

theorem allOf_eq_no_iff (cs : List Certainty) :
    allOf cs = .no ↔ cs ≠ [] ∧ ∀ c ∈ cs, c = .no :=
  allOf_eq_iff .no (by simp) cs

/-- The consequence the proof layer actually consumes: a decided `allOf` is a
statement about every member, so a `yes` can never be manufactured from an
empty or mixed collection. -/
theorem allOf_sound (cs : List Certainty) :
    (allOf cs = .yes → ∀ c ∈ cs, c = .yes) ∧ (allOf cs = .no → ∀ c ∈ cs, c = .no) :=
  ⟨fun h => ((allOf_eq_yes_iff cs).mp h).2, fun h => ((allOf_eq_no_iff cs).mp h).2⟩

end Certainty
end SteinsDomain
