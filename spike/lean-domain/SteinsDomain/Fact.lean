import SteinsDomain.Canon
import SteinsDomain.Certainty
import SteinsDomain.Range
import SteinsDomain.Shape

/-!
# The four-layer value domain

Ports `crates/steins-domain/src/fact.rs` (ADR-0035) clause for clause: `admits`,
the computed widening `summarize`, the layering decision `fromVals`, the join,
and the trinary queries.

Three places where Rust is `unreachable!`/`expect` and the spec must say
something total. Each is modelled by **widening**, which is the sound side, and
each is listed in `REPORT.md`:

* `intHullOf`/`strPredsOf` returning `none` — Rust `expect`s a non-empty fold
  after the base check. The spec widens to the General layer.
* `abstractFalsyTruthy` on a finite layer — Rust `unreachable!`s because the
  caller handles it. The spec answers `(true, true)`, i.e. `maybe`.
* `joinFiniteAbstract`'s non-abstract `abs` — Rust `unreachable!`s on the caller
  contract. The spec returns `none`, i.e. "drop the fact".
-/

namespace SteinsDomain

/-- Maximum cardinality of the `oneOf` layer (Rust `CAP`). -/
def CAP : Nat := 8

/-! The `Fact` and `Refinement` inductives live in `SteinsDomain.Shape`: `Fact`
gained a `shape` constructor whose payload nests `Option Fact`, so the two are
one recursive declaration and Lean requires them in one place. Everything else
about the four scalar layers is here, unchanged. -/

namespace Fact

/-- Normalising `Refined` constructor: a contentless refinement collapses to the
General layer, so a `refined` fact always carries real knowledge. -/
def mkRefined (b : Base) (r : Refinement) (nullable : Bool) : Fact :=
  match r with
  | .str p => if p.isEmpty then .general b nullable else .refined b r nullable
  | .int q => if q.isFull then .general b nullable else .refined b r nullable

/-- Does the refinement admit this value? Split out because the soundness proofs
need it independently of the null/base dispatch. -/
def refAdmits (M : Model) (r : Refinement) (v : Val) : Bool :=
  match r, v with
  | .str p, .str k => (M.predsOf k).containsAll p
  | .int q, .int i => q.contains i
  | _, _ => false

/-- The value stored at `k`, if the array has that key. -/
def entryOf (entries : List (Key × Val)) (k : Key) : Option Val :=
  (entries.find? (fun e => decide (e.1 = k))).map (·.2)

/-- Is a cover satisfied by this array? `isset` needs a present **non-null**
value; `keyExists` needs only presence (A-G8). -/
def coverSat (c : Cover) (entries : List (Key × Val)) : Bool :=
  c.keys.any (fun k =>
    match entryOf entries k with
    | none => false
    | some v => match c.flavor with
      | .isset => decide (v ≠ Val.null)
      | .keyExists => true)

/-! Extensional membership. The array stratum recurses into the shape's value
slots, which are *structural subterms* of the fact — so `admits`, unlike the
join below, is the full recursive mirror of Rust with no modelling shortcut. -/
mutual

/-- Extensional membership: is `v` in this fact's denotation?

`oneOf` is linear membership where Rust uses `binary_search`; see the note in
`SteinsDomain.Canon`. -/
def admits (M : Model) : Fact → Val → Bool
  | .singleton s, v => decide (s = v)
  | .oneOf vs, v => decide (v ∈ vs)
  | .refined b r nullable, v =>
    match v with
    | .null => nullable
    | _ => decide (v.base = some b) && refAdmits M r v
  | .general b nullable, v =>
    match v with
    | .null => nullable
    | _ => decide (v.base = some b)
  | .shape s nullable, v =>
    match v with
    | .null => nullable
    | .arr r => shapeAdmits M s (M.arrEntries r)
    | _ => false
termination_by f _ => sizeOf f

/-- Does this shape admit the array `entries`? -/
def shapeAdmits (M : Model) : ShapeFact → List (Key × Val) → Bool
  | ⟨fields, tail, isList, nonEmpty, covers⟩, entries =>
    fieldsAdmit M fields entries
      && entries.all (fun e => extraOk M fields tail e)
      && (match isList with
          | .yes => arrayIsList entries
          | .no => !arrayIsList entries
          | .maybe => true)
      && (!nonEmpty || !entries.isEmpty)
      && covers.all (fun c => coverSat c entries)
termination_by s _ => sizeOf s + 1

