import SteinsDomain.Soundness

/-!
# The trinary queries are sound

`truthy`, `satisfiesStr`, and `intIn` answer in `Certainty`, and the proof layer
reads a *decided* answer as a claim about every value the fact admits. These
theorems are that reading — the all-values generalisation of the
`queries_agree_with_witnesses` proptest, which can only check the witnesses it
happened to generate.

`truthy` is where `Model.nonFalsy_iff` earns its keep: on the Refined layer the
verdict is read off the predicate bitset, so it follows only because `NON_FALSY`
really is coupled to PHP string falsiness. That coupling is a *classifier*
obligation, checked on concrete strings by the vector file, not proved here.
-/

namespace SteinsDomain
namespace Fact

variable {M : Model}

/-- `NON_FALSY ⊆ p` is exactly the `nonFalsy` bit. -/
private theorem nonFalsy_of_containsAll {p : StrPreds}
    (h : p.containsAll StrPreds.NON_FALSY = true) : p.nonFalsy = true :=
  (StrPreds.containsAll_iff.mp h).2.1 rfl

/-! ## Reading a decided verdict off the abstract layers -/

private theorem canFalsy_false_of_truthy_yes {f : Fact} (hfin : f.finiteMembers = none)
    (h : f.truthy M = .yes) : f.abstractFalsyTruthy.1 = false := by
  rcases hft : f.abstractFalsyTruthy with ⟨cf, ct⟩
  simp only
  cases cf
  · rfl
  · cases ct <;> simp [truthy, hfin, hft] at h

private theorem canTruthy_false_of_truthy_no {f : Fact} (hfin : f.finiteMembers = none)
    (h : f.truthy M = .no) : f.abstractFalsyTruthy.2 = false := by
  rcases hft : f.abstractFalsyTruthy with ⟨cf, ct⟩
  simp only
  cases ct
  · rfl
  · cases cf <;> simp [truthy, hfin, hft] at h

/-! ### The union's verdict is the disjunction over its arms

`abstractFalsyTruthy` folds the arms, and both `truthy` theorems need to read the
fold back as "every arm says so" — the arm that admits the value is one of them. -/

private theorem foldl_ft_fst : ∀ (arms : List (Base × Option Refinement)) (acc : Bool × Bool),
    (arms.foldl (fun acc a =>
      let ft := armFalsyTruthy a.1 a.2
      (acc.1 || ft.1, acc.2 || ft.2)) acc).1
      = (acc.1 || arms.any (fun a => (armFalsyTruthy a.1 a.2).1)) := by
  intro arms
  induction arms with
  | nil => intro acc; simp
  | cons x rest ih => intro acc; rw [List.foldl_cons, ih]; simp [Bool.or_assoc]

private theorem foldl_ft_snd : ∀ (arms : List (Base × Option Refinement)) (acc : Bool × Bool),
    (arms.foldl (fun acc a =>
      let ft := armFalsyTruthy a.1 a.2
      (acc.1 || ft.1, acc.2 || ft.2)) acc).2
      = (acc.2 || arms.any (fun a => (armFalsyTruthy a.1 a.2).2)) := by
  intro arms
  induction arms with
  | nil => intro acc; simp
  | cons x rest ih => intro acc; rw [List.foldl_cons, ih]; simp [Bool.or_assoc]

private theorem union_canFalsy (arms : List (Base × Option Refinement)) (n : Bool) :
    (Fact.abstractFalsyTruthy (.union arms n)).1
      = (n || arms.any (fun a => (armFalsyTruthy a.1 a.2).1)) := by
  simp only [abstractFalsyTruthy]
  exact foldl_ft_fst arms (n, false)

private theorem union_canTruthy (arms : List (Base × Option Refinement)) (n : Bool) :
    (Fact.abstractFalsyTruthy (.union arms n)).2
      = arms.any (fun a => (armFalsyTruthy a.1 a.2).2) := by
  simp only [abstractFalsyTruthy]
  rw [foldl_ft_snd arms (n, false)]
  simp

/-! ## `truthy` -/

