/-!
# Integer intervals

Ports `crates/steins-domain/src/range.rs` (ADR-0035): the canonical int
refinement, an inclusive interval over PHP's 64-bit ints.

**Modelling note.** The spec carries `Int`, not a wrapping 64-bit integer. That
is faithful because the domain performs *no arithmetic* on interval bounds —
`contains`, `containsRange`, `hull`, and `inter` are order operations only, so
no wrap-around behaviour exists to model. The i64 endpoints appear solely as the
two constants `isFull` compares against, and the differential vectors pin the
Rust side's behaviour at exactly those endpoints.
-/

namespace SteinsDomain

/-- `i64::MIN` — the lower domain bound of PHP `int`. -/
def i64Min : Int := -9223372036854775808
/-- `i64::MAX` — the upper domain bound of PHP `int`. -/
def i64Max : Int := 9223372036854775807

/-- An inclusive integer interval. `Valid` is the Rust type's `lo <= hi`
invariant, which `new` enforces and every operation preserves. -/
structure IntRange where
  lo : Int
  hi : Int
  deriving DecidableEq, Repr, Inhabited

namespace IntRange

/-- The `lo ≤ hi` invariant. Rust makes this unconstructible-otherwise via
`new`; here it is a side condition carried by the lemmas that need it. -/
def Valid (r : IntRange) : Prop := r.lo ≤ r.hi

instance (r : IntRange) : Decidable r.Valid := inferInstanceAs (Decidable (_ ≤ _))

/-- Construct an interval; `none` when `lo > hi` (empty — the domain has no
empty fact). -/
def new (lo hi : Int) : Option IntRange := if lo ≤ hi then some ⟨lo, hi⟩ else none

/-- The single-point interval. -/
def point (v : Int) : IntRange := ⟨v, v⟩

/-- The whole `int` domain. -/
def full : IntRange := ⟨i64Min, i64Max⟩

/-- Whether this is the whole `int` domain (i.e. no knowledge). -/
def isFull (r : IntRange) : Bool := r.lo == i64Min && r.hi == i64Max

/-- Membership. -/
def contains (r : IntRange) (n : Int) : Bool := r.lo ≤ n && n ≤ r.hi

/-- Whether `r` contains every point of `s`. -/
def containsRange (r s : IntRange) : Bool := r.lo ≤ s.lo && s.hi ≤ r.hi

/-- Convex hull — the join for value-set union. May over-approximate a union
with gaps; that is the measured widening. -/
def hull (r s : IntRange) : IntRange := ⟨min r.lo s.lo, max r.hi s.hi⟩

/-- Intersection — the meet; `none` when disjoint. -/
def inter (r s : IntRange) : Option IntRange := new (max r.lo s.lo) (min r.hi s.hi)

/-! ## Hull laws -/

theorem point_valid (v : Int) : (point v).Valid := by simp [Valid, point]

theorem contains_point (v : Int) : (point v).contains v = true := by
  simp [contains, point]

theorem hull_valid {r s : IntRange} (hr : r.Valid) : (r.hull s).Valid := by
  simp only [Valid, hull] at *
  omega

/-- The soundness direction: a hull never loses a member. -/
theorem hull_contains_left {r s : IntRange} {n : Int} (h : r.contains n = true) :
    (r.hull s).contains n = true := by
  simp only [contains, hull, Bool.and_eq_true, decide_eq_true_eq] at *
  omega

theorem hull_contains_right {r s : IntRange} {n : Int} (h : s.contains n = true) :
    (r.hull s).contains n = true := by
  simp only [contains, hull, Bool.and_eq_true, decide_eq_true_eq] at *
  omega

theorem hull_containsRange_left (r s : IntRange) : (r.hull s).containsRange r = true := by
  simp only [containsRange, hull, Bool.and_eq_true, decide_eq_true_eq]
  omega

theorem hull_containsRange_right (r s : IntRange) : (r.hull s).containsRange s = true := by
  simp only [containsRange, hull, Bool.and_eq_true, decide_eq_true_eq]
  omega

theorem hull_comm (r s : IntRange) : r.hull s = s.hull r := by
  simp only [hull, IntRange.mk.injEq]
  omega

theorem hull_assoc (r s t : IntRange) : (r.hull s).hull t = r.hull (s.hull t) := by
  simp only [hull, IntRange.mk.injEq]
  omega

theorem hull_self (r : IntRange) : r.hull r = r := by
  obtain ⟨lo, hi⟩ := r
  simp only [hull, IntRange.mk.injEq]
  omega

/-- Hull is monotone in both arguments — what `summarize`'s fold needs. -/
theorem hull_mono {r s r' s' : IntRange}
    (h₁ : r.containsRange r' = true) (h₂ : s.containsRange s' = true) :
    (r.hull s).containsRange (r'.hull s') = true := by
  simp only [containsRange, hull, Bool.and_eq_true, decide_eq_true_eq] at *
  omega

theorem containsRange_trans {r s t : IntRange}
    (h₁ : r.containsRange s = true) (h₂ : s.containsRange t = true) :
    r.containsRange t = true := by
  simp only [containsRange, Bool.and_eq_true, decide_eq_true_eq] at *
  omega

theorem contains_of_containsRange {r s : IntRange} {n : Int}
    (h : r.containsRange s = true) (hn : s.contains n = true) : r.contains n = true := by
  simp only [containsRange, contains, Bool.and_eq_true, decide_eq_true_eq] at *
  omega

/-! ## Intersection laws -/

theorem inter_containsRange_left {r s t : IntRange} (h : r.inter s = some t) :
    r.containsRange t = true := by
  simp only [inter, new] at h
  split at h
  · injection h with h; subst h
    simp only [containsRange, Bool.and_eq_true, decide_eq_true_eq]
    omega
  · exact absurd h (by simp)

theorem inter_containsRange_right {r s t : IntRange} (h : r.inter s = some t) :
    s.containsRange t = true := by
  simp only [inter, new] at h
  split at h
  · injection h with h; subst h
    simp only [containsRange, Bool.and_eq_true, decide_eq_true_eq]
    omega
  · exact absurd h (by simp)

/-- A `none` intersection really is disjointness: no point lies in both. -/
theorem inter_none_disjoint {r s : IntRange} (h : r.inter s = none) (n : Int) :
    ¬(r.contains n = true ∧ s.contains n = true) := by
  simp only [inter, new] at h
  split at h
  · exact absurd h (by simp)
  · rename_i hlt
    simp only [contains, Bool.and_eq_true, decide_eq_true_eq]
    omega

/-! ## The full range -/

theorem full_contains {n : Int} (hlo : i64Min ≤ n) (hhi : n ≤ i64Max) :
    full.contains n = true := by
  simp only [contains, full, Bool.and_eq_true, decide_eq_true_eq]
  exact ⟨hlo, hhi⟩

theorem isFull_iff (r : IntRange) : r.isFull = true ↔ r = full := by
  obtain ⟨lo, hi⟩ := r
  simp only [isFull, full, Bool.and_eq_true, beq_iff_eq, IntRange.mk.injEq]

end IntRange
end SteinsDomain