/-- Every declared field's presence and value contract holds. The `witnessed`
bit is not read: it is provenance, not extension (A-G9). -/
def fieldsAdmit (M : Model) : List Field → List (Key × Val) → Bool
  | [], _ => true
  | (k, p, slot) :: rest, entries =>
    (match p, entryOf entries k with
     | .required _, none => false
     | .required _, some v => slotAdmits M slot v
     | .optional, none => true
     | .optional, some v => slotAdmits M slot v
     | .absent, none => true
     | .absent, some _ => false)
    && fieldsAdmit M rest entries
termination_by fs _ => sizeOf fs

/-- An entry whose key the shape does not declare must satisfy the tail. -/
def extraOk (M : Model) (fields : List Field) : GTail Fact → Key × Val → Bool
  | tail, e =>
    if (fieldOf fields e.1).isSome then true
    else match tail with
      | .sealed => false
      | .unsealed kc slot => kc.admitsKey e.1 && slotAdmits M slot e.2
termination_by t _ => sizeOf t

/-- An unknown slot is the honest floor: it admits anything. -/
def slotAdmits (M : Model) : Slot → Val → Bool
  | none, _ => true
  | some f, v => admits M f v
termination_by sl _ => sizeOf sl

end

/-- Finite members when this fact is in a finite layer. -/
def finiteMembers : Fact → Option (List Val)
  | .singleton v => some [v]
  | .oneOf vs => some vs
  | .refined .. => none
  | .general .. => none
  | .shape .. => none

/-! ## The computed widening -/

/-- Convex hull of the int members. `none` for a list with no int member —
unreachable after `summarize`'s base check, and widened rather than panicked. -/
def intHullOf : List Val → Option IntRange
  | [] => none
  | .int i :: vs =>
    match intHullOf vs with
    | none => some (IntRange.point i)
    | some r => some (r.hull (IntRange.point i))
  | _ :: vs => intHullOf vs

/-- Intersection of the string members' predicate summaries — the *computed*
widening: precision loss is measured member by member, never guessed. -/
def strPredsOf (M : Model) : List Val → Option StrPreds
  | [] => none
  | .str k :: vs =>
    match strPredsOf M vs with
    | none => some (M.predsOf k)
    | some p => some (p.inter (M.predsOf k))
  | _ :: vs => strPredsOf M vs

/-- Widen a non-empty, deduped value list to an abstract summary. `none` when
unsummarisable (mixed scalar bases, arrays present). -/
def summarizeScalar (M : Model) (vals : List Val) : Option Fact :=
  let nullable := decide (Val.null ∈ vals)
  let scalars := vals.filter (fun v => decide (v ≠ Val.null))
  match scalars with
  | [] =>
    -- Every member was null; the finite layer already represents that.
    some (.singleton .null)
  | first :: _ =>
    match first.base with
    | none => none
    | some b =>
      if scalars.any (fun v => decide (v.base ≠ some b)) then none
      else
        match b with
        | .int =>
          match intHullOf scalars with
          | some r => some (mkRefined .int (.int r) nullable)
          | none => some (.general .int nullable)
        | .str =>
          match strPredsOf M scalars with
          | some p => some (mkRefined .str (.str p) nullable)
          | none => some (.general .str nullable)
        | .float => some (.general .float nullable)
        | .bool => some (.general .bool nullable)

/-- Build a fact from values: canonicalised, then layered by cardinality. `none`
for an empty input or an unsummarisable overflow. -/
def fromValsScalar (M : Model) (vals : List Val) : Option Fact :=
  match Val.canon vals with
  | [] => none
  | [v] => some (.singleton v)
  | c@(_ :: _ :: _) => if c.length ≤ CAP then some (.oneOf c) else summarizeScalar M c

/-! ## Join -/

def abstractParts : Fact → Option (Base × Option Refinement × Bool)
  | .refined b r n => some (b, some r, n)
  | .general b n => some (b, none, n)
  | .singleton _ => none
  | .oneOf _ => none
  -- The array stratum has no scalar base, so the scalar join drops it.
  | .shape _ _ => none

def joinAbstract (a b : Fact) : Option Fact :=
  match abstractParts a, abstractParts b with
  | some (ab, ar, an), some (bb, br, bn) =>
    if ab ≠ bb then none
    else
      let nullable := an || bn
      match ar, br with
      | some (.str p), some (.str q) => some (mkRefined ab (.str (p.inter q)) nullable)
      | some (.int r), some (.int s) => some (mkRefined ab (.int (r.hull s)) nullable)
      -- A refinement joined with no-knowledge (or mismatched kinds, which cannot
      -- occur for one base) widens to General.
      | _, _ => some (.general ab nullable)
  | _, _ => none

