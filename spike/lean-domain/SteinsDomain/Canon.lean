import SteinsDomain.Val

/-!
# The canonical finite layer

Rust builds the `OneOf` layer with `vals.sort(); vals.dedup()` and then reads it
back with `binary_search`. The spec models that as a sorted-insert fold, and
proves the property the Rust code depends on but never states: the result is the
*unique* strictly-increasing list with the same members, so a value set has one
representation and `join` on the finite layers cannot depend on argument order.

**Modelling note.** `Fact.admits` on the `oneOf` layer is linear membership here,
where Rust uses `binary_search`. The two agree on strictly-increasing lists —
which `ssorted_canon` establishes for every list the algebra constructs. The
Rust side's actual `binary_search` is exercised by the differential vectors, not
re-derived here.
-/

namespace SteinsDomain
namespace Val

/-- Sorted-and-deduped insert. -/
def insertSorted (v : Val) : List Val → List Val
  | [] => [v]
  | x :: xs =>
    if v = x then x :: xs
    else if v.blt x then v :: x :: xs
    else x :: insertSorted v xs

/-- The canonical form of a value list: strictly increasing, duplicate-free. -/
def canon (vs : List Val) : List Val := vs.foldr insertSorted []

@[simp] theorem canon_nil : canon [] = [] := rfl

@[simp] theorem canon_cons (v : Val) (vs : List Val) :
    canon (v :: vs) = insertSorted v (canon vs) := rfl

/-- Strictly increasing — sortedness and duplicate-freedom in one predicate. -/
def SSorted : List Val → Prop
  | [] => True
  | [_] => True
  | a :: b :: l => a.blt b = true ∧ SSorted (b :: l)

/-! ## Membership -/

@[simp] theorem mem_insertSorted (v w : Val) (l : List Val) :
    v ∈ insertSorted w l ↔ v = w ∨ v ∈ l := by
  induction l with
  | nil => simp [insertSorted]
  | cons x xs ih =>
    unfold insertSorted
    split
    · rename_i h; subst h; grind
    · split
      · grind
      · simp only [List.mem_cons, ih]; grind

@[simp] theorem mem_canon (v : Val) (vs : List Val) : v ∈ canon vs ↔ v ∈ vs := by
  induction vs with
  | nil => simp
  | cons x xs ih => simp only [canon_cons, mem_insertSorted, ih, List.mem_cons]

/-! ## Sortedness -/

theorem ssorted_tail {a : Val} {l : List Val} (h : SSorted (a :: l)) : SSorted l := by
  cases l with
  | nil => trivial
  | cons b l => exact h.2

/-- A head strictly below every member extends a sorted list. -/
theorem ssorted_cons {a : Val} {l : List Val} (h : SSorted l)
    (hlt : ∀ v ∈ l, a.blt v = true) : SSorted (a :: l) := by
  cases l with
  | nil => trivial
  | cons b l => exact ⟨hlt b (by simp), h⟩

/-- In a sorted list the head is strictly below every later member. -/
theorem head_blt_of_mem : ∀ {a : Val} {l : List Val}, SSorted (a :: l) →
    ∀ {v : Val}, v ∈ l → a.blt v = true := by
  intro a l
  induction l generalizing a with
  | nil => intro _ v hv; exact absurd hv (by simp)
  | cons x l ih =>
    intro hs v hv
    obtain ⟨hax, hxs⟩ := hs
    rcases List.mem_cons.mp hv with rfl | hv
    · exact hax
    · exact Val.blt_trans hax (ih hxs hv)

theorem head_not_mem {a : Val} {l : List Val} (h : SSorted (a :: l)) : a ∉ l := by
  intro hmem
  have := head_blt_of_mem h hmem
  rw [blt_irrefl] at this
  exact absurd this (by simp)

