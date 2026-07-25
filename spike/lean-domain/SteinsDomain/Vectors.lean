import SteinsDomain.Queries

/-!
# The differential vector file

`lake exe vectors` prints a deterministic text file that
`crates/steins-domain/tests/lean_vectors.rs` regenerates from the Rust
implementation and diffs byte for byte (ADR-0059 §4).

Why a *regenerate-and-diff* file rather than a parsed input format: both sides
enumerate the same universe in the same documented order and render the results,
so no parser is needed on either side and a rendering mismatch is
self-describing — the line carries its own inputs.

The file has three kinds of content:

* `atom` lines — the concrete Rust value each abstract atom stands for, plus the
  field each side *computes*: the string's predicate summary, or a float/array's
  PHP falsiness. This is where the classifier obligation the proofs assume
  (`Model.predsOf_closed`, `Model.nonFalsy_iff`) is actually checked.
* `order` — the universe in ascending order, so a divergence between the spec's
  `(rank, tie)` order and Rust's `Ord for Val` shows up.
* behaviour lines — `admits`, `join`, `truthy`, `isNull`, `satisfiesStr`,
  `intIn`, and the exhaustive `assoc` tally.
-/

namespace SteinsDomain
namespace Vectors

/-! ## The model used by the vectors

The atom tables. Rank order matches Rust's `Ord` on the concrete values these
stand for; the `atom` lines in the output are what pins that down. -/

/-- `StrPreds::of` for the six string atoms. Ranks are the atoms' positions in
the **byte order** of the concrete strings they stand for — `"" < " 5 " < "0" <
"00" < "5" < "abc"` — because the spec's `Val.str rank` order has to be the real
order for the abstraction to be faithful. The `order` line checks it. -/
def vecPreds : Nat → StrPreds
  | 0 => { }                                                        -- ""
  | 1 => { nonEmpty := true, nonFalsy := true, numeric := true }     -- " 5 "  (PHP 8 trims)
  | 2 => { nonEmpty := true, numeric := true }                      -- "0"
  | 3 => { nonEmpty := true, nonFalsy := true, numeric := true }     -- "00"   (leading zeros are numeric)
  | 4 => { nonEmpty := true, nonFalsy := true, numeric := true }     -- "5"
  | 5 => { nonEmpty := true, nonFalsy := true }                      -- "abc"
  | _ => { }

/-- `php_str_is_falsy`: exactly `""` and `"0"`. Out-of-table ranks are `true` to
keep `nonFalsy_iff` total. -/
def vecStrFalsy : Nat → Bool
  | 0 => true
  | 2 => true
  | 1 | 3 | 4 | 5 => false
  | _ => true

/-- Float atoms: `-1.5`, `0.0`, `2.5`. -/
def vecFloatFalsy : Nat → Bool
  | 1 => true
  | _ => false

/-- Array atoms: `[]`, `[0 => 1]`. -/
def vecArrFalsy : Nat → Bool
  | 0 => true
  | _ => false

theorem vecPreds_closed : ∀ r, (vecPreds r).Closed := by
  intro r
  match r with
  | 0 | 1 | 2 | 3 | 4 | 5 => rfl
  | _ + 6 => rfl

theorem vecPreds_nonFalsy_iff : ∀ r, (vecPreds r).nonFalsy = !vecStrFalsy r := by
  intro r
  match r with
  | 0 | 1 | 2 | 3 | 4 | 5 => rfl
  | _ + 6 => rfl

/-- The model the vectors are computed in — a lawful `Model`, so every theorem in
`SteinsDomain.Soundness` and `SteinsDomain.Queries` applies to these very
numbers. -/
def vecModel : Model where
  predsOf := vecPreds
  strFalsy := vecStrFalsy
  floatFalsy := vecFloatFalsy
  arrFalsy := vecArrFalsy
  predsOf_closed := vecPreds_closed
  nonFalsy_iff := vecPreds_nonFalsy_iff

/-! ## Rendering -/

def renderBase : Base → String
  | .int => "int"
  | .float => "float"
  | .str => "str"
  | .bool => "bool"

def renderPreds (p : StrPreds) : String :=
  let parts := (if p.nonEmpty then ["NE"] else []) ++ (if p.nonFalsy then ["NF"] else [])
    ++ (if p.numeric then ["NUM"] else [])
  if parts.isEmpty then "-" else String.intercalate "|" parts