def joinFiniteAbstract (M : Model) (finite : List Val) (abs : Fact) : Option Fact :=
  match summarizeScalar M finite with
  | none => none
  | some summary =>
    match summary.finiteMembers with
    -- The finite side was all-null: fold it in as nullability.
    | some _ =>
      match abs with
      | .refined b r _ => some (mkRefined b r true)
      | .general b _ => some (.general b true)
      | .singleton _ => none
      | .oneOf _ => none
      -- The array stratum is not a scalar abstract layer; the scalar join
      -- drops the fact, which is the safe side.
      | .shape _ _ => none
    | none => joinAbstract summary abs

/-- The least representable fact admitting both denotations. `none` means
"unrepresentable"; the caller drops the fact, which is the safe side. -/
def joinScalar (M : Model) (a b : Fact) : Option Fact :=
  match a.finiteMembers, b.finiteMembers with
  | some xs, some ys => fromValsScalar M (xs ++ ys)
  | some xs, none => joinFiniteAbstract M xs b
  | none, some ys => joinFiniteAbstract M ys a
  | none, none => joinAbstract a b

/-! ## The array stratum (ADR-0062)

Layered on the scalar core rather than woven into it, and deliberately so. The
spec's `Val.arr` is an **opaque rank** whose entries come from `Model.arrEntries`
(the same abstraction that makes the scalar theorems independent of PHP's
`is_numeric` grammar), so a value reached through an array atom has no structure
for a termination measure to descend on. Two consequences, both *widenings* —
the sound side — and both listed in `REPORT.md`:

* a value slot that is itself a shape fact joins to `unknown` here, where Rust
  joins it recursively (`joinSlot` goes through `joinScalar`);
* the computed descent builds slot facts with `fromValsScalar`, where Rust calls
  the full `from_vals`.

Neither is reachable from the differential vector universe, so the two sides
agree on every vector; both make the spec *weaker*, never stronger, so nothing
proved about the scalar core is put at risk.

Everything the scalar core proves (`join_sound`, `join_comm`, `summarize_admits`,
`fromVals_admits`) is stated about `joinScalar`/`summarizeScalar`/
`fromValsScalar`, which are the pre-existing definitions verbatim — the array
stratum adds behaviour, it does not weaken a theorem. -/

/-- Field-width bound for a single shape (A-G6): PHPStan's `ARRAY_COUNT_LIMIT`,
imported as-is. -/
def SHAPE_WIDTH_LIMIT : Nat := 256

def isShapeFact : Fact → Bool
  | .shape _ _ => true
  | _ => false

/-- Value-slot join. `none` (unknown) on either side absorbs, and an
unrepresentable join degrades to unknown as well. -/
def joinSlot (M : Model) (a b : Slot) : Slot :=
  match a, b with
  | some x, some y => joinScalar M x y
  | _, _ => none

/-- A field present on one side only joins against the other side's tail bound:
an unsealed tail may already admit that key, a sealed one cannot. -/
def joinSlotWithTail (M : Model) (slot : Slot) (other : GTail Fact) : Slot :=
  match other with
  | .sealed => slot
  | .unsealed _ value => joinSlot M slot value

/-- `Sealed ⊔ Sealed = Sealed`; a sealed side yields to an unsealed one; two
unsealed tails join key-class and value (A-G5). -/
def joinTails (M : Model) (a b : GTail Fact) : GTail Fact :=
  match a, b with
  | .sealed, .sealed => .sealed
  | .sealed, t => t
  | t, .sealed => t
  | .unsealed ka va, .unsealed kb vb => .unsealed (ka.join kb) (joinSlot M va vb)

/-- The union of the two field key sets, canonically ordered. -/
def joinKeys (a b : List Field) : List Key :=
  canonKeys ((a.map (·.1)) ++ (b.map (·.1)))

/-- **Shape join** (A-G5): field-wise, with the tail absorbing the key-set
difference. `Sealed{a} ⊔ Sealed{b} = {a?, b?} + Sealed`. -/
def shapeJoin (M : Model) (a b : ShapeFact) : ShapeFact :=
  let fields := (joinKeys a.fields b.fields).map (fun k =>
    match GShape.field a k, GShape.field b k with
    | some (_, pa, sa), some (_, pb, sb) => (k, pa.join pb, joinSlot M sa sb)
    | some (_, _, sa), none => (k, Presence.optional, joinSlotWithTail M sa b.tail)
    | none, some (_, _, sb) => (k, Presence.optional, joinSlotWithTail M sb a.tail)
    | none, none => (k, Presence.optional, none))
  -- A cover survives only when *both* sides imply it (A-G8).
  let covers := (a.covers ++ b.covers).filter (fun c =>
    GShape.impliesCover a c && GShape.impliesCover b c)
  normalize fields (joinTails M a.tail b.tail) (Certainty.allOf [a.isList, b.isList])
    (a.nonEmpty && b.nonEmpty) covers