theorem ssorted_insertSorted (v : Val) : ∀ {l : List Val}, SSorted l →
    SSorted (insertSorted v l) := by
  intro l
  induction l with
  | nil => intro _; trivial
  | cons x xs ih =>
    intro hs
    unfold insertSorted
    split
    · exact hs
    · rename_i hne
      split
      · rename_i hlt; exact ⟨hlt, hs⟩
      · rename_i hnlt
        refine ssorted_cons (ih (ssorted_tail hs)) ?_
        intro w hw
        rcases (mem_insertSorted w v xs).mp hw with rfl | hw
        · rcases blt_total hne with h | h
          · exact absurd h (by simp [hnlt])
          · exact h
        · exact head_blt_of_mem hs hw

theorem ssorted_canon (vs : List Val) : SSorted (canon vs) := by
  induction vs with
  | nil => trivial
  | cons x xs ih => exact ssorted_insertSorted x ih

/-! ## Canonicity

The lemma the Rust invariant comment implies but never proves: a strictly
increasing list is determined by its member set. -/

theorem ssorted_ext : ∀ {xs ys : List Val}, SSorted xs → SSorted ys →
    (∀ v, v ∈ xs ↔ v ∈ ys) → xs = ys := by
  intro xs
  induction xs with
  | nil =>
    intro ys _ _ h
    cases ys with
    | nil => rfl
    | cons y ys => exact absurd ((h y).mpr (by simp)) (by simp)
  | cons x xs ih =>
    intro ys hx hy h
    cases ys with
    | nil => exact absurd ((h x).mp (by simp)) (by simp)
    | cons y ys =>
      have hxy : x = y := by
        rcases List.mem_cons.mp ((h x).mp (by simp)) with hx' | hx'
        · exact hx'
        · rcases List.mem_cons.mp ((h y).mpr (by simp)) with hy' | hy'
          · exact hy'.symm
          · have hcyc := Val.blt_trans (head_blt_of_mem hx hy') (head_blt_of_mem hy hx')
            rw [blt_irrefl] at hcyc
            exact absurd hcyc (by simp)
      subst hxy
      have htail : ∀ v, v ∈ xs ↔ v ∈ ys := by
        intro v
        constructor
        · intro hv
          rcases List.mem_cons.mp ((h v).mp (List.mem_cons_of_mem _ hv)) with rfl | hv'
          · exact absurd (head_not_mem hx) (by simp [hv])
          · exact hv'
        · intro hv
          rcases List.mem_cons.mp ((h v).mpr (List.mem_cons_of_mem _ hv)) with rfl | hv'
          · exact absurd (head_not_mem hy) (by simp [hv])
          · exact hv'
      rw [ih (ssorted_tail hx) (ssorted_tail hy) htail]

/-- Equal member sets give literally equal canonical forms. -/
theorem canon_ext {xs ys : List Val} (h : ∀ v, v ∈ xs ↔ v ∈ ys) : canon xs = canon ys :=
  ssorted_ext (ssorted_canon xs) (ssorted_canon ys) (by simp [h])

theorem canon_idem (vs : List Val) : canon (canon vs) = canon vs :=
  canon_ext (by simp)

/-- An already-canonical list is its own canonical form — the invariant the
`oneOf` layer carries. -/
theorem canon_eq_self {vs : List Val} (h : SSorted vs) : canon vs = vs :=
  ssorted_ext (ssorted_canon vs) h (by simp)

/-- Set union is commutative, so the finite layer's join cannot depend on which
branch was walked first. -/
theorem canon_append_comm (xs ys : List Val) : canon (xs ++ ys) = canon (ys ++ xs) :=
  canon_ext (by intro v; simp [List.mem_append]; grind)

/-- Canonicalising an operand first changes nothing. -/
theorem canon_append_canon_left (xs ys : List Val) :
    canon (canon xs ++ ys) = canon (xs ++ ys) :=
  canon_ext (by intro v; simp [List.mem_append])

theorem canon_append_canon_right (xs ys : List Val) :
    canon (xs ++ canon ys) = canon (xs ++ ys) :=
  canon_ext (by intro v; simp [List.mem_append])

end Val
end SteinsDomain
