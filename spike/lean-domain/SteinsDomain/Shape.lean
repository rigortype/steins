import SteinsDomain.Val
import SteinsDomain.Certainty
import SteinsDomain.Range

/-!
# The canonical abstract array fact

Ports `crates/steins-domain/src/shape.rs` (ADR-0062 §3 and Amendment A) clause
for clause: the shape form, its canonical constructor `normalize`, the
denotational `is_list`, and the cover algebra.

**Why `Fact` is declared in this file.** `Fact` gains a `shape` constructor and
`GShape` carries `Option Fact` value slots, so the two are one recursive
declaration and Lean requires them in one place. The value slots use real
`List`/`Option`/`Prod`, which makes this a *nested* (not mutual) inductive —
faithful to Rust's `Vec<(Key, Presence, Option<Box<Fact>>)>` and usable with the
ordinary `List` API. The price is that `deriving DecidableEq` has no handler for
nested inductives, so `Fact.beq` below is written out and `BEq Fact` is built
from it; nothing in the spec needs propositional decidability of fact equality.

Everything here is *structural*: no definition in this file inspects a slot
fact, so none of it is recursive with the algebra in `SteinsDomain.Fact`.
-/

namespace SteinsDomain

/-! ## Presence, key classes, covers -/

/-- Whether a key is in the array, and on what evidence. `witnessed` is the
presence stratum (ADR-0062 §3): a guard that really executed, rather than a
docblock. It is provenance, never extension — `admits` does not read it. -/
inductive Presence where
  | required (witnessed : Bool)
  | optional
  | absent
  deriving DecidableEq, Repr, Inhabited

namespace Presence

def isRequired : Presence → Bool
  | .required _ => true
  | _ => false

/-- Presence join (A-G5). -/
def join : Presence → Presence → Presence
  | .required a, .required b => .required (a && b)
  | .absent, .absent => .absent
  | _, _ => .optional

theorem join_comm (a b : Presence) : join a b = join b a := by
  cases a <;> cases b <;> simp [join, Bool.and_comm]

theorem join_idem (a : Presence) : join a a = a := by
  cases a <;> simp [join]

/-- Required survives a join only when both sides require. -/
theorem required_of_join {a b : Presence} (h : (join a b).isRequired = true) :
    a.isRequired = true ∧ b.isRequired = true := by
  cases a <;> cases b <;> simp_all [join, isRequired]

end Presence

/-- The key contract of an unsealed tail; `arrayKey` is PHP's `int|string`. -/
inductive KeyClass where
  | int
  | str
  | arrayKey
  deriving DecidableEq, Repr, Inhabited

namespace KeyClass

def ofKey : Key → KeyClass
  | .int _ => .int
  | .str _ => .str

def admitsKey : KeyClass → Key → Bool
  | .arrayKey, _ => true
  | .int, .int _ => true
  | .str, .str _ => true
  | _, _ => false

def admitsInt : KeyClass → Bool
  | .int => true
  | .arrayKey => true
  | .str => false

def join (a b : KeyClass) : KeyClass := if a = b then a else .arrayKey

theorem join_comm (a b : KeyClass) : join a b = join b a := by
  cases a <;> cases b <;> rfl

/-- The join admits every key either operand admits. -/
theorem join_admits {a b : KeyClass} {k : Key} (h : a.admitsKey k = true ∨ b.admitsKey k = true) :
    (join a b).admitsKey k = true := by
  cases a <;> cases b <;> cases k <;> simp_all [join, admitsKey]

end KeyClass

/-- Which disjunction produced a cover (A-G8). -/
inductive CoverFlavor where
  /-- `isset(…) || isset(…)`: at least one key present **and non-null**. -/
  | isset
  /-- `array_key_exists` disjunctions: at least one key present. -/
  | keyExists
  deriving DecidableEq, Repr, Inhabited

/-- A disjunctive-presence fact. Canonical covers have sorted, deduped keys with
at least two members. -/
structure Cover where
  keys : List Key
  flavor : CoverFlavor
  deriving DecidableEq, Repr, Inhabited

namespace Cover

/-- Does this cover's claim entail `other`'s? Fewer keys is a stronger claim,
and `isset` (present non-null) entails `keyExists` (present). -/
def subsumes (a b : Cover) : Bool :=
  (a.flavor == b.flavor || a.flavor == CoverFlavor.isset)
    && a.keys.all (fun k => b.keys.contains k)