/-- The tail-only summary an over-wide array degrades to (A-G6). -/
def tailSummary (M : Model) (entries : List (Key × Val)) : GTail Fact :=
  let kc := entries.foldl
    (fun acc e => match acc with
      | none => some (KeyClass.ofKey e.1)
      | some c => some (c.join (KeyClass.ofKey e.1))) none
  .unsealed (kc.getD .arrayKey) (fromValsScalar M (entries.map (·.2)))

/-- **Lift** an order-witnessed array value into the abstract stratum (A-G5):
every entry becomes `required true` with a `singleton` slot, the tail seals, and
`isList` is the real `arrayIsList` verdict. This is where order-witnessed-ness
is honestly lost. A nested array lifts to a `singleton` slot, not a nested
shape — the singleton is strictly more precise. -/
def lift (M : Model) (entries : List (Key × Val)) : ShapeFact :=
  let isList := Certainty.ofBool (arrayIsList entries)
  let nonEmpty := !entries.isEmpty
  if entries.length > SHAPE_WIDTH_LIMIT then
    normalize [] (tailSummary M entries) isList nonEmpty []
  else
    normalize (entries.map (fun e => (e.1, Presence.required true, some (Fact.singleton e.2))))
      .sealed isList nonEmpty []

/-- `(the array shape, nullability)` for a `oneOf` of arrays and nulls; `none`
as soon as a member is neither. -/
def oneOfShapeView (M : Model) : List Val → Option ShapeFact → Bool →
    Option (Option ShapeFact × Bool)
  | [], acc, nullable => some (acc, nullable)
  | .null :: rest, acc, _ => oneOfShapeView M rest acc true
  | .arr r :: rest, acc, nullable =>
    let lifted := lift M (M.arrEntries r)
    oneOfShapeView M rest
      (some (match acc with | none => lifted | some prev => shapeJoin M prev lifted)) nullable
  | _ :: _, _, _ => none

/-- `(the array shape, nullability)` for an array-shaped fact; `none` when the
fact denotes anything outside `array|null`. -/
def shapeView (M : Model) : Fact → Option (Option ShapeFact × Bool)
  | .shape s nullable => some (some s, nullable)
  | .singleton (.arr r) => some (some (lift M (M.arrEntries r)), false)
  | .singleton .null => some (none, true)
  | .oneOf vs => oneOfShapeView M vs none false
  | _ => none

/-- The full join: the array stratum's own algebra when either operand is a
shape fact, the scalar core otherwise. Mixed-base unions stay un-facted, exactly
as for scalars (A-G2). -/
def join (M : Model) (a b : Fact) : Option Fact :=
  if isShapeFact a || isShapeFact b then
    match shapeView M a, shapeView M b with
    | some (sa, na), some (sb, nb) =>
      match sa, sb with
      | some x, some y => some (.shape (shapeJoin M x y) (na || nb))
      | some x, none => some (.shape x (na || nb))
      | none, some y => some (.shape y (na || nb))
      | none, none => none
    | _, _ => none
  else joinScalar M a b

/-- Off the array stratum the full join *is* the scalar core, so every theorem
proved about `joinScalar` is a theorem about `join` there. -/
theorem join_eq_joinScalar {M : Model} {a b : Fact}
    (ha : isShapeFact a = false) (hb : isShapeFact b = false) :
    join M a b = joinScalar M a b := by
  simp [join, ha, hb]

def entriesOf (M : Model) : Val → List (Key × Val)
  | .arr r => M.arrEntries r
  | _ => []

def isArrVal : Val → Bool
  | .arr _ => true
  | _ => false

/-- **The computed descent** (ADR-0062 §3): keys present in every member are
`required true`, keys in some are `optional`, and the tail seals because the
members enumerate every key there is. Value slots are the members' own widening,
so the summary is derived member by member — never a threshold heuristic. -/
def shapeDescent (M : Model) (vals : List Val) (nullable : Bool) : Fact :=
  let entries := vals.map (entriesOf M)
  let nonEmpty := entries.all (fun e => !e.isEmpty)
  let isList := Certainty.allOf (entries.map (fun e => Certainty.ofBool (arrayIsList e)))
  let keys := canonKeys (entries.flatMap (fun e => e.map (·.1)))
  if keys.length > SHAPE_WIDTH_LIMIT then
    .shape (normalize [] (tailSummary M entries.flatten) isList nonEmpty []) nullable
  else
    let fields := keys.map (fun k =>
      (k,
       if entries.all (fun e => (entryOf e k).isSome) then Presence.required true
       else Presence.optional,
       fromValsScalar M (entries.filterMap (fun e => entryOf e k))))
    .shape (normalize fields .sealed isList nonEmpty []) nullable

