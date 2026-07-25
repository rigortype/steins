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

/-- `summarize` lands in a finite layer only in the all-null branch. This is what
lets `joinFiniteAbstract`'s `Some(_)` arm conclude that the finite operand was
nothing but nulls. -/
theorem summarize_finite {vals : List Val} {f : Fact} (h : summarize M vals = some f)
    (hf : f.finiteMembers ≠ none) :
    f = .singleton Val.null ∧ ∀ v ∈ vals, v = Val.null := by
  unfold summarize at h
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
      · cases b <;> simp only at h <;>
          first
            | (split at h <;> injection h with h <;> subst h <;>
                exact hf (by first | rfl | simp))
            | (injection h with h; subst h; exact hf (by first | rfl | simp))

/-- **The computed widening never loses a member.** Every value the list contains
is admitted by the summary an overflowing set widens to. -/
theorem summarize_admits {vals : List Val} {f : Fact} {v : Val}
    (hv : v ∈ vals) (h : summarize M vals = some f) : admits M f v = true := by
  unfold summarize at h
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
    (hv : v ∈ vals) (h : fromVals M vals = some f) : admits M f v = true := by
  have hvc : v ∈ Val.canon vals := (Val.mem_canon v vals).mpr hv
  unfold fromVals at h
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
  unfold joinAbstract at h
  cases a with
  | singleton _ => simp [abstractParts] at h
  | oneOf _ => simp [abstractParts] at h
  | refined ab ar an =>
    cases b with
    | singleton _ => simp [abstractParts] at h
    | oneOf _ => simp [abstractParts] at h
    | refined bb br bn =>
      simp only [abstractParts] at h
      split at h
      · exact absurd h (by simp)
      · cases ar with
        | str p =>
          cases br with
          | str q =>
            injection h with h; subst h
            exact Sub.trans (sub_refined_str (StrPreds.inter_containsAll_left p q)
              (fun hn => by simp [hn])) sub_mkRefined
          | int _ =>
            injection h with h; subst h
            exact sub_refined_general (fun hn => by simp [hn])
        | int r =>
          cases br with
          | str _ =>
            injection h with h; subst h
            exact sub_refined_general (fun hn => by simp [hn])
          | int s =>
            injection h with h; subst h
            exact Sub.trans (sub_refined_int (IntRange.hull_containsRange_left r s)
              (fun hn => by simp [hn])) sub_mkRefined
    | general bb bn =>
      simp only [abstractParts] at h
      split at h
      · exact absurd h (by simp)
      · injection h with h; subst h
        exact sub_refined_general (fun hn => by simp [hn])
  | general ab an =>
    cases b with
    | singleton _ => simp [abstractParts] at h
    | oneOf _ => simp [abstractParts] at h
    | refined bb br bn =>
      simp only [abstractParts] at h
      split at h
      · exact absurd h (by simp)
      · injection h with h; subst h
        exact sub_general_nullable (fun hn => by simp [hn])
    | general bb bn =>
      simp only [abstractParts] at h
      split at h
      · exact absurd h (by simp)
      · injection h with h; subst h
        exact sub_general_nullable (fun hn => by simp [hn])

/-- `joinAbstract` is commutative: the base test is symmetric and the two
refinement operations (`inter`, `hull`) are. -/
theorem joinAbstract_comm (a b : Fact) : joinAbstract a b = joinAbstract b a := by
  cases a with
  | singleton _ => cases b <;> rfl
  | oneOf _ => cases b <;> rfl
  | refined ab ar an =>
    cases b with
    | singleton _ => rfl
    | oneOf _ => rfl
    | refined bb br bn =>
      simp only [joinAbstract, abstractParts]
      by_cases hb : ab = bb
      · subst hb
        rw [if_neg (fun h => h rfl), if_neg (fun h => h rfl)]
        cases ar <;> cases br <;>
          simp [StrPreds.inter_comm, IntRange.hull_comm, Bool.or_comm]
      · rw [if_pos hb, if_pos (fun h => hb h.symm)]
    | general bb bn =>
      simp only [joinAbstract, abstractParts]
      by_cases hb : ab = bb
      · subst hb
        rw [if_neg (fun h => h rfl), if_neg (fun h => h rfl)]
        cases ar <;> simp [Bool.or_comm]
      · rw [if_pos hb, if_pos (fun h => hb h.symm)]
  | general ab an =>
    cases b with
    | singleton _ => rfl
    | oneOf _ => rfl
    | refined bb br bn =>
      simp only [joinAbstract, abstractParts]
      by_cases hb : ab = bb
      · subst hb
        rw [if_neg (fun h => h rfl), if_neg (fun h => h rfl)]
        cases br <;> simp [Bool.or_comm]
      · rw [if_pos hb, if_pos (fun h => hb h.symm)]
    | general bb bn =>
      simp only [joinAbstract, abstractParts]
      by_cases hb : ab = bb
      · subst hb
        rw [if_neg (fun h => h rfl), if_neg (fun h => h rfl)]
        simp [Bool.or_comm]
      · rw [if_pos hb, if_pos (fun h => hb h.symm)]

theorem joinAbstract_sub_right {a b j : Fact} (h : joinAbstract a b = some j) : Sub M b j :=
  joinAbstract_sub_left (M := M) (by rw [joinAbstract_comm]; exact h)

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
      | singleton _ => exact absurd h (by simp)
      | oneOf _ => exact absurd h (by simp)
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
      | singleton _ => simp [finiteMembers] at habs
      | oneOf _ => simp [finiteMembers] at habs
    · exact joinAbstract_sub_right h v hv

/-- **`γ(a) ∪ γ(b) ⊆ γ(join a b)`** — the soundness contract of ADR-0035, for
every value and every model. A `none` join is ⊤, which is why the statement is
phrased over `denotes`. -/
theorem join_sound (M : Model) (a b : Fact) (v : Val)
    (h : a.admits M v = true ∨ b.admits M v = true) : denotes M (join M a b) v := by
  unfold join
  split
  · rename_i xs ys hxs hys
    have hmem : v ∈ xs ++ ys := by
      rcases h with h | h
      · exact List.mem_append.mpr (Or.inl (mem_of_admits_finite hxs h))
      · exact List.mem_append.mpr (Or.inr (mem_of_admits_finite hys h))
    cases hj : fromVals M (xs ++ ys) with
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
    fromVals M (xs ++ ys) = fromVals M (ys ++ xs) := by
  unfold fromVals
  rw [Val.canon_append_comm]

/-- **The join is commutative** — the all-inputs generalisation of the
`join_is_commutative` proptest. -/
theorem join_comm (M : Model) (a b : Fact) : join M a b = join M b a := by
  unfold join
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