theorem subsumes_refl (c : Cover) : c.subsumes c = true := by
  simp only [subsumes, beq_self_eq_true, Bool.true_or, Bool.true_and]
  exact List.all_eq_true.mpr (fun k hk => by simpa using hk)

def flavorRank : CoverFlavor → Nat
  | .isset => 0
  | .keyExists => 1

/-- Lexicographic order on the key list, then the flavor — Rust's derived `Ord`
on `Cover { keys, flavor }`. -/
def keysBlt : List Key → List Key → Bool
  | [], [] => false
  | [], _ :: _ => true
  | _ :: _, [] => false
  | a :: as, b :: bs => if a = b then keysBlt as bs else a.blt b

def blt (a b : Cover) : Bool :=
  if a.keys = b.keys then flavorRank a.flavor < flavorRank b.flavor else keysBlt a.keys b.keys

end Cover

/-! ## The fact form -/

/-- A refinement on a scalar base (layer 3's content). Declared here because
`Fact` is (see the module docstring). -/
inductive Refinement where
  | str (p : StrPreds)
  | int (r : IntRange)
  deriving DecidableEq, Repr, Inhabited

/-- What the shape says about keys it does not declare. Parameterized over the
slot type so it can be declared before `Fact` and nested inside it. -/
inductive GTail (α : Type) where
  /-- No undeclared key may be present. -/
  | sealed
  /-- Undeclared keys matching `key` and `slot` are admitted. -/
  | unsealed (key : KeyClass) (slot : Option α)
  deriving Repr, Inhabited

/-- The canonical abstract array fact, parameterized over the slot type. -/
structure GShape (α : Type) where
  fields : List (Key × Presence × Option α)
  tail : GTail α
  isList : Certainty
  nonEmpty : Bool
  covers : List Cover
  deriving Repr, Inhabited

/-- What is known about a single value, in one of the four scalar layers or the
abstract array stratum. -/
inductive Fact where
  /-- Layer 1: exactly this value. -/
  | singleton (v : Val)
  /-- Layer 2: one of these values (canonical, `2..=CAP`). -/
  | oneOf (vs : List Val)
  /-- Layer 3: a scalar base plus a refinement; `nullable` adds `null`. -/
  | refined (base : Base) (r : Refinement) (nullable : Bool)
  /-- Layer 4: just a scalar base (plus optionally `null`). -/
  | general (base : Base) (nullable : Bool)
  /-- Layer 3½: an abstract union across bases (issue #339). One arm per
  `Base`, sorted, at least two of them; `nullable` carries `null` for the whole
  union. The array stratum is not an arm. -/
  | union (arms : List (Base × Option Refinement)) (nullable : Bool)
  /-- The abstract array stratum (A-G2). There is no array-`general`: the
  degenerate shape *is* plain `array`. -/
  | shape (s : GShape Fact) (nullable : Bool)
  deriving Repr, Inhabited

/-- The shape form at the fact slot type. -/
abbrev ShapeFact := GShape Fact

/-- One field of a shape. -/
abbrev Field := Key × Presence × Option Fact

/-- A value slot: `none` is the unknown floor, which admits anything. -/
abbrev Slot := Option Fact

/-! ### Equality

`deriving DecidableEq` has no handler for a nested inductive, so the Boolean
equality is written out. Only the differential vector machinery needs it. -/

mutual

def Fact.beq (a b : Fact) : Bool :=
  match a, b with
  | .singleton x, .singleton y => decide (x = y)
  | .oneOf x, .oneOf y => decide (x = y)
  | .refined b₁ r₁ n₁, .refined b₂ r₂ n₂ =>
    decide (b₁ = b₂) && decide (r₁ = r₂) && decide (n₁ = n₂)
  | .general b₁ n₁, .general b₂ n₂ => decide (b₁ = b₂) && decide (n₁ = n₂)
  | .union a₁ n₁, .union a₂ n₂ => decide (a₁ = a₂) && decide (n₁ = n₂)
  | .shape s₁ n₁, .shape s₂ n₂ => Fact.shapeBeq s₁ s₂ && decide (n₁ = n₂)
  | _, _ => false
termination_by sizeOf a

def Fact.shapeBeq (a b : ShapeFact) : Bool :=
  match a, b with
  | ⟨f₁, t₁, l₁, e₁, c₁⟩, ⟨f₂, t₂, l₂, e₂, c₂⟩ =>
    Fact.fieldsBeq f₁ f₂ && Fact.tailBeq t₁ t₂ && decide (l₁ = l₂) && decide (e₁ = e₂)
      && decide (c₁ = c₂)
termination_by sizeOf a

def Fact.fieldsBeq (a b : List Field) : Bool :=
  match a, b with
  | [], [] => true
  | (k₁, p₁, s₁) :: r₁, (k₂, p₂, s₂) :: r₂ =>
    decide (k₁ = k₂) && decide (p₁ = p₂) && Fact.slotBeq s₁ s₂ && Fact.fieldsBeq r₁ r₂
  | _, _ => false
termination_by sizeOf a

def Fact.tailBeq (a b : GTail Fact) : Bool :=
  match a, b with
  | .sealed, .sealed => true
  | .unsealed k₁ s₁, .unsealed k₂ s₂ => decide (k₁ = k₂) && Fact.slotBeq s₁ s₂
  | _, _ => false
termination_by sizeOf a

def Fact.slotBeq (a b : Slot) : Bool :=
  match a, b with
  | none, none => true
  | some x, some y => Fact.beq x y
  | _, _ => false
termination_by sizeOf a

end

instance : BEq Fact := ⟨Fact.beq⟩

/-- Certainty that the value is `null`.

Defined here rather than with the other trinary queries because
`GShape.keyImplies` needs it: an `isset` cover is only discharged by a required
key whose slot proves the value non-null. -/
def Fact.isNull : Fact → Certainty
  | .singleton v => Certainty.ofBool (decide (v = Val.null))
  | .oneOf vs => Certainty.allOf (vs.map (fun v => Certainty.ofBool (decide (v = Val.null))))
  | .refined _ _ nullable => if nullable then .maybe else .no
  | .general _ nullable => if nullable then .maybe else .no
  | .union _ nullable => if nullable then .maybe else .no
  | .shape _ nullable => if nullable then .maybe else .no

/-! ## `array_is_list`

PHP's predicate: the keys are exactly `0..n-1`, in that order. Sound only on the
value lane, which is order-witnessed (ADR-0062 §2). -/

def arrayIsListFrom (i : Int) : List (Key × Val) → Bool
  | [] => true
  | (.int n, _) :: rest => decide (n = i) && arrayIsListFrom (i + 1) rest
  | (.str _, _) :: _ => false

def arrayIsList (entries : List (Key × Val)) : Bool := arrayIsListFrom 0 entries

/-! ## Field lists -/

def fieldOf (fields : List Field) (k : Key) : Option Field :=
  fields.find? (fun f => decide (f.1 = k))

/-- Sorted-and-deduped insert on the key, **keeping the entry already in the
accumulator** on a collision. Folded left over the original list, this is Rust's
stable `sort_by(key)` followed by `dedup_by(key)`: the earliest occurrence of a
key wins. -/
def insertField (f : Field) : List Field → List Field
  | [] => [f]
  | g :: rest =>
    if f.1 = g.1 then g :: rest
    else if f.1.blt g.1 then f :: g :: rest
    else g :: insertField f rest

def sortFields (fs : List Field) : List Field := fs.foldl (fun acc f => insertField f acc) []

def insertKey (k : Key) : List Key → List Key
  | [] => [k]
  | x :: xs => if k = x then x :: xs else if k.blt x then k :: x :: xs else x :: insertKey k xs

/-- Sorted, deduped key list — Rust's `sort(); dedup()`. -/
def canonKeys (ks : List Key) : List Key := ks.foldl (fun acc k => insertKey k acc) []

def insertCover (c : Cover) : List Cover → List Cover
  | [] => [c]
  | x :: xs => if c = x then x :: xs else if c.blt x then c :: x :: xs else x :: insertCover c xs

def canonCovers (cs : List Cover) : List Cover := cs.foldl (fun acc c => insertCover c acc) []

/-- Keep only the minimal covers, deterministically ordered. -/
def antichain (cs : List Cover) : List Cover :=
  let sorted := canonCovers cs
  sorted.filter (fun c => !sorted.any (fun o => !(o == c) && o.subsumes c))

/-! ## Denotational `is_list` (ADR-0062 §3, RFC #14939) -/

def presentKeys (fields : List Field) : List Key :=
  (fields.filter (fun f => !(f.2.1 == Presence.absent))).map (·.1)

def requiredKeys (fields : List Field) : List Key :=
  (fields.filter (fun f => f.2.1.isRequired)).map (·.1)

def isSealed : GTail Fact → Bool
  | .sealed => true
  | .unsealed _ _ => false

def tailSuppliesInt : GTail Fact → Bool
  | .sealed => false
  | .unsealed k _ => k.admitsInt

/-- Is key `i` available from a declared, not-proven-absent field? -/
def keyAvailable (fields : List Field) (i : Int) : Bool :=
  fields.any (fun f => f.1 == Key.int i && !(f.2.1 == Presence.absent))

def maxInt : List Key → Int
  | [] => -1
  | .int n :: rest => max n (maxInt rest)
  | .str _ :: rest => maxInt rest

/-- Is *some* admitted value a list? The witness is the minimal key set holding
every required key and filling the gaps below the largest of them. -/
def listIsAdmitted (fields : List Field) (tail : GTail Fact) (nonEmpty : Bool) : Bool :=
  let req := requiredKeys fields
  if req.any (fun k => match k with | .str _ => true | .int n => decide (n < 0)) then false
  else
    let m := maxInt req
    if m < 0 then !nonEmpty || tailSuppliesInt tail || keyAvailable fields 0
    else if tailSuppliesInt tail then true
    else
      -- Filling `0..=m` needs at least that many declared keys; the cheap bound
      -- keeps a sparse required key from walking a million candidates.
      let span := m.toNat
      if span + 1 > fields.length then false
      else (List.range (span + 1)).all (fun i => keyAvailable fields (i : Int))

/-- `yes` iff *every* admitted value is a list, `no` iff none is, `maybe`
otherwise. Because `array{…}` is an order-agnostic key set, a shape whose
realizable key sets can hold two members admits both insertion orders, so
`array{0: T, 1: U}` is `maybe` and not `yes`. -/
def computeIsList (fields : List Field) (tail : GTail Fact) (nonEmpty : Bool) : Certainty :=
  let present := presentKeys fields
  if isSealed tail && (present.isEmpty || present == [Key.int 0]) then .yes
  else if listIsAdmitted fields tail nonEmpty then .maybe else .no

/-- A caller may know `is_list` more precisely than the key structure does; it
may never contradict it, and on a contradiction the computed value wins. -/
def sharpenIsList (computed given : Certainty) : Certainty :=
  match computed with
  | .maybe => given
  | c => c

theorem sharpenIsList_ne_maybe {computed given : Certainty} (h : computed ≠ .maybe) :
    sharpenIsList computed given = computed := by
  cases computed <;> simp_all [sharpenIsList]

/-! ## The canonical constructor -/

/-- A singleton cover is not a disjunction — it *is* presence. A proven-absent
key (or one a sealed tail forbids) contradicts the cover; dropping the cover
widens, which is the safe side. -/
def promotePresent (fields : List Field) (k : Key) (tail : GTail Fact) : List Field :=
  match fieldOf fields k with
  | some (_, .absent, _) => fields
  | some _ => fields.map (fun f => if f.1 = k then (f.1, Presence.required true, f.2.2) else f)
  | none => if isSealed tail then fields else fields ++ [(k, Presence.required true, none)]

/-- Split the raw covers into presence promotions and the covers that survive. -/
def foldCovers (tail : GTail Fact) : List Field × List Cover → Cover → List Field × List Cover :=
  fun acc c =>
    match canonKeys c.keys with
    | [] => acc
    | [k] => (promotePresent acc.1 k tail, acc.2)
    | k₁ :: k₂ :: ks => (acc.1, acc.2 ++ [{ keys := k₁ :: k₂ :: ks, flavor := c.flavor }])

/-- **The canonical constructor.** Every invariant the shape form carries is
established here. -/
def normalize (fields : List Field) (tail : GTail Fact) (isList : Certainty)
    (nonEmpty : Bool) (covers : List Cover) : ShapeFact :=
  let f₀ := sortFields fields
  let step := covers.foldl (foldCovers tail) (f₀, [])
  let f₁ := sortFields step.1
  -- A sealed tail already proves absence, so an `absent` field is redundant.
  let f₂ := if isSealed tail then f₁.filter (fun f => !(f.2.1 == Presence.absent)) else f₁
  -- A cover with a required member is discharged.
  let kept := step.2.filter (fun c => !c.keys.any (fun k =>
    match fieldOf f₂ k with
    | some (_, p, _) => p.isRequired
    | none => false))
  { fields := f₂
    tail := tail
    isList := sharpenIsList (computeIsList f₂ tail nonEmpty) isList
    nonEmpty := nonEmpty || f₂.any (fun f => f.2.1.isRequired)
    covers := antichain kept }

/-- The degenerate shape: plain `array` (A-G1). -/
def plainArray : ShapeFact :=
  normalize [] (.unsealed .arrayKey none) .maybe false []

/-! ## Shape-level queries that do not read a slot fact recursively -/

namespace GShape

def field (s : ShapeFact) (k : Key) : Option Field := fieldOf s.fields k

/-- Does this shape admit some non-empty array? Over-approximated on the
permissive side, which pushes `truthy` toward `maybe`. -/
def canBeNonEmpty (s : ShapeFact) : Bool :=
  !isSealed s.tail || s.fields.any (fun f => !(f.2.1 == Presence.absent))

/-- Does a required key discharge a cover of this flavor? A `keyExists` cover
needs only presence; an `isset` cover needs the slot to prove the value
non-null, because a required key whose value may be null does not make `isset`
true. -/
def keyImplies (s : ShapeFact) (k : Key) (flavor : CoverFlavor) : Bool :=
  match s.field k with
  | some (_, p, slot) =>
    p.isRequired && (match flavor with
      | .keyExists => true
      | .isset => match slot with
        | none => false
        | some f => decide (f.isNull = Certainty.no))
  | none => false

/-- Does this shape already establish `c` (A-G8)? -/
def impliesCover (s : ShapeFact) (c : Cover) : Bool :=
  s.covers.any (fun own => own.subsumes c) || c.keys.any (fun k => s.keyImplies k c.flavor)

/-- **The entry count every admitted array can have** (ADR-0062 §4's `count($x)`
row), as an inclusive interval.

* **Floor** — one entry per required field, floored at 1 when `nonEmpty` (or a
  cover) says the array cannot be empty.
* **Ceiling** — a sealed tail bounds the array by its declared, not-absent key
  set (the one place PHPStan has an exact size, mirrored); an unsealed tail
  admits arbitrarily many undeclared keys, so the ceiling is the domain's top.

`lo = hi` is exactly the exact-size case. **Modelling note**: Rust writes
`i64::try_from(len).unwrap_or(i64::MAX)` on the two counts, unreachable for any
list a machine can hold; the spec carries `Int` and needs no such guard, and the
`getD` fallback mirrors Rust's `unwrap_or(NON_NEGATIVE)` — which the comment
there calls a totality fallback rather than a reachable branch. -/
def countRange (s : ShapeFact) : IntRange :=
  let required : Int := (s.fields.filter (fun f => f.2.1.isRequired)).length
  let declared : Int := (s.fields.filter (fun f => !(f.2.1 == Presence.absent))).length
  let floorOne : Int := if s.nonEmpty || !s.covers.isEmpty then 1 else 0
  let lo : Int := max required floorOne
  let hi : Int := if isSealed s.tail then declared else i64Max
  (IntRange.new lo hi).getD ⟨0, i64Max⟩

/-- The count interval is always a well-formed interval — the `getD` fallback is
what makes the constructor total rather than trusting `required ⊆ declared`. -/
theorem countRange_valid (s : ShapeFact) : (countRange s).Valid :=
  IntRange.new_getD_valid _ _ (by decide)

/-- **Cover recording** (A-G8, S5): the true branch of a disjunction of depth-1
constant-key presence tests over one binding. The claim recorded is exactly what
the disjunction being true says — "at least one of `keys` satisfies `flavor`" —
and every canonicalization is `normalize`'s, so the invariants hold by
construction: a singleton input is promoted to presence rather than stored, a
cover with a required member drops as discharged, and the survivors stay an
antichain.

`nonEmpty` follows from the claim: a cover says at least one of its keys is
present, so no admitted array is empty. A singleton input reaches the same
conclusion through the promotion branch, which is why only the ≥ 2 case sets the
flag here. -/
def recordCover (s : ShapeFact) (keys : List Key) (flavor : CoverFlavor) : ShapeFact :=
  normalize s.fields s.tail s.isList (s.nonEmpty || 1 < keys.length)
    (s.covers ++ [{ keys := keys, flavor := flavor }])

/-- **Cover discharge** (A-G11): does some cover prove `key` present, given that
every *other* member of that cover is in `absentKeys`?

The returned flavor is the *claim*, not the verdict: an `isset` cover discharges
unconditionally, a `keyExists` cover only when the caller has checked that every
absent key's value slot is provably non-nullable (a present-null member satisfies
the cover while `??` still falls through it). `isset` wins when both are
available — it is the stronger claim, and the fold below mirrors Rust's `.min()`
over the derived flavor order. -/
def coverProves (s : ShapeFact) (key : Key) (absentKeys : List Key) : Option CoverFlavor :=
  let usable := s.covers.filter (fun c =>
    c.keys.contains key && c.keys.all (fun k => k = key || absentKeys.contains k))
  usable.foldl (fun acc c =>
    match acc with
    | none => some c.flavor
    | some f => some (if Cover.flavorRank c.flavor < Cover.flavorRank f then c.flavor else f))
    none

end GShape

/-! ## Normalization invariants

The properties `normalize` establishes, as theorems rather than comments. -/

/-- Strictly increasing on the key — sortedness and one-entry-per-key in one
predicate. -/
def FieldsSorted : List Field → Prop
  | [] => True
  | [_] => True
  | a :: b :: l => a.1.blt b.1 = true ∧ FieldsSorted (b :: l)

theorem fieldsSorted_tail {a : Field} {l : List Field} (h : FieldsSorted (a :: l)) :
    FieldsSorted l := by
  cases l with
  | nil => trivial
  | cons b l => exact h.2

theorem fieldsSorted_cons {a : Field} {l : List Field} (h : FieldsSorted l)
    (hlt : ∀ f ∈ l, a.1.blt f.1 = true) : FieldsSorted (a :: l) := by
  cases l with
  | nil => trivial
  | cons b l => exact ⟨hlt b (by simp), h⟩

theorem head_blt_of_mem : ∀ {a : Field} {l : List Field}, FieldsSorted (a :: l) →
    ∀ {f : Field}, f ∈ l → a.1.blt f.1 = true := by
  intro a l
  induction l generalizing a with
  | nil => intro _ f hf; exact absurd hf (by simp)
  | cons x l ih =>
    intro hs f hf
    obtain ⟨hax, hxs⟩ := hs
    rcases List.mem_cons.mp hf with rfl | hf
    · exact hax
    · exact Key.blt_trans hax (ih hxs hf)

@[simp] theorem mem_insertField (f g : Field) (l : List Field) :
    g ∈ insertField f l → g = f ∨ g ∈ l := by
  induction l with
  | nil => intro h; simp only [insertField, List.mem_singleton] at h; exact Or.inl h
  | cons x xs ih =>
    intro h
    unfold insertField at h
    split at h
    · exact Or.inr h
    · split at h
      · rcases List.mem_cons.mp h with rfl | h
        · exact Or.inl rfl
        · exact Or.inr h
      · rcases List.mem_cons.mp h with rfl | h
        · exact Or.inr (by simp)
        · rcases ih h with rfl | h
          · exact Or.inl rfl
          · exact Or.inr (List.mem_cons_of_mem _ h)

theorem fieldsSorted_insertField (f : Field) : ∀ {l : List Field}, FieldsSorted l →
    FieldsSorted (insertField f l) := by
  intro l
  induction l with
  | nil => intro _; trivial
  | cons x xs ih =>
    intro hs
    unfold insertField
    split
    · exact hs
    · rename_i hne
      split
      · rename_i hlt; exact ⟨hlt, hs⟩
      · rename_i hnlt
        refine fieldsSorted_cons (ih (fieldsSorted_tail hs)) ?_
        intro g hg
        rcases mem_insertField f g xs hg with rfl | hg
        · rcases Key.blt_total hne with h | h
          · exact absurd h (by simp [hnlt])
          · exact h
        · exact head_blt_of_mem hs hg

/-- **`sortFields` is canonical**: the result is strictly increasing on the key,
so a shape has one field order and at most one entry per key. -/
theorem fieldsSorted_sortFields (fs : List Field) : FieldsSorted (sortFields fs) := by
  unfold sortFields
  suffices h : ∀ (l : List Field) (acc : List Field), FieldsSorted acc →
      FieldsSorted (l.foldl (fun acc f => insertField f acc) acc) by
    exact h fs [] trivial
  intro l
  induction l with
  | nil => intro acc h; exact h
  | cons x xs ih => intro acc h; exact ih _ (fieldsSorted_insertField x h)

theorem fieldsSorted_filter {p : Field → Bool} : ∀ {l : List Field}, FieldsSorted l →
    FieldsSorted (l.filter p) := by
  intro l
  induction l with
  | nil => intro _; trivial
  | cons x xs ih =>
    intro hs
    rw [List.filter_cons]
    split
    · refine fieldsSorted_cons (ih (fieldsSorted_tail hs)) ?_
      intro f hf
      exact head_blt_of_mem hs (List.mem_filter.mp hf).1
    · exact ih (fieldsSorted_tail hs)

/-- **The fields of a normalized shape are canonical.** -/
theorem normalize_fieldsSorted (fields : List Field) (tail : GTail Fact) (isList : Certainty)
    (nonEmpty : Bool) (covers : List Cover) :
    FieldsSorted (normalize fields tail isList nonEmpty covers).fields := by
  unfold normalize
  simp only
  split
  · exact fieldsSorted_filter (fieldsSorted_sortFields _)
  · exact fieldsSorted_sortFields _

/-- **A sealed shape carries no proven-absent field**: sealing already proves
absence, so the two would be one claim twice. -/
theorem normalize_sealed_no_absent (fields : List Field) (isList : Certainty)
    (nonEmpty : Bool) (covers : List Cover) :
    ∀ f ∈ (normalize fields .sealed isList nonEmpty covers).fields, f.2.1 ≠ Presence.absent := by
  intro f hf
  unfold normalize at hf
  simp only [isSealed, if_true] at hf
  have := (List.mem_filter.mp hf).2
  simpa using this

theorem mem_insertCover {c d : Cover} {l : List Cover} (h : d ∈ insertCover c l) :
    d = c ∨ d ∈ l := by
  induction l with
  | nil => simp only [insertCover, List.mem_singleton] at h; exact Or.inl h
  | cons x xs ih =>
    unfold insertCover at h
    split at h
    · exact Or.inr h
    · split at h
      · rcases List.mem_cons.mp h with rfl | h
        · exact Or.inl rfl
        · exact Or.inr h
      · rcases List.mem_cons.mp h with rfl | h
        · exact Or.inr (by simp)
        · rcases ih h with rfl | h
          · exact Or.inl rfl
          · exact Or.inr (List.mem_cons_of_mem _ h)

theorem mem_canonCovers {c : Cover} {cs : List Cover} (h : c ∈ canonCovers cs) : c ∈ cs := by
  unfold canonCovers at h
  suffices hg : ∀ (l acc : List Cover), c ∈ l.foldl (fun acc c => insertCover c acc) acc →
      c ∈ l ∨ c ∈ acc by
    rcases hg cs [] h with h | h
    · exact h
    · exact absurd h (by simp)
  intro l
  induction l with
  | nil => intro acc h; exact Or.inr h
  | cons x xs ih =>
    intro acc h
    rcases ih _ h with h | h
    · exact Or.inl (List.mem_cons_of_mem _ h)
    · rcases mem_insertCover h with rfl | h
      · exact Or.inl (by simp)
      · exact Or.inr h

/-- Every cover surviving `foldCovers` has at least two keys — the branch that
keeps one is the *promotion* branch, which produces presence, not a cover. -/
theorem foldCovers_two_keys (tail : GTail Fact) : ∀ (cs : List Cover) (acc : List Field × List Cover),
    (∀ d ∈ acc.2, 2 ≤ d.keys.length) →
    ∀ d ∈ (cs.foldl (foldCovers tail) acc).2, 2 ≤ d.keys.length := by
  intro cs
  induction cs with
  | nil => intro acc h d hd; exact h d hd
  | cons x xs ih =>
    intro acc h d hd
    refine ih _ ?_ d hd
    intro e he
    unfold foldCovers at he
    split at he
    · exact h e he
    · exact h e he
    · rcases List.mem_append.mp he with he | he
      · exact h e he
      · simp only [List.mem_singleton] at he
        subst he
        simp

/-- **Every cover a normalized shape carries has at least two keys**: a
singleton cover is presence, and is promoted rather than kept. -/
theorem normalize_covers_have_two_keys (fields : List Field) (tail : GTail Fact)
    (isList : Certainty) (nonEmpty : Bool) (covers : List Cover) :
    ∀ c ∈ (normalize fields tail isList nonEmpty covers).covers, 2 ≤ c.keys.length := by
  intro c hc
  unfold normalize at hc
  simp only [antichain] at hc
  have h₁ := (List.mem_filter.mp hc).1
  have h₂ := mem_canonCovers h₁
  have h₃ := (List.mem_filter.mp h₂).1
  exact foldCovers_two_keys tail covers (sortFields fields, [])
    (by intro d hd; exact absurd hd (by simp)) c h₃

/-! ### The S5 recording constructor

`recordCover` establishes nothing of its own: it routes through `normalize`, so
the two invariants that could be lost by adding a cover are the ones already
proved above, instantiated. That is the whole design argument for a normalizing
constructor, stated as theorems. -/

theorem recordCover_fieldsSorted (s : ShapeFact) (keys : List Key) (flavor : CoverFlavor) :
    FieldsSorted (GShape.recordCover s keys flavor).fields :=
  normalize_fieldsSorted _ _ _ _ _

theorem recordCover_covers_have_two_keys (s : ShapeFact) (keys : List Key) (flavor : CoverFlavor) :
    ∀ c ∈ (GShape.recordCover s keys flavor).covers, 2 ≤ c.keys.length :=
  normalize_covers_have_two_keys _ _ _ _ _

theorem recordCover_sealed_no_absent (s : ShapeFact) (keys : List Key) (flavor : CoverFlavor)
    (h : s.tail = GTail.sealed) :
    ∀ f ∈ (GShape.recordCover s keys flavor).fields, f.2.1 ≠ Presence.absent := by
  unfold GShape.recordCover
  rw [h]
  exact normalize_sealed_no_absent _ _ _ _

/-- A **singleton** input is not a disjunction — it is presence — so it is never
stored as a cover. The general statement is `recordCover_covers_have_two_keys`;
this names the case A-G8 singles out. -/
theorem recordCover_singleton_stores_no_cover (s : ShapeFact) (k : Key) (flavor : CoverFlavor) :
    ∀ c ∈ (GShape.recordCover s [k] flavor).covers, 2 ≤ c.keys.length :=
  recordCover_covers_have_two_keys s [k] flavor

/-- `coverProves` only ever answers with a cover the shape actually carries: the
discharge cannot invent a claim. -/
theorem coverProves_mem (s : ShapeFact) (key : Key) (absentKeys : List Key) :
    ∀ f, GShape.coverProves s key absentKeys = some f → ∃ c ∈ s.covers, c.flavor = f := by
  unfold GShape.coverProves
  simp only
  suffices hg : ∀ (l : List Cover) (acc : Option CoverFlavor),
      (∀ f, acc = some f → ∃ c ∈ s.covers, c.flavor = f) →
      (∀ c ∈ l, c ∈ s.covers) →
      ∀ f, (l.foldl (fun acc c =>
        match acc with
        | none => some c.flavor
        | some g => some (if Cover.flavorRank c.flavor < Cover.flavorRank g then c.flavor else g))
        acc) = some f → ∃ c ∈ s.covers, c.flavor = f by
    refine hg _ none (by intro f h; exact absurd h (by simp)) ?_
    intro c hc; exact (List.mem_filter.mp hc).1
  intro l
  induction l with
  | nil => intro acc h _ f hf; exact h f hf
  | cons x xs ih =>
    intro acc h hmem f hf
    refine ih _ ?_ (fun c hc => hmem c (List.mem_cons_of_mem _ hc)) f hf
    intro g hg
    cases acc with
    | none =>
      simp only [Option.some.injEq] at hg
      exact ⟨x, hmem x (by simp), hg⟩
    | some a =>
      simp only [Option.some.injEq] at hg
      split at hg
      · exact ⟨x, hmem x (by simp), hg⟩
      · exact h a rfl |>.imp (fun c hc => ⟨hc.1, hg ▸ hc.2⟩)

end SteinsDomain