/-- The widening with the array stratum: an all-array overflow descends to a
shape rather than being dropped. A *mixed* overflow is still unsummarizable. -/
def summarize (M : Model) (vals : List Val) : Option Fact :=
  let nullable := decide (Val.null ∈ vals)
  let scalars := vals.filter (fun v => decide (v ≠ Val.null))
  match scalars with
  | [] => some (.singleton .null)
  | _ :: _ =>
    if scalars.all isArrVal then some (shapeDescent M scalars nullable)
    else summarizeScalar M vals

/-- Layering by cardinality, over the widening that includes the array stratum
— Rust's `Fact::from_vals`. -/
def fromVals (M : Model) (vals : List Val) : Option Fact :=
  match Val.canon vals with
  | [] => none
  | [v] => some (.singleton v)
  | c@(_ :: _ :: _) => if c.length ≤ CAP then some (.oneOf c) else summarize M c

/-! ## Trinary queries -/

/-- `(canBeFalsy, canBeTruthy)` for the abstract layers. The finite layers are
`(true, true)` — i.e. `maybe` — where Rust `unreachable!`s. -/
def abstractFalsyTruthy : Fact → Bool × Bool
  | .singleton _ => (true, true)
  | .oneOf _ => (true, true)
  | .refined b r nullable =>
    let ft := match b, r with
      | .str, .str p =>
        -- Some truthy string satisfies any predicate set; falsy strings are
        -- excluded exactly by NON_FALSY.
        (!p.containsAll StrPreds.NON_FALSY, true)
      | .int, .int q => (q.contains 0, decide (q ≠ IntRange.point 0))
      | _, _ => (true, true)
    (ft.1 || nullable, ft.2)
  | .general _ _ => (true, true)
  -- An array is falsy exactly when empty; `null` is falsy too. The truthy side
  -- is over-approximated to `true` (see the Rust comment): it pushes the
  -- verdict toward `maybe`, the honest direction for a trinary.
  | .shape s nullable => (!s.nonEmpty || nullable, true)

/-- Certainty that the value is truthy under PHP semantics. -/
def truthy (M : Model) (f : Fact) : Certainty :=
  match f.finiteMembers with
  | some vals => Certainty.allOf (vals.map (fun v => Certainty.ofBool (!v.falsy M)))
  | none =>
    match f.abstractFalsyTruthy with
    | (false, true) => .yes
    | (true, false) => .no
    | _ => .maybe

/-- Certainty that the value is a string satisfying every predicate in `pred`. -/
def satisfiesStr (M : Model) (f : Fact) (pred : StrPreds) : Certainty :=
  let evalOne : Val → Certainty := fun v =>
    match v with
    | .str k => Certainty.ofBool ((M.predsOf k).containsAll pred)
    | _ => .no
  match f with
  | .singleton v => evalOne v
  | .oneOf vs => Certainty.allOf (vs.map evalOne)
  | .refined b r nullable =>
    if b ≠ .str then .no
    else
      match r with
      | .str p => if p.containsAll pred && !nullable then .yes else .maybe
      | .int _ => .maybe
  | .general b _ => if b = .str then .maybe else .no
  -- An array is never a string, and neither is null.
  | .shape _ _ => .no

/-- Certainty that the value is an int within `range`. -/
def intIn (f : Fact) (range : IntRange) : Certainty :=
  let evalOne : Val → Certainty := fun v =>
    match v with
    | .int i => Certainty.ofBool (range.contains i)
    | _ => .no
  match f with
  | .singleton v => evalOne v
  | .oneOf vs => Certainty.allOf (vs.map evalOne)
  | .refined b r nullable =>
    if b ≠ .int then .no
    else
      match r with
      | .int q =>
        if range.containsRange q && !nullable then .yes
        else if q.inter range = none then .no
        else .maybe
      | .str _ => .maybe
  | .general b _ => if b = .int then .maybe else .no
  -- An array is never an int, and neither is null.
  | .shape _ _ => .no

end Fact
end SteinsDomain
