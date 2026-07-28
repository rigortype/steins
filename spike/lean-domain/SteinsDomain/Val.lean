import SteinsDomain.Preds

/-!
# Values, their total order, and the model parameters

Ports `crates/steins-domain/src/value.rs` and the parts of `php.rs` the domain
algebra consumes.

**The abstraction.** Ints and bools are carried concretely. Strings, floats, and
arrays are carried as *ordered atoms* — a `Nat` giving the value's position in
the domain's total order — because those are the only two things the algebra
ever does with them:

* compare them (`Ord`/`Eq`, for the sorted-deduped `OneOf` layer), and
* for strings, ask the classifier for a predicate summary.

Abstracting them is what makes the soundness proofs independent of PHP's
`is_numeric` grammar and of IEEE-754 (`total_cmp`, `NaN`, `-0.0`). That
independence is the point, not a shortcut: the classifier's *fidelity* is a
separate obligation, discharged differentially against the real engine
(`tests/php_oracle.rs`, and the ADR-0059 vector file's `classifier` block), not
by this spec.

The atom ranks reproduce Rust's `discriminant_rank`: Null < Bool < Int < Float <
Str < Array.
-/

namespace SteinsDomain

/-- A scalar base type (Rust `Base`). `Null` is deliberately absent: nullability
is a flag on the abstract layers. -/
inductive Base where
  | int
  | float
  | str
  | bool
  deriving DecidableEq, Repr, Inhabited

/-- A concrete PHP value. See the module docstring for what `float`/`str`/`arr`
abstract. -/
inductive Val where
  | null
  | bool (b : Bool)
  | int (n : Int)
  | float (rank : Nat)
  | str (rank : Nat)
  | arr (rank : Nat)
  deriving DecidableEq, Repr, Inhabited

namespace Val

/-- The scalar base of a value, if it is a scalar. `null` and arrays have none. -/
def base : Val → Option Base
  | .int _ => some .int
  | .float _ => some .float
  | .str _ => some .str
  | .bool _ => some .bool
  | .null => none
  | .arr _ => none

/-- Rust's `discriminant_rank`. -/
def rank : Val → Nat
  | .null => 0
  | .bool _ => 1
  | .int _ => 2
  | .float _ => 3
  | .str _ => 4
  | .arr _ => 5

/-- The within-rank ordering key. For bools this is Rust's `false < true`; for
ints the value; for the atoms their position. -/
def tie : Val → Int
  | .null => 0
  | .bool b => if b then 1 else 0
  | .int n => n
  | .float r => (r : Int)
  | .str r => (r : Int)
  | .arr r => (r : Int)

/-- Strict order: lexicographic on `(rank, tie)`. This is Rust's `Ord for Val`
(discriminant rank first, payload comparison within a variant). -/
def blt (a b : Val) : Bool :=
  a.rank < b.rank || (a.rank == b.rank && a.tie < b.tie)

/-- `(rank, tie)` identifies a value — the reason the lexicographic order is a
*total* order and not merely a preorder. -/
theorem key_inj {a b : Val} (hr : a.rank = b.rank) (ht : a.tie = b.tie) : a = b := by
  cases a <;> cases b <;> simp_all [rank, tie]
  case bool.bool x y => cases x <;> cases y <;> simp_all
  all_goals omega

theorem blt_irrefl (a : Val) : a.blt a = false := by
  simp only [blt, Bool.or_eq_false_iff, Bool.and_eq_false_imp, decide_eq_false_iff_not,
    Nat.not_lt, beq_iff_eq]
  omega

theorem blt_asymm {a b : Val} (h : a.blt b = true) : b.blt a = false := by
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at h
  simp only [blt, Bool.or_eq_false_iff, Bool.and_eq_false_imp, decide_eq_false_iff_not,
    Nat.not_lt, beq_iff_eq]
  omega

theorem blt_trans {a b c : Val} (h₁ : a.blt b = true) (h₂ : b.blt c = true) :
    a.blt c = true := by
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at *
  omega

/-- Totality (trichotomy): distinct values are ordered one way or the other. -/
theorem blt_total {a b : Val} (h : a ≠ b) : a.blt b = true ∨ b.blt a = true := by
  have hk : ¬(a.rank = b.rank ∧ a.tie = b.tie) := fun ⟨h₁, h₂⟩ => h (key_inj h₁ h₂)
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq]
  omega

theorem eq_of_not_blt {a b : Val} (h₁ : a.blt b = false) (h₂ : b.blt a = false) : a = b := by
  by_cases hne : a = b
  · exact hne
  · rcases blt_total hne with h | h
    · rw [h₁] at h; exact absurd h (by simp)
    · rw [h₂] at h; exact absurd h (by simp)