def renderInt (n : Int) : String :=
  if n = i64Min then "min" else if n = i64Max then "max" else toString n

def renderRange (r : IntRange) : String :=
  "[" ++ renderInt r.lo ++ "," ++ renderInt r.hi ++ "]"

def renderRef : Refinement → String
  | .str p => "str{" ++ renderPreds p ++ "}"
  | .int q => "int" ++ renderRange q

def renderVal : Val → String
  | .null => "null"
  | .bool b => if b then "true" else "false"
  | .int n => "int:" ++ renderInt n
  | .float r => "float#" ++ toString r
  | .str r => "str#" ++ toString r
  | .arr r => "arr#" ++ toString r

def renderNullable (n : Bool) : String := if n then "null" else "nonnull"

def renderFact : Fact → String
  | .singleton v => "S(" ++ renderVal v ++ ")"
  | .oneOf vs => "O(" ++ String.intercalate "," (vs.map renderVal) ++ ")"
  | .refined b r n =>
    "R(" ++ renderBase b ++ "," ++ renderRef r ++ "," ++ renderNullable n ++ ")"
  | .general b n => "G(" ++ renderBase b ++ "," ++ renderNullable n ++ ")"

/-- `none` is ⊤ — "the caller drops the fact". Spelled ASCII so the file stays
byte-comparable without encoding questions. -/
def renderOptFact : Option Fact → String
  | none => "TOP"
  | some f => renderFact f

def renderCert : Certainty → String
  | .yes => "yes"
  | .no => "no"
  | .maybe => "maybe"

/-! ## The universe -/

def values : List Val :=
  [ .null, .bool false, .bool true,
    .int i64Min, .int (-1), .int 0, .int 1, .int 2, .int 8, .int 9, .int i64Max,
    .float 0, .float 1, .float 2,
    .str 0, .str 1, .str 2, .str 3, .str 4, .str 5,
    .arr 0, .arr 1 ]

/-- The predicate sets a Rust caller can actually build, closed and unclosed
alike: the five closed sets, plus the two bare constants `NON_FALSY` and
`NUMERIC`, which are values of the Rust type but are not implication-closed.

`{NonFalsy, Numeric}` without `NonEmpty` is deliberately **absent**: `union`
closes and `intersect` cannot add bits, so no sequence of `StrPreds` operations
reaches it. Recorded in `REPORT.md` — closure is enforced by construction for
every set except the single-bit constants. -/
def predsUniverse : List StrPreds :=
  [ ⟨false, false, false⟩, ⟨true, false, false⟩, ⟨false, true, false⟩,
    ⟨false, false, true⟩, ⟨true, true, false⟩, ⟨true, false, true⟩,
    ⟨true, true, true⟩ ]

def rangeUniverse : List IntRange :=
  [ IntRange.full, ⟨1, i64Max⟩, ⟨i64Min, -1⟩, ⟨0, i64Max⟩, ⟨0, 0⟩, ⟨1, 9⟩, ⟨-5, 5⟩ ]

private def oneOfSeeds : List (List Val) :=
  [ [.int 1, .int 2],
    -- exactly CAP members: the last set that stays in the finite layer
    [.int i64Min, .int (-1), .int 0, .int 1, .int 2, .int 8, .int 9, .int i64Max],
    [.str 2, .str 4],
    [.null, .int 1],
    [.bool false, .bool true],
    [.int 1, .str 4],
    [.float 0, .float 1] ]

def facts : List Fact :=
  let singles : List Fact :=
    [Val.null, .bool false, .int 0, .int 1, .int 9, .str 0, .str 4, .float 1, .arr 0].map
      Fact.singleton
  let oneOfs : List Fact := oneOfSeeds.filterMap (Fact.fromVals vecModel)
  let refs : List Fact :=
    (predsUniverse.flatMap fun p =>
      [Fact.mkRefined .str (.str p) false, Fact.mkRefined .str (.str p) true]) ++
    (rangeUniverse.flatMap fun q =>
      [Fact.mkRefined .int (.int q) false, Fact.mkRefined .int (.int q) true])
  let gens : List Fact :=
    [Base.int, .float, .str, .bool].flatMap fun b => [Fact.general b false, Fact.general b true]
  (singles ++ oneOfs ++ refs ++ gens).eraseDups

/-! ## Associativity, checked exhaustively over the universe

