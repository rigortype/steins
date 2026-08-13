import SteinsDomain.Fact

/-!
# Soundness of the value domain

The theorems this spike exists for. The Rust crate states them as doc comments
and checks them with five proptests (`crates/steins-domain/tests/lattice.rs`);
here they hold for *every* value, every predicate set, every interval, and every
string classifier.

The headline is `join_sound`:

    γ(a) ∪ γ(b) ⊆ γ(join a b)

i.e. a join may lose precision (widen), never members. Since the zero-FP bar
(ADR-0002) is exactly the claim that `γ(fact)` contains every value the program
can produce, this is the algebraic half of the product's central promise.

Note what the soundness proofs do **not** use: `Model.predsOf_closed`. The
implication closure is a canonicity/precision property, not a soundness one —
`admits` and `join` stay sound on unclosed predicate sets, which the Rust type
can hold (`StrPreds::NUMERIC` alone is such a value).
-/

namespace SteinsDomain
namespace Fact

variable {M : Model}

/-- Denotation containment on `Option Fact`, with `none = ⊤`.

`none` really is ⊤ in the consuming code: `join_envs` drops the binding when a
join is unrepresentable, i.e. it stops claiming anything about the value. -/
def denotes (M : Model) : Option Fact → Val → Prop
  | none, _ => True
  | some g, v => g.admits M v = true

/-- `f`'s denotation is contained in `g`'s. -/
def Sub (M : Model) (f g : Fact) : Prop := ∀ v, f.admits M v = true → g.admits M v = true

theorem Sub.refl (f : Fact) : Sub M f f := fun _ h => h

theorem Sub.trans {f g h : Fact} (h₁ : Sub M f g) (h₂ : Sub M g h) : Sub M f h :=
  fun v hv => h₂ v (h₁ v hv)

/-! ## Elementary admission facts -/

theorem admits_null_refined {b r} {n : Bool} (h : n = true) :
    admits M (.refined b r n) Val.null = true := by simp [admits, h]

theorem admits_null_general {b} {n : Bool} (h : n = true) :
    admits M (.general b n) Val.null = true := by simp [admits, h]

theorem nullable_of_admits_null {b r} {n : Bool}
    (h : admits M (.refined b r n) Val.null = true) : n = true := by
  simpa [admits] using h

theorem nullable_of_admits_null_general {b} {n : Bool}
    (h : admits M (.general b n) Val.null = true) : n = true := by
  simpa [admits] using h

@[simp] theorem finiteMembers_mkRefined (b : Base) (r : Refinement) (n : Bool) :
    (mkRefined b r n).finiteMembers = none := by
  cases r <;> simp only [mkRefined] <;> split <;> rfl

/-- A fact in a finite layer admits exactly the members of its list. -/
theorem mem_of_admits_finite {a : Fact} {xs : List Val} {v : Val}
    (hxs : a.finiteMembers = some xs) (hv : a.admits M v = true) : v ∈ xs := by
  cases a with
  | singleton w =>
    simp only [finiteMembers, Option.some.injEq] at hxs
    subst hxs
    have : w = v := by simpa [admits] using hv
    simp [this]
  | oneOf ws =>
    simp only [finiteMembers, Option.some.injEq] at hxs
    subst hxs
    simpa [admits] using hv
  | refined _ _ _ => simp [finiteMembers] at hxs
  | general _ _ => simp [finiteMembers] at hxs
  | union _ _ => simp [finiteMembers] at hxs
  | shape _ _ => simp [finiteMembers] at hxs

/-! ## Widening steps

Each lemma is one arrow of the layer descent: weakening a refinement, dropping
it, or turning nullability on. Every clause of the join is built from these,
which is why soundness reduces to composing them. -/

