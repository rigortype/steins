import SteinsDomain.Canon
import SteinsDomain.Certainty
import SteinsDomain.Range

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

/-- A refinement on a scalar base (layer 3's content). -/
inductive Refinement where
  | str (p : StrPreds)
  | int (r : IntRange)
  deriving DecidableEq, Repr, Inhabited

/-- What is known about a single value, in one of the four layers. -/
inductive Fact where
  /-- Layer 1: exactly this value. -/
  | singleton (v : Val)
  /-- Layer 2: one of these values (canonical, `2..=CAP`). -/
  | oneOf (vs : List Val)
  /-- Layer 3: a scalar base plus a refinement; `nullable` adds `null`. -/
  | refined (base : Base) (r : Refinement) (nullable : Bool)
  /-- Layer 4: just a scalar base (plus optionally `null`). -/
  | general (base : Base) (nullable : Bool)
  deriving DecidableEq, Repr, Inhabited

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

/-- Finite members when this fact is in a finite layer. -/
def finiteMembers : Fact → Option (List Val)
  | .singleton v => some [v]
  | .oneOf vs => some vs
  | .refined .. => none
  | .general .. => none

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
def summarize (M : Model) (vals : List Val) : Option Fact :=
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
def fromVals (M : Model) (vals : List Val) : Option Fact :=
  match Val.canon vals with
  | [] => none
  | [v] => some (.singleton v)
  | c@(_ :: _ :: _) => if c.length ≤ CAP then some (.oneOf c) else summarize M c

/-! ## Join -/

def abstractParts : Fact → Option (Base × Option Refinement × Bool)
  | .refined b r n => some (b, some r, n)
  | .general b n => some (b, none, n)
  | .singleton _ => none
  | .oneOf _ => none

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
  match summarize M finite with
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
    | none => joinAbstract summary abs

/-- The least representable fact admitting both denotations. `none` means
"unrepresentable"; the caller drops the fact, which is the safe side. -/
def join (M : Model) (a b : Fact) : Option Fact :=
  match a.finiteMembers, b.finiteMembers with
  | some xs, some ys => fromVals M (xs ++ ys)
  | some xs, none => joinFiniteAbstract M xs b
  | none, some ys => joinFiniteAbstract M ys a
  | none, none => joinAbstract a b

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

/-- Certainty that the value is truthy under PHP semantics. -/
def truthy (M : Model) (f : Fact) : Certainty :=
  match f.finiteMembers with
  | some vals => Certainty.allOf (vals.map (fun v => Certainty.ofBool (!v.falsy M)))
  | none =>
    match f.abstractFalsyTruthy with
    | (false, true) => .yes
    | (true, false) => .no
    | _ => .maybe

/-- Certainty that the value is `null`. -/
def isNull : Fact → Certainty
  | .singleton v => Certainty.ofBool (decide (v = Val.null))
  | .oneOf vs => Certainty.allOf (vs.map (fun v => Certainty.ofBool (decide (v = Val.null))))
  | .refined _ _ nullable => if nullable then .maybe else .no
  | .general _ nullable => if nullable then .maybe else .no

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

end Fact
end SteinsDomain