Associativity of `join` is **not** proved in `SteinsDomain.Soundness` (see
`REPORT.md`): the CAP boundary makes the case analysis large. It is checked here
over every triple of the fact universe, on both sides of the harness, with
`none` read as ⊤ (absorbing) exactly as `join_envs` treats a dropped fact. -/

def joinOpt (M : Model) : Option Fact → Option Fact → Option Fact
  | none, _ => none
  | some _, none => none
  | some a, some b => Fact.join M a b

def assocTally (M : Model) (fs : List Fact) : Nat × Nat :=
  fs.foldl (fun acc a =>
    fs.foldl (fun acc b =>
      fs.foldl (fun (acc : Nat × Nat) c =>
        let lhs := joinOpt M (Fact.join M a b) (some c)
        let rhs := joinOpt M (some a) (Fact.join M b c)
        (acc.1 + 1, if lhs = rhs then acc.2 else acc.2 + 1)) acc) acc) (0, 0)

/-! ## The file -/

private def header : List String :=
  [ "# steins-domain differential vectors — GENERATED, do not hand-edit.",
    "# spec:    spike/lean-domain (ADR-0059), `lake exe vectors`",
    "# checker: crates/steins-domain/tests/lean_vectors.rs",
    "#",
    "# `atom` lines carry the concrete Rust value each abstract atom stands for and",
    "# the field each side computes independently (a string's predicate summary, a",
    "# float's/array's PHP falsiness) — this is the classifier check the proofs",
    "# assume but do not establish.  `order` pins the total order.  `TOP` is the",
    "# unrepresentable join: the caller drops the fact.",
    "version 1" ]

private def atomLines : List String :=
  [ "atom str#0 \"\" " ++ renderPreds (vecPreds 0),
    "atom str#1 \" 5 \" " ++ renderPreds (vecPreds 1),
    "atom str#2 \"0\" " ++ renderPreds (vecPreds 2),
    "atom str#3 \"00\" " ++ renderPreds (vecPreds 3),
    "atom str#4 \"5\" " ++ renderPreds (vecPreds 4),
    "atom str#5 \"abc\" " ++ renderPreds (vecPreds 5),
    "atom float#0 -1.5 " ++ (if vecFloatFalsy 0 then "falsy" else "truthy"),
    "atom float#1 0.0 " ++ (if vecFloatFalsy 1 then "falsy" else "truthy"),
    "atom float#2 2.5 " ++ (if vecFloatFalsy 2 then "falsy" else "truthy"),
    "atom arr#0 [] " ++ (if vecArrFalsy 0 then "falsy" else "truthy"),
    "atom arr#1 [0=>1] " ++ (if vecArrFalsy 1 then "falsy" else "truthy") ]

def render : String :=
  let ordered := Val.canon values
  let orderLine := "order " ++ String.intercalate " " (ordered.map renderVal)
  let admitsLines := facts.flatMap fun f =>
    values.map fun v =>
      "admits " ++ renderFact f ++ " " ++ renderVal v ++ " => " ++
        (if f.admits vecModel v then "true" else "false")
  let truthyLines := facts.map fun f =>
    "truthy " ++ renderFact f ++ " => " ++ renderCert (f.truthy vecModel)
  let isNullLines := facts.map fun f =>
    "isnull " ++ renderFact f ++ " => " ++ renderCert f.isNull
  let satisfiesLines := facts.flatMap fun f =>
    predsUniverse.map fun p =>
      "satisfiesstr " ++ renderFact f ++ " " ++ renderPreds p ++ " => " ++
        renderCert (f.satisfiesStr vecModel p)
  let intInLines := facts.flatMap fun f =>
    rangeUniverse.map fun r =>
      "intin " ++ renderFact f ++ " " ++ renderRange r ++ " => " ++
        renderCert (f.intIn r)
  let joinLines := facts.flatMap fun a =>
    facts.map fun b =>
      "join " ++ renderFact a ++ " " ++ renderFact b ++ " => " ++
        renderOptFact (Fact.join vecModel a b)
  let tally := assocTally vecModel facts
  let assocLine := "assoc " ++ toString tally.1 ++ " " ++ toString tally.2
  String.intercalate "\n"
    (header ++ ["#"] ++ atomLines ++ ["#", orderLine, "#"] ++
      admitsLines ++ truthyLines ++ isNullLines ++ satisfiesLines ++ intInLines ++
      joinLines ++ [assocLine]) ++ "\n"

end Vectors
end SteinsDomain