/-- Turning nullability on widens the Refined layer. -/
theorem sub_refined_nullable {b r} {n n' : Bool} (hn : n = true → n' = true) :
    Sub M (.refined b r n) (.refined b r n') := by
  intro v hv
  cases v with
  | null => exact admits_null_refined (hn (nullable_of_admits_null hv))
  | bool _ => simpa only [admits] using hv
  | int _ => simpa only [admits] using hv
  | float _ => simpa only [admits] using hv
  | str _ => simpa only [admits] using hv
  | arr _ => simpa only [admits] using hv

/-- Turning nullability on widens the General layer. -/
theorem sub_general_nullable {b} {n n' : Bool} (hn : n = true → n' = true) :
    Sub M (.general b n) (.general b n') := by
  intro v hv
  cases v with
  | null => exact admits_null_general (hn (nullable_of_admits_null_general hv))
  | bool _ => simpa only [admits] using hv
  | int _ => simpa only [admits] using hv
  | float _ => simpa only [admits] using hv
  | str _ => simpa only [admits] using hv
  | arr _ => simpa only [admits] using hv

/-- Dropping the refinement widens (layer 3 → layer 4). -/
theorem sub_refined_general {b r} {n n' : Bool} (hn : n = true → n' = true) :
    Sub M (.refined b r n) (.general b n') := by
  intro v hv
  cases v with
  | null => exact admits_null_general (hn (nullable_of_admits_null hv))
  | bool _ => simp only [admits, Bool.and_eq_true] at hv ⊢; exact hv.1
  | int _ => simp only [admits, Bool.and_eq_true] at hv ⊢; exact hv.1
  | float _ => simp only [admits, Bool.and_eq_true] at hv ⊢; exact hv.1
  | str _ => simp only [admits, Bool.and_eq_true] at hv ⊢; exact hv.1
  | arr _ => simp only [admits, Bool.and_eq_true] at hv ⊢; exact hv.1

/-- Weakening a string refinement widens. -/
theorem sub_refined_str {b} {n n' : Bool} {p p' : StrPreds}
    (hp : p.containsAll p' = true) (hn : n = true → n' = true) :
    Sub M (.refined b (.str p) n) (.refined b (.str p') n') := by
  intro v hv
  cases v with
  | null => exact admits_null_refined (hn (nullable_of_admits_null hv))
  | str k =>
    simp only [admits, refAdmits, Bool.and_eq_true] at hv ⊢
    exact ⟨hv.1, StrPreds.containsAll_trans hv.2 hp⟩
  | bool _ => simp [admits, refAdmits] at hv
  | int _ => simp [admits, refAdmits] at hv
  | float _ => simp [admits, refAdmits] at hv
  | arr _ => simp [admits, refAdmits] at hv

/-- Widening an int refinement widens. -/
theorem sub_refined_int {b} {n n' : Bool} {q q' : IntRange}
    (hq : q'.containsRange q = true) (hn : n = true → n' = true) :
    Sub M (.refined b (.int q) n) (.refined b (.int q') n') := by
  intro v hv
  cases v with
  | null => exact admits_null_refined (hn (nullable_of_admits_null hv))
  | int i =>
    simp only [admits, refAdmits, Bool.and_eq_true] at hv ⊢
    exact ⟨hv.1, IntRange.contains_of_containsRange hq hv.2⟩
  | bool _ => simp [admits, refAdmits] at hv
  | float _ => simp [admits, refAdmits] at hv
  | str _ => simp [admits, refAdmits] at hv
  | arr _ => simp [admits, refAdmits] at hv

/-- The normalising constructor never loses a member: `mkRefined` either keeps
the refinement or drops it, and dropping widens. -/
theorem sub_mkRefined {b r} {n : Bool} : Sub M (.refined b r n) (mkRefined b r n) := by
  cases r with
  | str p =>
    simp only [mkRefined]
    split
    · exact sub_refined_general (fun h => h)
    · exact Sub.refl _
  | int q =>
    simp only [mkRefined]
    split
    · exact sub_refined_general (fun h => h)
    · exact Sub.refl _

/-! ## The union layer (issue #339)

`mkUnion` is the only way into `Fact.union`, so everything the soundness proofs
need from the layer is one property: **an arm that admits `v` survives the
merge**. The merge is a left fold of `insertArm`, so the property is proved once
for `insertArm` — for an arm already in the list, and for the one going in — and
then lifted through the fold.

Commutativity needs more: that the merged list is a *canonical form*, determined
by the per-base aggregate of the input and nothing else. `armsFind` reads that
form, `armsAgg` computes the aggregate, and `armsSorted_ext` is the
extensionality that turns "same aggregate" into "same list" — the arm-list
analogue of `Val.ssorted_ext`. -/

/-- The refinement side of an arm's membership test. `none` is that base's
General, which admits every value of the base. -/
def refOptAdmits (M : Model) (r : Option Refinement) (v : Val) : Bool :=
  match r with
  | some rr => refAdmits M rr v
  | none => true

/-- One union arm's membership test, exactly as `admits` spells it inline. -/
def armAdmits (M : Model) (v : Val) (a : Base × Option Refinement) : Bool :=
  decide (v.base = some a.1) && refOptAdmits M a.2 v

theorem admits_union_eq {arms : List (Base × Option Refinement)} {n : Bool} {v : Val}
    (hv : v ≠ Val.null) : admits M (.union arms n) v = arms.any (armAdmits M v) := by
  cases v with
  | null => exact absurd rfl hv
  | bool _ => simp only [admits]; rfl
  | int _ => simp only [admits]; rfl
  | float _ => simp only [admits]; rfl
  | str _ => simp only [admits]; rfl
  | arr _ => simp only [admits]; rfl

/-- Normalising an arm widens it: a contentless refinement becomes that base's
General, which admits strictly more. -/
theorem refOptAdmits_normArm {r : Option Refinement} {v : Val}
    (h : refOptAdmits M r v = true) : refOptAdmits M (normArm r) v = true := by
  cases r with
  | none => exact h
  | some rr =>
    simp only [normArm]
    split
    · rfl
    · exact h

/-- **The refinement join is a widening.** Whatever the left operand admits, the
join admits — the arm-level reading of `sub_refined_str`/`sub_refined_int`. -/
theorem refOptAdmits_joinRefinements_left {r s : Option Refinement} {v : Val}
    (h : refOptAdmits M r v = true) : refOptAdmits M (joinRefinements r s) v = true := by
  cases r with
  | none => simp [joinRefinements, refOptAdmits]
  | some rr =>
    cases s with
    | none => cases rr <;> simp [joinRefinements, refOptAdmits]
    | some ss =>
      cases rr with
      | str p =>
        cases ss with
        | int _ => simp [joinRefinements, refOptAdmits]
        | str q =>
          cases v <;>
            simp only [joinRefinements, refOptAdmits, refAdmits] at h ⊢ <;>
            first
              | exact StrPreds.containsAll_trans h (StrPreds.inter_containsAll_left p q)
              | exact absurd h (by simp)
      | int q =>
        cases ss with
        | str _ => simp [joinRefinements, refOptAdmits]
        | int s =>
          cases v <;>
            simp only [joinRefinements, refOptAdmits, refAdmits] at h ⊢ <;>
            first
              | exact IntRange.contains_of_containsRange (IntRange.hull_containsRange_left q s) h
              | exact absurd h (by simp)

theorem joinRefinements_comm (r s : Option Refinement) :
    joinRefinements r s = joinRefinements s r := by
  cases r with
  | none => cases s with
    | none => rfl
    | some ss => cases ss <;> rfl
  | some rr =>
    cases s with
    | none => cases rr <;> rfl
    | some ss =>
      cases rr <;> cases ss <;>
        simp [joinRefinements, StrPreds.inter_comm, IntRange.hull_comm]

theorem refOptAdmits_joinRefinements_right {r s : Option Refinement} {v : Val}
    (h : refOptAdmits M s v = true) : refOptAdmits M (joinRefinements r s) v = true := by
  rw [joinRefinements_comm]; exact refOptAdmits_joinRefinements_left h

/-! ### An admitting arm survives the merge -/

theorem any_armAdmits_insertArm {M : Model} {v : Val} {a : Base × Option Refinement} :
    ∀ arms : List (Base × Option Refinement), arms.any (armAdmits M v) = true →
      (insertArm arms a).any (armAdmits M v) = true := by
  intro arms
  induction arms with
  | nil => intro h; simp at h
  | cons x rest ih =>
    intro h
    simp only [List.any_cons, Bool.or_eq_true] at h
    simp only [insertArm]
    split
    · simp only [List.any_cons, Bool.or_eq_true]
      rcases h with h | h
      · left
        simp only [armAdmits, Bool.and_eq_true] at h ⊢
        exact ⟨h.1, refOptAdmits_normArm (refOptAdmits_joinRefinements_left h.2)⟩
      · exact Or.inr h
    · split
      · simp only [List.any_cons, Bool.or_eq_true]
        rcases h with h | h
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr h)
      · simp only [List.any_cons, Bool.or_eq_true]
        rcases h with h | h
        · exact Or.inl h
        · exact Or.inr (ih h)

theorem any_armAdmits_insertArm_self {M : Model} {v : Val} {a : Base × Option Refinement}
    (ha : armAdmits M v a = true) :
    ∀ arms : List (Base × Option Refinement), (insertArm arms a).any (armAdmits M v) = true := by
  intro arms
  induction arms with
  | nil =>
    simp only [insertArm, List.any_cons, List.any_nil, Bool.or_false]
    simp only [armAdmits, Bool.and_eq_true] at ha ⊢
    exact ⟨ha.1, refOptAdmits_normArm ha.2⟩
  | cons x rest ih =>
    simp only [insertArm]
    split
    · rename_i hx
      simp only [List.any_cons, Bool.or_eq_true]
      left
      simp only [armAdmits, Bool.and_eq_true] at ha ⊢
      refine ⟨?_, refOptAdmits_normArm (refOptAdmits_joinRefinements_right ha.2)⟩
      show decide (v.base = some x.1) = true
      rw [hx]; exact ha.1
    · split
      · simp only [List.any_cons, Bool.or_eq_true]
        left
        simp only [armAdmits, Bool.and_eq_true] at ha ⊢
        exact ⟨ha.1, refOptAdmits_normArm ha.2⟩
      · simp only [List.any_cons, Bool.or_eq_true]
        exact Or.inr ih

theorem any_armAdmits_foldl {M : Model} {v : Val} :
    ∀ arms acc : List (Base × Option Refinement),
      acc.any (armAdmits M v) = true ∨ arms.any (armAdmits M v) = true →
      (arms.foldl insertArm acc).any (armAdmits M v) = true := by
  intro arms
  induction arms with
  | nil =>
    intro acc h
    rcases h with h | h
    · simpa using h
    · simp at h
  | cons x rest ih =>
    intro acc h
    rw [List.foldl_cons]
    refine ih _ ?_
    rcases h with h | h
    · exact Or.inl (any_armAdmits_insertArm _ h)
    · simp only [List.any_cons, Bool.or_eq_true] at h
      rcases h with h | h
      · exact Or.inl (any_armAdmits_insertArm_self h _)
      · exact Or.inr h

/-! ### What `mkUnion` returns -/

theorem finiteMembers_mkUnion {arms : List (Base × Option Refinement)} {n : Bool} {f : Fact}
    (h : mkUnion arms n = some f) : f.finiteMembers = none := by
  simp only [mkUnion] at h
  split at h
  · exact absurd h (by simp)
  · rename_i b r _
    cases r with
    | none => simp only at h; injection h with h; subst h; rfl
    | some rr => simp only at h; injection h with h; subst h; exact finiteMembers_mkRefined b rr n
  · injection h with h; subst h; rfl

theorem mkUnion_admits_null {arms : List (Base × Option Refinement)} {n : Bool} {f : Fact}
    (hn : n = true) (h : mkUnion arms n = some f) : admits M f Val.null = true := by
  subst hn
  simp only [mkUnion] at h
  split at h
  · exact absurd h (by simp)
  · rename_i b r _
    cases r with
    | none => simp only at h; injection h with h; subst h; exact admits_null_general rfl
    | some rr =>
      simp only at h; injection h with h; subst h
      exact sub_mkRefined Val.null (admits_null_refined rfl)
  · injection h with h; subst h; simp [admits]

/-- **`mkUnion` keeps every arm's members.** The union the constructor returns
admits `v` as soon as one input arm does — through the per-base merge, through
the refinement join, and through the one- and two-arm collapses. -/
theorem mkUnion_admits {arms : List (Base × Option Refinement)} {n : Bool} {f : Fact} {v : Val}
    (hv : v ≠ Val.null) (harm : arms.any (armAdmits M v) = true)
    (h : mkUnion arms n = some f) : admits M f v = true := by
  have hm : (arms.foldl insertArm []).any (armAdmits M v) = true :=
    any_armAdmits_foldl arms [] (Or.inr harm)
  simp only [mkUnion] at h
  split at h
  · exact absurd h (by simp)
  · rename_i b r hl
    rw [hl] at hm
    simp only [List.any_cons, List.any_nil, Bool.or_false, armAdmits, Bool.and_eq_true,
      decide_eq_true_eq] at hm
    cases r with
    | none =>
      simp only at h; injection h with h; subst h
      cases v <;>
        simp only [admits, decide_eq_true_eq] <;>
        first | exact absurd rfl hv | exact hm.1
    | some rr =>
      simp only at h; injection h with h; subst h
      refine sub_mkRefined v ?_
      cases v <;>
        simp only [admits, Bool.and_eq_true, decide_eq_true_eq] <;>
        first | exact absurd rfl hv | exact ⟨hm.1, hm.2⟩
  · injection h with h; subst h
    rw [admits_union_eq hv]
    exact hm

/-! ### The arms an abstract fact presents -/

theorem admits_eq_any_abstractArms {a : Fact} {arms : List (Base × Option Refinement)} {n : Bool}
    (ha : a.abstractArms = some (arms, n)) {v : Val} (hv : v ≠ Val.null) :
    a.admits M v = arms.any (armAdmits M v) := by
  cases a with
  | singleton _ => simp [abstractArms] at ha
  | oneOf _ => simp [abstractArms] at ha
  | shape _ _ => simp [abstractArms] at ha
  | refined b r m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨h1, h2⟩ := ha; subst h1; subst h2
    cases v <;>
      first
        | exact absurd rfl hv
        | simp only [admits, List.any_cons, List.any_nil, Bool.or_false, armAdmits, refOptAdmits]
  | general b m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨h1, h2⟩ := ha; subst h1; subst h2
    cases v <;>
      first
        | exact absurd rfl hv
        | simp only [admits, List.any_cons, List.any_nil, Bool.or_false, armAdmits, refOptAdmits,
            Bool.and_true]
  | union as m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨h1, h2⟩ := ha; subst h1; subst h2
    exact admits_union_eq hv

theorem nullable_of_abstractArms {a : Fact} {arms : List (Base × Option Refinement)} {n : Bool}
    (ha : a.abstractArms = some (arms, n)) (hv : a.admits M Val.null = true) : n = true := by
  cases a with
  | singleton _ => simp [abstractArms] at ha
  | oneOf _ => simp [abstractArms] at ha
  | shape _ _ => simp [abstractArms] at ha
  | refined b r m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨_, h2⟩ := ha; subst h2
    exact nullable_of_admits_null hv
  | general b m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨_, h2⟩ := ha; subst h2
    exact nullable_of_admits_null_general hv
  | union as m =>
    simp only [abstractArms, Option.some.injEq, Prod.mk.injEq] at ha
    obtain ⟨_, h2⟩ := ha; subst h2
    simpa [admits] using hv

/-! ### The merged arm list is a canonical form

The commutativity half. `insertArm` keeps the list strictly increasing in
`baseRank`, so it holds at most one arm per base; `armsFind` then determines the
list, and what `armsFind` reads back is the per-base aggregate `armsAgg` of the
input — a commutative monoid, so appending in either order gives the same fact. -/

theorem baseRank_inj {b c : Base} (h : baseRank b = baseRank c) : b = c := by
  cases b <;> cases c <;> simp_all [baseRank]

/-- Strictly increasing in `baseRank`, hence one arm per base. -/
def ArmsSorted : List (Base × Option Refinement) → Prop
  | [] => True
  | a :: rest => (∀ x ∈ rest, baseRank a.1 < baseRank x.1) ∧ ArmsSorted rest

theorem armsSorted_nil : ArmsSorted [] := True.intro

/-- The arm list's refinement for `b`; `none` when `b` has no arm. -/
def armsFind : List (Base × Option Refinement) → Base → Option (Option Refinement)
  | [], _ => none
  | a :: rest, b => if a.1 = b then some a.2 else armsFind rest b

theorem armsFind_eq_none {l : List (Base × Option Refinement)} {b : Base}
    (h : ∀ x ∈ l, baseRank b < baseRank x.1) : armsFind l b = none := by
  induction l with
  | nil => rfl
  | cons x rest ih =>
    have hx : ¬ x.1 = b := by
      intro he
      have hlt := h x (by simp)
      rw [he] at hlt
      exact absurd hlt (by simp)
    simp only [armsFind, if_neg hx]
    exact ih (fun y hy => h y (by simp [hy]))

theorem exists_of_armsFind {l : List (Base × Option Refinement)} {b : Base}
    {r : Option Refinement} (h : armsFind l b = some r) : ∃ x ∈ l, x.1 = b := by
  induction l with
  | nil => simp [armsFind] at h
  | cons x rest ih =>
    simp only [armsFind] at h
    split at h
    · rename_i hx; exact ⟨x, by simp, hx⟩
    · obtain ⟨y, hy, hy2⟩ := ih h; exact ⟨y, by simp [hy], hy2⟩

/-- **Extensionality for merged arm lists**: a strictly increasing arm list is
determined by what `armsFind` reads out of it. -/
theorem armsSorted_ext : ∀ {xs ys : List (Base × Option Refinement)},
    ArmsSorted xs → ArmsSorted ys → (∀ b, armsFind xs b = armsFind ys b) → xs = ys := by
  intro xs
  induction xs with
  | nil =>
    intro ys _ _ h
    cases ys with
    | nil => rfl
    | cons y yr =>
      have hy := h y.1
      simp only [armsFind] at hy
      exact absurd hy.symm (by simp)
  | cons x xr ih =>
    intro ys hx hy h
    cases ys with
    | nil =>
      have hx' := h x.1
      simp only [armsFind] at hx'
      exact absurd hx' (by simp)
    | cons y yr =>
      obtain ⟨hx1, hx2⟩ := hx
      obtain ⟨hy1, hy2⟩ := hy
      have hbase : x.1 = y.1 := by
        by_cases hne : x.1 = y.1
        · exact hne
        exfalso
        have h1 := h x.1
        rw [armsFind, if_pos rfl, armsFind, if_neg (fun hh => hne hh.symm)] at h1
        obtain ⟨e, he, he2⟩ := exists_of_armsFind h1.symm
        have hr1 : baseRank y.1 < baseRank x.1 := he2 ▸ hy1 e he
        have h2 := h y.1
        rw [armsFind, if_neg hne, armsFind, if_pos rfl] at h2
        obtain ⟨e', he', he'2⟩ := exists_of_armsFind h2
        have hr2 : baseRank x.1 < baseRank y.1 := he'2 ▸ hx1 e' he'
        omega
      have hval : x.2 = y.2 := by
        have h1 := h x.1
        rw [armsFind, if_pos rfl, armsFind, if_pos hbase.symm] at h1
        exact Option.some.inj h1
      have hxy : x = y := by
        rcases x with ⟨xb, xv⟩; rcases y with ⟨yb, yv⟩
        simp only at hbase hval
        subst hbase; subst hval; rfl
      subst hxy
      refine congrArg (x :: ·) (ih hx2 hy2 (fun b => ?_))
      by_cases hb : x.1 = b
      · subst hb
        rw [armsFind_eq_none hx1, armsFind_eq_none hy1]
      · have := h b
        rwa [armsFind, if_neg hb, armsFind, if_neg hb] at this

theorem base_mem_insertArm {a : Base × Option Refinement} :
    ∀ {l : List (Base × Option Refinement)}, ∀ e ∈ insertArm l a,
      e.1 = a.1 ∨ ∃ e' ∈ l, e'.1 = e.1 := by
  intro l
  induction l with
  | nil =>
    intro e he
    simp only [insertArm, List.mem_singleton] at he
    subst he; exact Or.inl rfl
  | cons x rest ih =>
    intro e he
    simp only [insertArm] at he
    split at he
    · rcases List.mem_cons.mp he with h | h
      · subst h; exact Or.inr ⟨x, by simp, rfl⟩
      · exact Or.inr ⟨e, by simp [h], rfl⟩
    · split at he
      · rcases List.mem_cons.mp he with h | h
        · subst h; exact Or.inl rfl
        · exact Or.inr ⟨e, by simp [h], rfl⟩
      · rcases List.mem_cons.mp he with h | h
        · subst h; exact Or.inr ⟨e, by simp, rfl⟩
        · rcases ih e h with h' | ⟨e', he', he'2⟩
          · exact Or.inl h'
          · exact Or.inr ⟨e', by simp [he'], he'2⟩

theorem armsSorted_insertArm : ∀ (l : List (Base × Option Refinement))
    (a : Base × Option Refinement), ArmsSorted l → ArmsSorted (insertArm l a) := by
  intro l
  induction l with
  | nil => intro a _; exact ⟨by simp, armsSorted_nil⟩
  | cons x rest ih =>
    intro a hs
    obtain ⟨hs1, hs2⟩ := hs
    simp only [insertArm]
    split
    · exact ⟨hs1, hs2⟩
    · rename_i hx
      split
      · rename_i hlt
        refine ⟨?_, hs1, hs2⟩
        intro y hy
        rcases List.mem_cons.mp hy with h | h
        · subst h; exact hlt
        · exact Nat.lt_trans hlt (hs1 y h)
      · rename_i hnlt
        refine ⟨?_, ih a hs2⟩
        intro y hy
        rcases base_mem_insertArm y hy with h | ⟨e, he, he2⟩
        · have hne : baseRank x.1 ≠ baseRank a.1 := fun hb => hx (baseRank_inj hb)
          rw [h]; omega
        · rw [← he2]; exact hs1 e he

theorem armsSorted_foldl : ∀ (arms acc : List (Base × Option Refinement)),
    ArmsSorted acc → ArmsSorted (arms.foldl insertArm acc) := by
  intro arms
  induction arms with
  | nil => intro acc h; exact h
  | cons x rest ih => intro acc h; exact ih _ (armsSorted_insertArm acc x h)

theorem armsFind_insertArm_ne {a : Base × Option Refinement} {b : Base} (hb : ¬ a.1 = b) :
    ∀ l : List (Base × Option Refinement), armsFind (insertArm l a) b = armsFind l b := by
  intro l
  induction l with
  | nil => simp [insertArm, armsFind, hb]
  | cons x rest ih =>
    simp only [insertArm]
    split
    · rename_i hx
      have hxb : ¬ x.1 = b := by rw [hx]; exact hb
      simp only [armsFind, if_neg hxb]
    · split
      · simp only [armsFind, if_neg hb]
      · simp only [armsFind]
        by_cases hxb : x.1 = b
        · rw [if_pos hxb, if_pos hxb]
        · rw [if_neg hxb, if_neg hxb]; exact ih

/-- The accumulated refinement for a base: `none` means "no arm yet". -/
def mergeInto : Option (Option Refinement) → Option Refinement → Option Refinement
  | none, r => r
  | some x, r => joinRefinements x r

theorem armsFind_insertArm_self {a : Base × Option Refinement} :
    ∀ {l : List (Base × Option Refinement)}, ArmsSorted l →
      armsFind (insertArm l a) a.1 = some (normArm (mergeInto (armsFind l a.1) a.2)) := by
  intro l
  induction l with
  | nil => simp [insertArm, armsFind, mergeInto]
  | cons x rest ih =>
    intro hs
    obtain ⟨hs1, hs2⟩ := hs
    simp only [insertArm]
    split
    · rename_i hx
      simp only [armsFind, if_pos hx, mergeInto]
    · rename_i hx
      split
      · rename_i hlt
        have hnone : armsFind (x :: rest) a.1 = none := by
          refine armsFind_eq_none (fun e he => ?_)
          rcases List.mem_cons.mp he with h | h
          · subst h; exact hlt
          · exact Nat.lt_trans hlt (hs1 e h)
        rw [hnone]
        simp [armsFind, mergeInto]
      · simp only [armsFind, if_neg hx]
        exact ih hs2

/-! ### The per-base aggregate is a commutative monoid -/

theorem joinRefinements_assoc (r s t : Option Refinement) :
    joinRefinements (joinRefinements r s) t = joinRefinements r (joinRefinements s t) := by
  cases r with
  | none => cases s <;> cases t <;> rfl
  | some rr =>
    cases s with
    | none => cases rr <;> cases t <;> rfl
    | some ss =>
      cases t with
      | none => cases rr <;> cases ss <;> rfl
      | some tt =>
        cases rr <;> cases ss <;> cases tt <;>
          simp [joinRefinements, StrPreds.inter_assoc, IntRange.hull_assoc]

theorem inter_empty_left (q : StrPreds) : StrPreds.empty.inter q = StrPreds.empty := rfl

theorem isEmpty_empty : StrPreds.empty.isEmpty = true := rfl

theorem containsRange_full_hull {u q : IntRange} (h : u.containsRange IntRange.full = true) :
    (u.hull q).containsRange IntRange.full = true := by
  simp only [IntRange.containsRange, IntRange.hull, Bool.and_eq_true, decide_eq_true_eq] at h ⊢
  omega

/-- **Normalisation is absorbed by the join.** Widening an operand to that base's
General before joining is the same as widening the result — which is what makes
the arm merge associative, hence the union constructor order-independent. -/
theorem normArm_joinRefinements_left (x y : Option Refinement) :
    normArm (joinRefinements (normArm x) y) = normArm (joinRefinements x y) := by
  cases x with
  | none => rfl
  | some r =>
    by_cases he : refinementIsEmpty r = true
    · have h0 : normArm (some r) = none := by simp only [normArm, if_pos he]
      have hl : joinRefinements (none : Option Refinement) y = none := by cases y <;> rfl
      rw [h0, hl]
      cases r with
      | str p =>
        have hp : p = StrPreds.empty :=
          (StrPreds.isEmpty_iff p).mp (by simpa [refinementIsEmpty] using he)
        subst hp
        cases y with
        | none => rfl
        | some s =>
          cases s with
          | str q =>
            simp only [joinRefinements, normArm, refinementIsEmpty, inter_empty_left,
              isEmpty_empty, if_pos]
          | int _ => rfl
      | int u =>
        have hu : u.containsRange IntRange.full = true := by simpa [refinementIsEmpty] using he
        cases y with
        | none => rfl
        | some s =>
          cases s with
          | str _ => rfl
          | int q =>
            simp only [joinRefinements, normArm, refinementIsEmpty,
              containsRange_full_hull hu, if_pos]
    · have h0 : normArm (some r) = some r := by simp only [normArm, if_neg he]
      rw [h0]

theorem normArm_joinRefinements_right (x y : Option Refinement) :
    normArm (joinRefinements x (normArm y)) = normArm (joinRefinements x y) := by
  rw [joinRefinements_comm x (normArm y), joinRefinements_comm x y]
  exact normArm_joinRefinements_left y x

/-- The per-base accumulator: `none` is "no arm", which is the unit. -/
def joinOpt : Option (Option Refinement) → Option (Option Refinement) →
    Option (Option Refinement)
  | none, y => y
  | some x, none => some x
  | some x, some y => some (normArm (joinRefinements x y))

def armsAgg : List (Base × Option Refinement) → Base → Option (Option Refinement)
  | [], _ => none
  | a :: rest, b => joinOpt (if a.1 = b then some (normArm a.2) else none) (armsAgg rest b)

theorem joinOpt_none (x : Option (Option Refinement)) : joinOpt x none = x := by
  cases x <;> rfl

theorem joinOpt_comm (x y : Option (Option Refinement)) : joinOpt x y = joinOpt y x := by
  cases x <;> cases y <;> simp [joinOpt, joinRefinements_comm]

theorem joinOpt_assoc (x y z : Option (Option Refinement)) :
    joinOpt (joinOpt x y) z = joinOpt x (joinOpt y z) := by
  cases x with
  | none => rfl
  | some a =>
    cases y with
    | none => cases z <;> rfl
    | some b =>
      cases z with
      | none => rfl
      | some c =>
        simp only [joinOpt, Option.some.injEq]
        rw [normArm_joinRefinements_left, normArm_joinRefinements_right,
          joinRefinements_assoc]

theorem armsFind_foldl : ∀ (arms acc : List (Base × Option Refinement)) (b : Base),
    ArmsSorted acc →
      armsFind (arms.foldl insertArm acc) b = joinOpt (armsFind acc b) (armsAgg arms b) := by
  intro arms
  induction arms with
  | nil => intro acc b _; simp [armsAgg, joinOpt_none]
  | cons x rest ih =>
    intro acc b hs
    rw [List.foldl_cons, ih _ b (armsSorted_insertArm acc x hs), armsAgg, ← joinOpt_assoc]
    refine congrArg (joinOpt · (armsAgg rest b)) ?_
    by_cases hb : x.1 = b
    · subst hb
      rw [if_pos rfl, armsFind_insertArm_self hs]
      cases armsFind acc x.1 with
      | none => rfl
      | some w => simp only [joinOpt, mergeInto, normArm_joinRefinements_right]
    · rw [if_neg hb, armsFind_insertArm_ne hb, joinOpt_none]

theorem armsAgg_append (xs ys : List (Base × Option Refinement)) (b : Base) :
    armsAgg (xs ++ ys) b = joinOpt (armsAgg xs b) (armsAgg ys b) := by
  induction xs with
  | nil => rfl
  | cons x rest ih =>
    rw [List.cons_append, armsAgg, ih, armsAgg, joinOpt_assoc]

theorem foldl_insertArm_append_comm (xs ys : List (Base × Option Refinement)) :
    (xs ++ ys).foldl insertArm [] = (ys ++ xs).foldl insertArm [] := by
  refine armsSorted_ext (armsSorted_foldl _ _ armsSorted_nil)
    (armsSorted_foldl _ _ armsSorted_nil) (fun b => ?_)
  rw [armsFind_foldl (xs ++ ys) [] b armsSorted_nil,
    armsFind_foldl (ys ++ xs) [] b armsSorted_nil, armsAgg_append, armsAgg_append,
    joinOpt_comm (armsAgg xs b)]

/-- **The union constructor does not read the arm order.** -/
theorem mkUnion_append_comm (xs ys : List (Base × Option Refinement)) (n : Bool) :
    mkUnion (xs ++ ys) n = mkUnion (ys ++ xs) n := by
  simp only [mkUnion, foldl_insertArm_append_comm xs ys]

/-! ## The computed widening is sound -/

theorem intHullOf_eq_none {vals : List Val} (h : intHullOf vals = none) {i : Int} :
    Val.int i ∉ vals := by
  induction vals with
  | nil => simp
  | cons w ws ih =>
    cases w with
    | int j => rw [intHullOf] at h; split at h <;> exact absurd h (by simp)
    | null => simp only [intHullOf] at h; simpa using ih h
    | bool _ => simp only [intHullOf] at h; simpa using ih h
    | float _ => simp only [intHullOf] at h; simpa using ih h
    | str _ => simp only [intHullOf] at h; simpa using ih h
    | arr _ => simp only [intHullOf] at h; simpa using ih h

/-- The hull of a value list contains every int member. -/
theorem intHullOf_contains : ∀ {vals : List Val} {r : IntRange}, intHullOf vals = some r →
    ∀ {i : Int}, Val.int i ∈ vals → r.contains i = true := by
  intro vals
  induction vals with
  | nil => intro r h; exact absurd h (by simp [intHullOf])
  | cons w ws ih =>
    intro r h i hmem
    cases w with
    | int j =>
      rw [intHullOf] at h
      split at h
      · rename_i hnone
        injection h with h; subst h
        rcases List.mem_cons.mp hmem with heq | hmem'
        · injection heq with heq; subst heq; exact IntRange.contains_point i
        · exact absurd hmem' (intHullOf_eq_none hnone)
      · rename_i r' hsome
        injection h with h; subst h
        rcases List.mem_cons.mp hmem with heq | hmem'
        · injection heq with heq; subst heq
          exact IntRange.hull_contains_right (IntRange.contains_point i)
        · exact IntRange.hull_contains_left (ih hsome hmem')
    | null => exact ih (by simpa [intHullOf] using h) (by simpa using hmem)
    | bool _ => exact ih (by simpa [intHullOf] using h) (by simpa using hmem)
    | float _ => exact ih (by simpa [intHullOf] using h) (by simpa using hmem)
    | str _ => exact ih (by simpa [intHullOf] using h) (by simpa using hmem)
    | arr _ => exact ih (by simpa [intHullOf] using h) (by simpa using hmem)

theorem strPredsOf_eq_none {vals : List Val} (h : strPredsOf M vals = none) {k : Nat} :
    Val.str k ∉ vals := by
  induction vals with
  | nil => simp
  | cons w ws ih =>
    cases w with
    | str j => rw [strPredsOf] at h; split at h <;> exact absurd h (by simp)
    | null => simp only [strPredsOf] at h; simpa using ih h
    | bool _ => simp only [strPredsOf] at h; simpa using ih h
    | int _ => simp only [strPredsOf] at h; simpa using ih h
    | float _ => simp only [strPredsOf] at h; simpa using ih h
    | arr _ => simp only [strPredsOf] at h; simpa using ih h

/-- The intersected predicate summary is below every string member's own summary
— the direction `admits` needs, and the sense in which the widening is
*computed* rather than guessed. -/
theorem strPredsOf_below : ∀ {vals : List Val} {p : StrPreds}, strPredsOf M vals = some p →
    ∀ {k : Nat}, Val.str k ∈ vals → (M.predsOf k).containsAll p = true := by
  intro vals
  induction vals with
  | nil => intro p h; exact absurd h (by simp [strPredsOf])
  | cons w ws ih =>
    intro p h k hmem
    cases w with
    | str j =>
      rw [strPredsOf] at h
      split at h
      · rename_i hnone
        injection h with h; subst h
        rcases List.mem_cons.mp hmem with heq | hmem'
        · injection heq with heq; subst heq; exact StrPreds.containsAll_refl _
        · exact absurd hmem' (strPredsOf_eq_none hnone)
      · rename_i p' hsome
        injection h with h; subst h
        rcases List.mem_cons.mp hmem with heq | hmem'
        · injection heq with heq; subst heq
          exact StrPreds.inter_containsAll_right _ _
        · exact StrPreds.containsAll_trans (ih hsome hmem')
            (StrPreds.inter_containsAll_left _ _)
    | null => exact ih (by simpa [strPredsOf] using h) (by simpa using hmem)
    | bool _ => exact ih (by simpa [strPredsOf] using h) (by simpa using hmem)
    | int _ => exact ih (by simpa [strPredsOf] using h) (by simpa using hmem)
    | float _ => exact ih (by simpa [strPredsOf] using h) (by simpa using hmem)
    | arr _ => exact ih (by simpa [strPredsOf] using h) (by simpa using hmem)

private theorem mem_scalars {vals : List Val} {v : Val} (hv : v ∈ vals) (hn : v ≠ Val.null) :
    v ∈ vals.filter (fun w => decide (w ≠ Val.null)) :=
  List.mem_filter.mpr ⟨hv, by simpa using hn⟩

/-- **The mixed-base overflow's arms cover every member.** `v`'s own base gets an
arm — its member list contains `v`, so it is not empty — and that arm's
refinement is `v`'s base summarized over a list `v` belongs to, which is exactly
what `intHullOf_contains`/`strPredsOf_below` bound. -/
private theorem any_armAdmits_unionArms (M : Model) (scalars : List Val) {v : Val} {bb : Base}
    (hbase : v.base = some bb) (hmem : v ∈ scalars) :
    (([Base.int, Base.float, Base.str, Base.bool] : List Base).filterMap (fun b =>
      match scalars.filter (fun w => decide (w.base = some b)) with
      | [] => none
      | _ =>
        match b with
        | .int => some (b, (intHullOf (scalars.filter (fun w => decide (w.base = some b)))).map
            Refinement.int)
        | .str => some (b, (strPredsOf M (scalars.filter (fun w => decide (w.base = some b)))).map
            Refinement.str)
        | _ => some (b, none))).any (armAdmits M v) = true := by
  have hmm : v ∈ scalars.filter (fun w => decide (w.base = some bb)) :=
    List.mem_filter.mpr ⟨hmem, by simp [hbase]⟩
  cases bb with
  | int =>
    obtain ⟨i, rfl⟩ : ∃ i, v = Val.int i := by cases v <;> simp_all [Val.base]
    refine List.any_eq_true.mpr ⟨(Base.int,
      (intHullOf (scalars.filter (fun w => decide (w.base = some Base.int)))).map Refinement.int),
      List.mem_filterMap.mpr ⟨Base.int, by simp, ?_⟩, ?_⟩
    · cases hm : scalars.filter (fun w => decide (w.base = some Base.int)) with
      | nil => rw [hm] at hmm; simp at hmm
      | cons y ys => rfl
    · cases hq : intHullOf (scalars.filter (fun w => decide (w.base = some Base.int))) with
      | none => simp [armAdmits, refOptAdmits, Val.base]
      | some q =>
        have hc : q.contains i = true := intHullOf_contains hq hmm
        simp [armAdmits, refOptAdmits, refAdmits, Val.base, hc]
  | str =>
    obtain ⟨k, rfl⟩ : ∃ k, v = Val.str k := by cases v <;> simp_all [Val.base]
    refine List.any_eq_true.mpr ⟨(Base.str,
      (strPredsOf M (scalars.filter (fun w => decide (w.base = some Base.str)))).map
        Refinement.str),
      List.mem_filterMap.mpr ⟨Base.str, by simp, ?_⟩, ?_⟩
    · cases hm : scalars.filter (fun w => decide (w.base = some Base.str)) with
      | nil => rw [hm] at hmm; simp at hmm
      | cons y ys => rfl
    · cases hp : strPredsOf M (scalars.filter (fun w => decide (w.base = some Base.str))) with
      | none => simp [armAdmits, refOptAdmits, Val.base]
      | some p =>
        have hc : (M.predsOf k).containsAll p = true := strPredsOf_below hp hmm
        simp [armAdmits, refOptAdmits, refAdmits, Val.base, hc]
  | float =>
    obtain ⟨x, rfl⟩ : ∃ x, v = Val.float x := by cases v <;> simp_all [Val.base]
    refine List.any_eq_true.mpr ⟨(Base.float, none),
      List.mem_filterMap.mpr ⟨Base.float, by simp, ?_⟩, ?_⟩
    · cases hm : scalars.filter (fun w => decide (w.base = some Base.float)) with
      | nil => rw [hm] at hmm; simp at hmm
      | cons y ys => rfl
    · simp [armAdmits, refOptAdmits, Val.base]
  | bool =>
    obtain ⟨x, rfl⟩ : ∃ x, v = Val.bool x := by cases v <;> simp_all [Val.base]
    refine List.any_eq_true.mpr ⟨(Base.bool, none),
      List.mem_filterMap.mpr ⟨Base.bool, by simp, ?_⟩, ?_⟩
    · cases hm : scalars.filter (fun w => decide (w.base = some Base.bool)) with
      | nil => rw [hm] at hmm; simp at hmm
      | cons y ys => rfl
    · simp [armAdmits, refOptAdmits, Val.base]

/-- `summarize` lands in a finite layer only in the all-null branch. This is what
lets `joinFiniteAbstract`'s `Some(_)` arm conclude that the finite operand was
nothing but nulls. -/
theorem summarize_finite {vals : List Val} {f : Fact} (h : summarizeScalar M vals = some f)
    (hf : f.finiteMembers ≠ none) :
    f = .singleton Val.null ∧ ∀ v ∈ vals, v = Val.null := by
  unfold summarizeScalar at h
  simp only at h
  split at h
  · rename_i hnil
    injection h with h
    refine ⟨h.symm, fun v hv => ?_⟩
    simpa using List.filter_eq_nil_iff.mp hnil v hv
  · exfalso
    split at h
    · exact absurd h (by simp)
    · rename_i b _
      split at h
      · exact absurd h (by simp)
      · split at h
        -- The mixed-base overflow is a union, and no union is in a finite layer.
        · exact hf (finiteMembers_mkUnion h)
        · cases b <;> simp only at h <;>
            first
              | (split at h <;> injection h with h <;> subst h <;>
                  exact hf (by first | rfl | simp))
              | (injection h with h; subst h; exact hf (by first | rfl | simp))

/-- **The computed widening never loses a member.** Every value the list contains
is admitted by the summary an overflowing set widens to. -/
theorem summarize_admits {vals : List Val} {f : Fact} {v : Val}
    (hv : v ∈ vals) (h : summarizeScalar M vals = some f) : admits M f v = true := by
  unfold summarizeScalar at h
  simp only at h
  split at h
  · rename_i hnil
    have hvnull : v = Val.null := by simpa using List.filter_eq_nil_iff.mp hnil v hv
    injection h with h; subst h; subst hvnull; simp [admits]
  · rename_i first rest hcons
    have hnullable : v = Val.null → decide (Val.null ∈ vals) = true := by
      intro hvn; subst hvn; simpa using hv
    split at h
    · exact absurd h (by simp)
    · rename_i b hbase
      split at h
      · exact absurd h (by simp)
      · rename_i hallbase
        simp only [Bool.not_eq_true] at hallbase
        split at h
        -- **The mixed-base overflow is a union.** Every non-null member has a
        -- scalar base (the guard just above), so every one of them has an arm.
        · by_cases hvn : v = Val.null
          · subst hvn
            exact mkUnion_admits_null (hnullable rfl) h
          · have hvs : v ∈ List.filter (fun w => decide (w ≠ Val.null)) vals :=
              mem_scalars hv hvn
            obtain ⟨bb, hbb⟩ : ∃ bb, v.base = some bb := by
              cases hvv : v.base with
              | some bb => exact ⟨bb, rfl⟩
              | none =>
                have := List.any_eq_false.mp hallbase v hvs
                simp [hvv] at this
            exact mkUnion_admits hvn
              (any_armAdmits_unionArms M (List.filter (fun w => decide (w ≠ Val.null)) vals)
                hbb hvs) h
        · rename_i hany
          simp only [Bool.not_eq_true] at hany
          have hbase_of : ∀ w ∈ first :: rest, w.base = some b := by
            intro w hw
            have hw' : w ∈ List.filter (fun v => decide (v ≠ Val.null)) vals := by
              rw [hcons]; exact hw
            simpa using List.any_eq_false.mp hany w hw'
          have hvs' : v ≠ Val.null → v ∈ first :: rest := fun hvn => by
            rw [← hcons]; exact mem_scalars hv hvn
          by_cases hvn : v = Val.null
          · subst hvn
            have hn := hnullable rfl
            cases b <;> simp only at h <;>
              first
                | (split at h <;> injection h with h <;> subst h <;>
                    first
                      | exact sub_mkRefined Val.null (admits_null_refined hn)
                      | exact admits_null_general hn)
                | (injection h with h; subst h; exact admits_null_general hn)
          · have hvs : v ∈ first :: rest := hvs' hvn
            have hvb : v.base = some b := hbase_of v hvs
            cases b with
            | int =>
              simp only at h
              obtain ⟨i, rfl⟩ : ∃ i, v = Val.int i := by
                cases v <;> simp_all [Val.base]
              split at h
              · rename_i r hr
                injection h with h; subst h
                refine sub_mkRefined (Val.int i) ?_
                simp only [admits, refAdmits, Bool.and_eq_true, decide_eq_true_eq]
                exact ⟨hvb, intHullOf_contains hr (by rw [hcons]; exact hvs)⟩
              · injection h with h; subst h
                simp only [admits, decide_eq_true_eq]; exact hvb
            | str =>
              simp only at h
              obtain ⟨k, rfl⟩ : ∃ k, v = Val.str k := by
                cases v <;> simp_all [Val.base]
              split at h
              · rename_i p hp
                injection h with h; subst h
                refine sub_mkRefined (Val.str k) ?_
                simp only [admits, refAdmits, Bool.and_eq_true, decide_eq_true_eq]
                exact ⟨hvb, strPredsOf_below hp (by rw [hcons]; exact hvs)⟩
              · injection h with h; subst h
                simp only [admits, decide_eq_true_eq]; exact hvb
            | float =>
              simp only at h
              injection h with h; subst h
              obtain ⟨x, rfl⟩ : ∃ x, v = Val.float x := by cases v <;> simp_all [Val.base]
              simp only [admits, decide_eq_true_eq]; exact hvb
            | bool =>
              simp only at h
              injection h with h; subst h
              obtain ⟨x, rfl⟩ : ∃ x, v = Val.bool x := by cases v <;> simp_all [Val.base]
              simp only [admits, decide_eq_true_eq]; exact hvb

/-- **Layering never loses a member.** The all-values generalisation of the
`from_vals_admits_every_input` proptest. -/
theorem fromVals_admits {vals : List Val} {f : Fact} {v : Val}
    (hv : v ∈ vals) (h : fromValsScalar M vals = some f) : admits M f v = true := by
  have hvc : v ∈ Val.canon vals := (Val.mem_canon v vals).mpr hv
  unfold fromValsScalar at h
  rcases hc : Val.canon vals with _ | ⟨w, tl⟩
  · rw [hc] at hvc; exact absurd hvc (by simp)
  · rw [hc] at h hvc
    cases tl with
    | nil =>
      simp only at h
      injection h with h; subst h
      have : v = w := by simpa using hvc
      subst this; simp [admits]
    | cons y rest =>
      simp only at h
      split at h
      · injection h with h; subst h
        simpa only [admits, decide_eq_true_eq] using hvc
      · exact summarize_admits hvc h

/-! ## The join is sound -/

theorem joinAbstract_sub_left {a b j : Fact} (h : joinAbstract a b = some j) : Sub M a j := by
  intro v hv
  rcases ha : a.abstractArms with _ | ⟨aa, an⟩
  · simp [joinAbstract, ha] at h
  · rcases hb : b.abstractArms with _ | ⟨ba, bn⟩
    · simp [joinAbstract, ha, hb] at h
    · simp only [joinAbstract, ha, hb] at h
      by_cases hvn : v = Val.null
      · subst hvn
        exact mkUnion_admits_null (by simp [nullable_of_abstractArms ha hv]) h
      · refine mkUnion_admits hvn ?_ h
        rw [List.any_append, Bool.or_eq_true]
        exact Or.inl ((admits_eq_any_abstractArms ha hvn) ▸ hv)

theorem joinAbstract_sub_right {a b j : Fact} (h : joinAbstract a b = some j) : Sub M b j := by
  intro v hv
  rcases ha : a.abstractArms with _ | ⟨aa, an⟩
  · simp [joinAbstract, ha] at h
  · rcases hb : b.abstractArms with _ | ⟨ba, bn⟩
    · simp [joinAbstract, ha, hb] at h
    · simp only [joinAbstract, ha, hb] at h
      by_cases hvn : v = Val.null
      · subst hvn
        exact mkUnion_admits_null (by simp [nullable_of_abstractArms hb hv]) h
      · refine mkUnion_admits hvn ?_ h
        rw [List.any_append, Bool.or_eq_true]
        exact Or.inr ((admits_eq_any_abstractArms hb hvn) ▸ hv)

/-- `joinAbstract` is commutative. The arms are concatenated, so this is exactly
`mkUnion_append_comm`: the merged list is a canonical form of the per-base
aggregate, and that aggregate is a commutative monoid. -/
theorem joinAbstract_comm (a b : Fact) : joinAbstract a b = joinAbstract b a := by
  rcases ha : a.abstractArms with _ | ⟨aa, an⟩
  · rcases hb : b.abstractArms with _ | ⟨ba, bn⟩ <;> simp only [joinAbstract, ha, hb]
  · rcases hb : b.abstractArms with _ | ⟨ba, bn⟩
    · simp only [joinAbstract, ha, hb]
    · simp only [joinAbstract, ha, hb]
      rw [mkUnion_append_comm, Bool.or_comm]

theorem joinFiniteAbstract_admits_finite {finite : List Val} {abs j : Fact} {v : Val}
    (hv : v ∈ finite) (h : joinFiniteAbstract M finite abs = some j) :
    admits M j v = true := by
  unfold joinFiniteAbstract at h
  split at h
  · exact absurd h (by simp)
  · rename_i summary hsum
    have hadm : summary.admits M v = true := summarize_admits hv hsum
    split at h
    · rename_i members hfin
      obtain ⟨hsingle, hallnull⟩ := summarize_finite hsum (by simp [hfin])
      have hvnull : v = Val.null := hallnull v hv
      subst hvnull
      cases abs with
      | refined b r n =>
        injection h with h; subst h
        exact sub_mkRefined Val.null (admits_null_refined rfl)
      | general b n => injection h with h; subst h; exact admits_null_general rfl
      -- A union takes the nullability beside its arms; `mkUnion` may collapse
      -- it, but never below `null`.
      | union arms n => exact mkUnion_admits_null rfl h
      | singleton _ => exact absurd h (by simp)
      | oneOf _ => exact absurd h (by simp)
      | shape _ _ => exact absurd h (by simp)
    · exact joinAbstract_sub_left h v hadm

theorem joinFiniteAbstract_admits_abs {finite : List Val} {abs j : Fact} {v : Val}
    (habs : abs.finiteMembers = none) (hv : abs.admits M v = true)
    (h : joinFiniteAbstract M finite abs = some j) : admits M j v = true := by
  unfold joinFiniteAbstract at h
  split at h
  · exact absurd h (by simp)
  · split at h
    · cases abs with
      | refined b r n =>
        injection h with h; subst h
        exact Sub.trans (sub_refined_nullable (fun _ => rfl)) sub_mkRefined v hv
      | general b n =>
        injection h with h; subst h
        exact sub_general_nullable (fun _ => rfl) v hv
      | union arms n =>
        by_cases hvn : v = Val.null
        · subst hvn; exact mkUnion_admits_null rfl h
        · refine mkUnion_admits hvn ?_ h
          exact (admits_eq_any_abstractArms (a := Fact.union arms n) rfl hvn) ▸ hv
      | singleton _ => simp [finiteMembers] at habs
      | oneOf _ => simp [finiteMembers] at habs
      | shape _ _ => exact absurd h (by simp)
    · exact joinAbstract_sub_right h v hv

/-- **`γ(a) ∪ γ(b) ⊆ γ(join a b)`** — the soundness contract of ADR-0035, for
every value and every model. A `none` join is ⊤, which is why the statement is
phrased over `denotes`. -/
theorem join_sound (M : Model) (a b : Fact) (v : Val)
    (h : a.admits M v = true ∨ b.admits M v = true) : denotes M (joinScalar M a b) v := by
  unfold joinScalar
  split
  · rename_i xs ys hxs hys
    have hmem : v ∈ xs ++ ys := by
      rcases h with h | h
      · exact List.mem_append.mpr (Or.inl (mem_of_admits_finite hxs h))
      · exact List.mem_append.mpr (Or.inr (mem_of_admits_finite hys h))
    cases hj : fromValsScalar M (xs ++ ys) with
    | none => simp [denotes]
    | some f => simpa only [denotes] using fromVals_admits hmem hj
  · rename_i xs hxs hys
    cases hj : joinFiniteAbstract M xs b with
    | none => simp [denotes]
    | some f =>
      simp only [denotes]
      rcases h with h | h
      · exact joinFiniteAbstract_admits_finite (mem_of_admits_finite hxs h) hj
      · exact joinFiniteAbstract_admits_abs hys h hj
  · rename_i ys hxs hys
    cases hj : joinFiniteAbstract M ys a with
    | none => simp [denotes]
    | some f =>
      simp only [denotes]
      rcases h with h | h
      · exact joinFiniteAbstract_admits_abs hxs h hj
      · exact joinFiniteAbstract_admits_finite (mem_of_admits_finite hys h) hj
  · rename_i hxs hys
    cases hj : joinAbstract a b with
    | none => simp [denotes]
    | some f =>
      simp only [denotes]
      rcases h with h | h
      · exact joinAbstract_sub_left hj v h
      · exact joinAbstract_sub_right hj v h

/-! ## Commutativity -/

/-- `fromVals` sees only the canonical member set, so appending in either order
gives the same fact. -/
theorem fromVals_append_comm (M : Model) (xs ys : List Val) :
    fromValsScalar M (xs ++ ys) = fromValsScalar M (ys ++ xs) := by
  unfold fromValsScalar
  rw [Val.canon_append_comm]

/-- **The join is commutative** — the all-inputs generalisation of the
`join_is_commutative` proptest. -/
theorem join_comm (M : Model) (a b : Fact) : joinScalar M a b = joinScalar M b a := by
  unfold joinScalar
  cases ha : a.finiteMembers with
  | none =>
    cases hb : b.finiteMembers with
    | none => exact joinAbstract_comm a b
    | some ys => rfl
  | some xs =>
    cases hb : b.finiteMembers with
    | none => rfl
    | some ys => exact fromVals_append_comm M xs ys

end Fact
end SteinsDomain