end Val

/-! ## Keys -/

/-- An array key after PHP normalization. Strings are ordered atoms, exactly as
in `Val` — see the module docstring of `SteinsDomain.Val`. -/
inductive Key where
  /-- Integer key. -/
  | int (n : Int)
  /-- String key, as its position in the domain's order on key strings. -/
  | str (rank : Nat)
  deriving DecidableEq, Repr, Inhabited

namespace Key

/-- Rust's derived discriminant order: `Int` before `Str`. -/
def rank : Key → Nat
  | .int _ => 0
  | .str _ => 1

def tie : Key → Int
  | .int n => n
  | .str r => (r : Int)

/-- Strict order, lexicographic on `(rank, tie)` — Rust's derived `Ord for Key`
(ints before strings, each ascending). -/
def blt (a b : Key) : Bool :=
  a.rank < b.rank || (a.rank == b.rank && a.tie < b.tie)

theorem blt_irrefl (a : Key) : a.blt a = false := by
  simp only [blt, Bool.or_eq_false_iff, Bool.and_eq_false_imp, decide_eq_false_iff_not,
    Nat.not_lt, beq_iff_eq]
  omega

theorem key_inj {a b : Key} (hr : a.rank = b.rank) (ht : a.tie = b.tie) : a = b := by
  cases a <;> cases b <;> simp_all [rank, tie]
  all_goals omega

theorem blt_asymm {a b : Key} (h : a.blt b = true) : b.blt a = false := by
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at h
  simp only [blt, Bool.or_eq_false_iff, Bool.and_eq_false_imp, decide_eq_false_iff_not,
    Nat.not_lt, beq_iff_eq]
  omega

theorem blt_trans {a b c : Key} (h₁ : a.blt b = true) (h₂ : b.blt c = true) : a.blt c = true := by
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at *
  omega

theorem blt_total {a b : Key} (h : a ≠ b) : a.blt b = true ∨ b.blt a = true := by
  have hk : ¬(a.rank = b.rank ∧ a.tie = b.tie) := fun ⟨h₁, h₂⟩ => h (key_inj h₁ h₂)
  simp only [blt, Bool.or_eq_true, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq]
  omega

end Key

/-- The parameters the value domain treats as opaque, together with the two
coherence laws the algebra actually leans on.

Keeping these as parameters is what makes the theorems in
`SteinsDomain.Soundness` hold for *any* string classifier: nothing in the join
algebra inspects PHP's `is_numeric` grammar. -/
structure Model where
  /-- `StrPreds::of` — the predicate summary of a string atom. -/
  predsOf : Nat → StrPreds
  /-- `php_str_is_falsy` — the string atom is `""` or `"0"`. -/
  strFalsy : Nat → Bool
  /-- `php_is_falsy` on a float atom (`0.0`, `-0.0`). -/
  floatFalsy : Nat → Bool
  /-- `php_is_falsy` on an array atom (the empty array). -/
  arrFalsy : Nat → Bool
  /-- The entries of an array atom, in insertion order — `Val::Array`'s payload.
  The scalar algebra never reads this; the array stratum (ADR-0062,
  `SteinsDomain.Shape`) does, and it is the one place the abstraction has to be
  opened. Like `predsOf`, it is a parameter, so the shape theorems hold for any
  array table, and its *fidelity* is checked differentially by the vector file's
  `shapearr` block rather than assumed here. -/
  arrEntries : Nat → List (Key × Val)
  /-- `StrPreds::of` returns implication-closed sets. Recorded because the Rust
  doc comment claims it; **not** used by any soundness proof (see
  `SteinsDomain.Soundness`). -/
  predsOf_closed : ∀ r, (predsOf r).Closed
  /-- `of` sets `NON_FALSY` exactly when the string is truthy. This coupling is
  what `abstract_falsy_truthy` reads a predicate set for, so a proof about
  `truthy` is only as good as this law — which the vector file's `classifier`
  block checks on concrete strings. -/
  nonFalsy_iff : ∀ r, (predsOf r).nonFalsy = !strFalsy r
  /-- An array is falsy exactly when it is empty. This couples the two array
  parameters, so a `truthy` verdict read off `arrFalsy` and a shape verdict read
  off the entries cannot disagree. -/
  arrFalsy_iff : ∀ r, arrFalsy r = (arrEntries r).isEmpty

namespace Val

/-- PHP falsiness, ported from `php_is_falsy`. -/
def falsy (M : Model) : Val → Bool
  | .bool b => !b
  | .int n => n == 0
  | .float r => M.floatFalsy r
  | .str r => M.strFalsy r
  | .null => true
  | .arr r => M.arrFalsy r

end Val
end SteinsDomain