/-- **A `yes` from `truthy` holds for every admitted value.** -/
theorem truthy_yes (M : Model) {f : Fact} {v : Val}
    (h : f.truthy M = .yes) (hv : f.admits M v = true) : v.falsy M = false := by
  cases hb : Val.falsy M v with
  | false => rfl
  | true =>
    exfalso
    cases f with
    | singleton w =>
      have hw : w = v := by simpa [admits] using hv
      rw [hw] at h
      simp [truthy, finiteMembers, Certainty.allOf, Certainty.ofBool, hb] at h
    | oneOf vs =>
      have hmem : v ∈ vs := by simpa [admits] using hv
      have hall := ((Certainty.allOf_eq_yes_iff _).mp
        (by simpa [truthy, finiteMembers] using h)).2
      have hc := hall _ (List.mem_map_of_mem
        (f := fun w => Certainty.ofBool (!Val.falsy M w)) hmem)
      simp [Certainty.ofBool, hb] at hc
    | general b n => simp [truthy, finiteMembers, abstractFalsyTruthy] at h
    | refined b r n =>
      have hcf := canFalsy_false_of_truthy_yes (M := M) (by simp [finiteMembers]) h
      simp only [abstractFalsyTruthy] at hcf
      obtain ⟨hft, hn⟩ := Bool.or_eq_false_iff.mp hcf
      cases b with
      | int =>
        cases r with
        | str p => simp only at hft; exact absurd hft (by simp)
        | int q =>
          simp only at hft
          cases v with
          | int i =>
            have hzero : i = 0 := by
              by_cases hz : i = 0
              · exact hz
              · simp [Val.falsy, hz] at hb
            subst hzero
            simp only [admits, refAdmits, Bool.and_eq_true] at hv
            obtain ⟨-, hv⟩ := hv
            simp only [IntRange.contains, Bool.and_eq_false_iff,
              decide_eq_false_iff_not] at hft
            simp only [IntRange.contains, Bool.and_eq_true, decide_eq_true_eq] at hv
            omega
          | null => rw [hn] at hv; simp [admits] at hv
          | bool _ => simp [admits, refAdmits] at hv
          | float _ => simp [admits, refAdmits] at hv
          | str _ => simp [admits, refAdmits] at hv
          | arr _ => simp [admits, refAdmits] at hv
      | str =>
        cases r with
        | int q => simp only at hft; exact absurd hft (by simp)
        | str p =>
          simp only [Bool.not_eq_false'] at hft
          cases v with
          | str k =>
            simp only [admits, refAdmits, Bool.and_eq_true] at hv
            have hnf := nonFalsy_of_containsAll (StrPreds.containsAll_trans hv.2 hft)
            rw [M.nonFalsy_iff k] at hnf
            simp only [Val.falsy] at hb
            simp_all
          | null => rw [hn] at hv; simp [admits] at hv
          | bool _ => simp [admits, refAdmits] at hv
          | int _ => simp [admits, refAdmits] at hv
          | float _ => simp [admits, refAdmits] at hv
          | arr _ => simp [admits, refAdmits] at hv
      | float => cases r <;> (simp only at hft; exact absurd hft (by simp))
      | bool => cases r <;> (simp only at hft; exact absurd hft (by simp))
    | union arms n =>
      -- A `yes` means no arm can be falsy, and the value sits in one of them.
      have hcf := canFalsy_false_of_truthy_yes (M := M) (by simp [finiteMembers]) h
      rw [union_canFalsy] at hcf
      obtain ⟨hn, hall⟩ := Bool.or_eq_false_iff.mp hcf
      have hvn : v ≠ Val.null := by
        intro he; subst he; rw [hn] at hv; simp [admits] at hv
      rw [admits_union_eq hvn] at hv
      obtain ⟨a, hmem, ha⟩ := List.any_eq_true.mp hv
      have hft := List.any_eq_false.mp hall a hmem
      simp only [Bool.not_eq_true] at hft
      obtain ⟨ab, ar⟩ := a
      simp only [armAdmits, Bool.and_eq_true, decide_eq_true_eq] at ha
      obtain ⟨hab, har⟩ := ha
      cases ab with
      | float => rcases ar with _ | rr
                 · simp [armFalsyTruthy] at hft
                 · cases rr <;> simp [armFalsyTruthy] at hft
      | bool => rcases ar with _ | rr
                · simp [armFalsyTruthy] at hft
                · cases rr <;> simp [armFalsyTruthy] at hft
      | int =>
        rcases ar with _ | rr
        · simp [armFalsyTruthy] at hft
        · cases rr with
          | str _ => simp [armFalsyTruthy] at hft
          | int q =>
            obtain ⟨j, rfl⟩ : ∃ j, v = Val.int j := by cases v <;> simp_all [Val.base]
            have hzero : j = 0 := by
              by_cases hz : j = 0
              · exact hz
              · simp [Val.falsy, hz] at hb
            subst hzero
            simp only [armFalsyTruthy] at hft
            simp only [refOptAdmits, refAdmits] at har
            simp only [IntRange.contains, Bool.and_eq_false_iff,
              decide_eq_false_iff_not] at hft
            simp only [IntRange.contains, Bool.and_eq_true, decide_eq_true_eq] at har
            omega
      | str =>
        rcases ar with _ | rr
        · simp [armFalsyTruthy] at hft
        · cases rr with
          | int _ => simp [armFalsyTruthy] at hft
          | str p =>
            obtain ⟨k, rfl⟩ : ∃ k, v = Val.str k := by cases v <;> simp_all [Val.base]
            simp only [armFalsyTruthy, Bool.not_eq_false'] at hft
            simp only [refOptAdmits, refAdmits] at har
            have hnf := nonFalsy_of_containsAll (StrPreds.containsAll_trans har hft)
            rw [M.nonFalsy_iff k] at hnf
            simp only [Val.falsy] at hb
            simp_all
    | shape s n =>
      -- A `yes` on the array stratum says the shape is known non-empty and
      -- non-nullable, and an array is falsy exactly when it is empty — the
      -- coupling `Model.arrFalsy_iff` records.
      have hcf := canFalsy_false_of_truthy_yes (M := M) (by simp [finiteMembers]) h
      simp only [abstractFalsyTruthy] at hcf
      obtain ⟨hne, hn⟩ := Bool.or_eq_false_iff.mp hcf
      have hne' : s.nonEmpty = true := by simpa using hne
      cases v with
      | null => rw [hn] at hv; simp [admits] at hv
      | arr r =>
        have hempty : (M.arrEntries r).isEmpty = true := by
          have hco := M.arrFalsy_iff r
          simp only [Val.falsy] at hb
          rw [hb] at hco
          exact hco.symm
        obtain ⟨fields, tail, isList, nonEmpty, covers⟩ := s
        simp only at hne'
        subst hne'
        simp only [admits] at hv
        unfold shapeAdmits at hv
        simp [hempty] at hv
      | bool _ => simp [admits] at hv
      | int _ => simp [admits] at hv
      | float _ => simp [admits] at hv
      | str _ => simp [admits] at hv

/-- **A `no` from `truthy` holds for every admitted value.** On the Refined layer
only `int<0, 0>` can produce it, which is exactly what `canTruthy` encodes. -/
theorem truthy_no (M : Model) {f : Fact} {v : Val}
    (h : f.truthy M = .no) (hv : f.admits M v = true) : v.falsy M = true := by
  cases hb : Val.falsy M v with
  | true => rfl
  | false =>
    exfalso
    cases f with
    | singleton w =>
      have hw : w = v := by simpa [admits] using hv
      rw [hw] at h
      simp [truthy, finiteMembers, Certainty.allOf, Certainty.ofBool, hb] at h
    | oneOf vs =>
      have hmem : v ∈ vs := by simpa [admits] using hv
      have hall := ((Certainty.allOf_eq_no_iff _).mp
        (by simpa [truthy, finiteMembers] using h)).2
      have hc := hall _ (List.mem_map_of_mem
        (f := fun w => Certainty.ofBool (!Val.falsy M w)) hmem)
      simp [Certainty.ofBool, hb] at hc
    | general b n => simp [truthy, finiteMembers, abstractFalsyTruthy] at h
    | refined b r n =>
      have hct := canTruthy_false_of_truthy_no (M := M) (by simp [finiteMembers]) h
      simp only [abstractFalsyTruthy] at hct
      cases b with
      | int =>
        cases r with
        | str p => simp only at hct; exact absurd hct (by simp)
        | int q =>
          simp only [decide_eq_false_iff_not] at hct
          have hq : q = IntRange.point 0 := by
            by_cases hq : q = IntRange.point 0
            · exact hq
            · exact absurd hq hct
          subst hq
          cases v with
          | int i =>
            have hne : i ≠ 0 := by
              intro hz; subst hz; simp [Val.falsy] at hb
            simp only [admits, refAdmits, Bool.and_eq_true] at hv
            obtain ⟨-, hv⟩ := hv
            simp only [IntRange.contains, IntRange.point, Bool.and_eq_true,
              decide_eq_true_eq] at hv
            omega
          | null => simp [Val.falsy] at hb
          | bool _ => simp [admits, refAdmits] at hv
          | float _ => simp [admits, refAdmits] at hv
          | str _ => simp [admits, refAdmits] at hv
          | arr _ => simp [admits, refAdmits] at hv
      | str => cases r <;> (simp only at hct; exact absurd hct (by simp))
      | float => cases r <;> (simp only at hct; exact absurd hct (by simp))
      | bool => cases r <;> (simp only at hct; exact absurd hct (by simp))
    | union arms n =>
      -- A `no` means no arm can be truthy, and the value sits in one of them.
      have hct := canTruthy_false_of_truthy_no (M := M) (by simp [finiteMembers]) h
      rw [union_canTruthy] at hct
      have hvn : v ≠ Val.null := by
        intro he; subst he; simp [Val.falsy] at hb
      rw [admits_union_eq hvn] at hv
      obtain ⟨a, hmem, ha⟩ := List.any_eq_true.mp hv
      have hft := List.any_eq_false.mp hct a hmem
      simp only [Bool.not_eq_true] at hft
      obtain ⟨ab, ar⟩ := a
      simp only [armAdmits, Bool.and_eq_true, decide_eq_true_eq] at ha
      obtain ⟨hab, har⟩ := ha
      cases ab with
      | float => rcases ar with _ | rr
                 · simp [armFalsyTruthy] at hft
                 · cases rr <;> simp [armFalsyTruthy] at hft
      | bool => rcases ar with _ | rr
                · simp [armFalsyTruthy] at hft
                · cases rr <;> simp [armFalsyTruthy] at hft
      | str => rcases ar with _ | rr
               · simp [armFalsyTruthy] at hft
               · cases rr <;> simp [armFalsyTruthy] at hft
      | int =>
        rcases ar with _ | rr
        · simp [armFalsyTruthy] at hft
        · cases rr with
          | str _ => simp [armFalsyTruthy] at hft
          | int q =>
            obtain ⟨j, rfl⟩ : ∃ j, v = Val.int j := by cases v <;> simp_all [Val.base]
            have hne : j ≠ 0 := by intro hz; subst hz; simp [Val.falsy] at hb
            simp only [armFalsyTruthy, decide_eq_false_iff_not] at hft
            have hq : q = IntRange.point 0 := by
              by_cases hq : q = IntRange.point 0
              · exact hq
              · exact absurd hq hft
            subst hq
            simp only [refOptAdmits, refAdmits] at har
            simp only [IntRange.contains, IntRange.point, Bool.and_eq_true,
              decide_eq_true_eq] at har
            omega
    | shape s n =>
      -- The array stratum over-approximates the truthy side, so it never
      -- answers `no` — there is nothing to discharge.
      have hct := canTruthy_false_of_truthy_no (M := M) (by simp [finiteMembers]) h
      simp only [abstractFalsyTruthy] at hct
      exact absurd hct (by simp)

/-! ## `satisfiesStr` -/

/-- **A `yes` from `satisfiesStr` holds for every admitted value**: every one is
a string whose own summary entails the queried predicates. -/
theorem satisfiesStr_yes (M : Model) {f : Fact} {v : Val} {pred : StrPreds}
    (h : f.satisfiesStr M pred = .yes) (hv : f.admits M v = true) :
    ∃ k, v = Val.str k ∧ (M.predsOf k).containsAll pred = true := by
  cases f with
  | singleton w =>
    have hw : w = v := by simpa [admits] using hv
    subst hw
    cases w with
    | str k => exact ⟨k, rfl, by simpa [satisfiesStr, Certainty.ofBool] using h⟩
    | null => simp [satisfiesStr] at h
    | bool _ => simp [satisfiesStr] at h
    | int _ => simp [satisfiesStr] at h
    | float _ => simp [satisfiesStr] at h
    | arr _ => simp [satisfiesStr] at h
  | oneOf vs =>
    have hmem : v ∈ vs := by simpa [admits] using hv
    have hall := ((Certainty.allOf_eq_yes_iff _).mp (by simpa [satisfiesStr] using h)).2
    have hc := hall _ (List.mem_map_of_mem (f := fun w =>
      match w with
      | Val.str k => Certainty.ofBool ((M.predsOf k).containsAll pred)
      | _ => Certainty.no) hmem)
    cases v with
    | str k => exact ⟨k, rfl, by simpa [Certainty.ofBool] using hc⟩
    | null => simp at hc
    | bool _ => simp at hc
    | int _ => simp at hc
    | float _ => simp at hc
    | arr _ => simp at hc
  | general b n => cases b <;> simp [satisfiesStr] at h
  | refined b r n =>
    cases b with
    | str =>
      cases r with
      | int q => simp [satisfiesStr] at h
      | str p =>
        simp only [satisfiesStr, ne_eq, not_true_eq_false, if_false] at h
        split at h
        · rename_i hp
          simp only [Bool.and_eq_true, Bool.not_eq_true'] at hp
          cases v with
          | str k =>
            simp only [admits, refAdmits, Bool.and_eq_true] at hv
            exact ⟨k, rfl, StrPreds.containsAll_trans hv.2 hp.1⟩
          | null => rw [hp.2] at hv; simp [admits] at hv
          | bool _ => simp [admits, refAdmits] at hv
          | int _ => simp [admits, refAdmits] at hv
          | float _ => simp [admits, refAdmits] at hv
          | arr _ => simp [admits, refAdmits] at hv
        · exact absurd h (by simp)
    | int => cases r <;> simp [satisfiesStr] at h
    | float => cases r <;> simp [satisfiesStr] at h
    | bool => cases r <;> simp [satisfiesStr] at h
  | union arms n =>
    -- `allOf` is `yes` only when *every* arm is a string arm whose predicate set
    -- entails `pred` — including the one the value sits in.
    simp only [satisfiesStr] at h
    have hyes := (Certainty.allOf_eq_yes_iff _).mp h
    have harm : ∀ a ∈ arms, a.1 = Base.str ∧ n = false ∧
        ∃ p, a.2 = some (.str p) ∧ p.containsAll pred = true := by
      intro a ha
      obtain ⟨ab, ar⟩ := a
      by_cases hs : ab = Base.str
      · subst hs
        rcases ar with _ | rr
        · have hy := hyes.2 _ (List.mem_map_of_mem ha)
          simp at hy
        · cases rr with
          | int _ =>
            have hy := hyes.2 _ (List.mem_map_of_mem ha)
            simp at hy
          | str p =>
            have hy := hyes.2 _ (List.mem_map_of_mem ha)
            simp only [ne_eq, not_true_eq_false, if_false] at hy
            split at hy
            · rename_i hp
              simp only [Bool.and_eq_true, Bool.not_eq_true'] at hp
              exact ⟨rfl, hp.2, p, rfl, hp.1⟩
            · exact absurd hy (by simp)
      · have hy := hyes.2 _ (List.mem_map_of_mem ha)
        simp [hs] at hy
    have hne : arms ≠ [] := by
      intro he
      rw [he] at hyes
      exact hyes.1 (by simp)
    obtain ⟨a0, hmem0⟩ : ∃ a0, a0 ∈ arms := by
      cases arms with
      | nil => exact absurd rfl hne
      | cons x xs => exact ⟨x, by simp⟩
    have hn : n = false := (harm a0 hmem0).2.1
    have hvn : v ≠ Val.null := by
      intro he; subst he; rw [hn] at hv; simp [admits] at hv
    rw [admits_union_eq hvn] at hv
    obtain ⟨a, hmem, ha⟩ := List.any_eq_true.mp hv
    obtain ⟨hs, -, p, hp, hcp⟩ := harm a hmem
    simp only [armAdmits, hs, hp, refOptAdmits, Bool.and_eq_true, decide_eq_true_eq] at ha
    obtain ⟨hab, har⟩ := ha
    obtain ⟨k, rfl⟩ : ∃ k, v = Val.str k := by
      cases v with
      | str k => exact ⟨k, rfl⟩
      | null => exact absurd rfl hvn
      | bool _ => simp [Val.base] at hab
      | int _ => simp [Val.base] at hab
      | float _ => simp [Val.base] at hab
      | arr _ => simp [Val.base] at hab
    simp only [refAdmits] at har
    exact ⟨k, rfl, StrPreds.containsAll_trans har hcp⟩
  | shape _ _ => simp [satisfiesStr] at h

/-! ## `intIn` -/

/-- **A `no` from `intIn` holds for every admitted value**: no admitted int lies
in the queried interval. -/
theorem intIn_no (M : Model) {f : Fact} {v : Val} {range : IntRange} {i : Int}
    (h : f.intIn range = .no) (hv : f.admits M v = true) (hi : v = Val.int i) :
    range.contains i = false := by
  subst hi
  cases f with
  | singleton w =>
    have hw : w = Val.int i := by simpa [admits] using hv
    subst hw
    simpa [intIn, Certainty.ofBool] using h
  | oneOf vs =>
    have hmem : Val.int i ∈ vs := by simpa [admits] using hv
    have hall := ((Certainty.allOf_eq_no_iff _).mp (by simpa [intIn] using h)).2
    have hc := hall _ (List.mem_map_of_mem (f := fun w =>
      match w with
      | Val.int j => Certainty.ofBool (range.contains j)
      | _ => Certainty.no) hmem)
    simpa [Certainty.ofBool] using hc
  | general b n =>
    cases b with
    | int => simp [intIn] at h
    | float => simp [admits, Val.base] at hv
    | str => simp [admits, Val.base] at hv
    | bool => simp [admits, Val.base] at hv
  | refined b r n =>
    cases b with
    | int =>
      cases r with
      | str p => simp [intIn] at h
      | int q =>
        simp only [intIn, ne_eq, not_true_eq_false, if_false] at h
        split at h
        · exact absurd h (by simp)
        · split at h
          · rename_i hdisj
            have hq : q.contains i = true := by
              simp only [admits, refAdmits, Bool.and_eq_true] at hv; exact hv.2
            cases hr : range.contains i with
            | false => rfl
            | true => exact absurd ⟨hq, hr⟩ (IntRange.inter_none_disjoint hdisj i)
          · exact absurd h (by simp)
    | float => simp [admits, Val.base] at hv
    | str => simp [admits, Val.base] at hv
    | bool => simp [admits, Val.base] at hv
  | union arms n =>
    -- `allOf` is `no` only when every arm is disjoint from `range`, including
    -- the int arm the value sits in.
    simp only [intIn] at h
    have hno := (Certainty.allOf_eq_no_iff _).mp h
    have hvn : Val.int i ≠ Val.null := by simp
    rw [admits_union_eq hvn] at hv
    obtain ⟨a, hmem, ha⟩ := List.any_eq_true.mp hv
    obtain ⟨ab, ar⟩ := a
    simp only [armAdmits, Bool.and_eq_true, decide_eq_true_eq] at ha
    obtain ⟨hab, har⟩ := ha
    have hbi : Base.int = ab := by simpa [Val.base] using hab
    subst hbi
    rcases ar with _ | rr
    · have hc := hno.2 _ (List.mem_map_of_mem hmem)
      simp at hc
    · cases rr with
      | str _ =>
        have hc := hno.2 _ (List.mem_map_of_mem hmem)
        simp at hc
      | int q =>
        have hc := hno.2 _ (List.mem_map_of_mem hmem)
        simp only [ne_eq, not_true_eq_false, if_false] at hc
        split at hc
        · exact absurd hc (by simp)
        · split at hc
          · rename_i hdisj
            have hq : q.contains i = true := by
              simpa [refOptAdmits, refAdmits] using har
            cases hr : range.contains i with
            | false => rfl
            | true => exact absurd ⟨hq, hr⟩ (IntRange.inter_none_disjoint hdisj i)
          · exact absurd hc (by simp)
  | shape _ _ => simp [admits] at hv

end Fact
end SteinsDomain
