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

/-- `StrPreds::of` for the seven string atoms. Ranks are the atoms' positions in
the **byte order** of the concrete strings they stand for — `"" < " 5 " < "0" <
"00" < "5" < "ABC" < "abc"` — because the spec's `Val.str rank` order has to be
the real order for the abstraction to be faithful. The `order` line checks it.

`"ABC"` is the atom the casing predicates need: without a value that is
uppercase and *not* lowercase, no vector would tell the two apart. Note that
every uncased atom (`""`, `" 5 "`, `"0"`, `"00"`, `"5"`) carries **both**
casings — `lowercase-string` is `strtolower($s) === $s`, an identity, not a
letter test. -/
def vecPreds : Nat → StrPreds
  | 0 => { lowercase := true, uppercase := true }                   -- ""
  | 1 => { nonEmpty := true, nonFalsy := true, numeric := true,
           lowercase := true, uppercase := true }                   -- " 5 "  (PHP 8 trims)
  | 2 => { nonEmpty := true, numeric := true,
           lowercase := true, uppercase := true }                   -- "0"
  | 3 => { nonEmpty := true, nonFalsy := true, numeric := true,
           lowercase := true, uppercase := true }                   -- "00"   (leading zeros are numeric)
  | 4 => { nonEmpty := true, nonFalsy := true, numeric := true,
           lowercase := true, uppercase := true }                   -- "5"
  | 5 => { nonEmpty := true, nonFalsy := true, uppercase := true }   -- "ABC"
  | 6 => { nonEmpty := true, nonFalsy := true, lowercase := true }   -- "abc"
  | _ => { }

/-- `php_str_is_falsy`: exactly `""` and `"0"`. Out-of-table ranks are `true` to
keep `nonFalsy_iff` total. -/
def vecStrFalsy : Nat → Bool
  | 0 => true
  | 2 => true
  | 1 | 3 | 4 | 5 | 6 => false
  | _ => true

/-- Float atoms: `-1.5`, `0.0`, `2.5`. -/
def vecFloatFalsy : Nat → Bool
  | 1 => true
  | _ => false

/-- The array atoms, as their real entry lists. Ranks are the atoms' positions
in the domain's total order on `Val` — `[] < [0=>1] < [0=>1,1=>2] < …` — because
the spec models an array value as its position in that order. Ranks 0 and 1 are
the only ones the scalar universe uses; the rest exist for the shape section
(ADR-0062 S2). The `shapearr` lines pin the whole table. -/
def vecArrEntries : Nat → List (Key × Val)
  | 0 => []
  | 1 => [(.int 0, .int 1)]
  | 2 => [(.int 0, .int 1), (.int 1, .int 2)]
  | 3 => [(.int 0, .int 1), (.str 0, .int 2)]
  | 4 => [(.int 1, .int 2)]
  | 5 => [(.str 0, .int 1)]
  | 6 => [(.str 0, .int 2)]
  | 7 => [(.str 0, .int 2), (.str 1, .int 1)]
  | 8 => [(.str 0, .int 3)]
  | 9 => [(.str 0, .int 4)]
  | 10 => [(.str 0, .int 5)]
  | 11 => [(.str 0, .int 6)]
  | 12 => [(.str 0, .int 7)]
  | 13 => [(.str 0, .int 8)]
  | 14 => [(.str 0, .int 9)]
  | 15 => [(.str 1, .int 1)]
  | _ => []

/-- `php_is_falsy` on an array: exactly the empty array. Derived from the entry
table, which is what makes `Model.arrFalsy_iff` hold by `rfl`. -/
def vecArrFalsy (r : Nat) : Bool := (vecArrEntries r).isEmpty

theorem vecPreds_closed : ∀ r, (vecPreds r).Closed := by
  intro r
  match r with
  | 0 | 1 | 2 | 3 | 4 | 5 | 6 => rfl
  | _ + 7 => rfl

theorem vecPreds_nonFalsy_iff : ∀ r, (vecPreds r).nonFalsy = !vecStrFalsy r := by
  intro r
  match r with
  | 0 | 1 | 2 | 3 | 4 | 5 | 6 => rfl
  | _ + 7 => rfl

/-- The model the vectors are computed in — a lawful `Model`, so every theorem in
`SteinsDomain.Soundness` and `SteinsDomain.Queries` applies to these very
numbers. -/
def vecModel : Model where
  predsOf := vecPreds
  strFalsy := vecStrFalsy
  floatFalsy := vecFloatFalsy
  arrFalsy := vecArrFalsy
  arrEntries := vecArrEntries
  predsOf_closed := vecPreds_closed
  nonFalsy_iff := vecPreds_nonFalsy_iff
  arrFalsy_iff := fun _ => rfl

/-! ## Rendering -/

def renderBase : Base → String
  | .int => "int"
  | .float => "float"
  | .str => "str"
  | .bool => "bool"

def renderPreds (p : StrPreds) : String :=
  let parts := (if p.nonEmpty then ["NE"] else []) ++ (if p.nonFalsy then ["NF"] else [])
    ++ (if p.numeric then ["NUM"] else []) ++ (if p.lowercase then ["LC"] else [])
    ++ (if p.uppercase then ["UC"] else [])
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

def renderCert : Certainty → String
  | .yes => "yes"
  | .no => "no"
  | .maybe => "maybe"

def renderKey : Key → String
  | .int n => "i" ++ renderInt n
  | .str r => "s" ++ toString r

def renderPresence : Presence → String
  | .required true => "R!"
  | .required false => "R"
  | .optional => "O"
  | .absent => "X"

def renderKeyClass : KeyClass → String
  | .int => "i"
  | .str => "s"
  | .arrayKey => "k"

def renderCover (c : Cover) : String :=
  String.intercalate "+" (c.keys.map renderKey) ++ "@" ++
    (match c.flavor with | .isset => "i" | .keyExists => "e")

mutual

def renderFact : Fact → String
  | .singleton v => "S(" ++ renderVal v ++ ")"
  | .oneOf vs => "O(" ++ String.intercalate "," (vs.map renderVal) ++ ")"
  | .refined b r n =>
    "R(" ++ renderBase b ++ "," ++ renderRef r ++ "," ++ renderNullable n ++ ")"
  | .general b n => "G(" ++ renderBase b ++ "," ++ renderNullable n ++ ")"
  | .shape s n => "A(" ++ renderShape s ++ "," ++ renderNullable n ++ ")"
termination_by f => sizeOf f

def renderShape : ShapeFact → String
  | ⟨fields, tail, isList, nonEmpty, covers⟩ =>
    "{" ++ (if fields.isEmpty then "-" else renderFields fields)
      ++ "|" ++ renderTail tail
      ++ "|" ++ renderCert isList
      ++ "|" ++ (if nonEmpty then "ne" else "-")
      ++ "|" ++ (if covers.isEmpty then "-"
                 else String.intercalate ";" (covers.map renderCover))
      ++ "}"
termination_by s => sizeOf s + 1

def renderFields : List Field → String
  | [] => ""
  | [(k, p, slot)] => renderKey k ++ "=" ++ renderPresence p ++ ":" ++ renderSlot slot
  | (k, p, slot) :: rest =>
    renderKey k ++ "=" ++ renderPresence p ++ ":" ++ renderSlot slot ++ ";" ++ renderFields rest
termination_by fs => sizeOf fs

def renderTail : GTail Fact → String
  | .sealed => "."
  | .unsealed kc slot => "*" ++ renderKeyClass kc ++ ":" ++ renderSlot slot
termination_by t => sizeOf t

def renderSlot : Slot → String
  | none => "-"
  | some f => renderFact f
termination_by sl => sizeOf sl

end

/-- `none` is ⊤ — "the caller drops the fact". Spelled ASCII so the file stays
byte-comparable without encoding questions. -/
def renderOptFact : Option Fact → String
  | none => "TOP"
  | some f => renderFact f


/-! ## The universe -/

def values : List Val :=
  [ .null, .bool false, .bool true,
    .int i64Min, .int (-1), .int 0, .int 1, .int 2, .int 8, .int 9, .int i64Max,
    .float 0, .float 1, .float 2,
    .str 0, .str 1, .str 2, .str 3, .str 4, .str 5, .str 6,
    .arr 0, .arr 1 ]

/-- The predicate sets a Rust caller can actually build, closed and unclosed
alike: the five closed casing-free sets, plus the two bare constants
`NON_FALSY` and `NUMERIC`, which are values of the Rust type but are not
implication-closed, and then ten casing sets.

`{NonFalsy, Numeric}` without `NonEmpty` is deliberately **absent**: `union`
closes and `intersect` cannot add bits, so no sequence of `StrPreds` operations
reaches it. Recorded in `REPORT.md` — closure is enforced by construction for
every set except the single-bit constants.

The casing tail is a **spanning subset**, not the 4× cross product the two new
orthogonal bits would allow: the full product multiplies the fact universe by
four and the associativity tally by sixty-four, for no interaction class the ten
below miss. They are: each casing alone; both together (an uncased string); each
against the length half; both against it; and casing against `NonFalsy` and
against `Numeric` in each direction — the last three written as `StrPreds::of`
of the string that produces them, which is how the Rust side builds them too.
The four the shipped table lowers to (`lowercase-string`, `uppercase-string`
and their `non-empty-` forms) are all in here. -/
def predsUniverse : List StrPreds :=
  [ ⟨false, false, false, false, false⟩, ⟨true, false, false, false, false⟩,
    ⟨false, true, false, false, false⟩, ⟨false, false, true, false, false⟩,
    ⟨true, true, false, false, false⟩, ⟨true, false, true, false, false⟩,
    ⟨true, true, true, false, false⟩,
    -- LOWERCASE, UPPERCASE, and the uncased set that carries both
    ⟨false, false, false, true, false⟩, ⟨false, false, false, false, true⟩,
    ⟨false, false, false, true, true⟩,
    -- the `non-empty-` intersections (two of them are shipped table rows)
    ⟨true, false, false, true, false⟩, ⟨true, false, false, false, true⟩,
    ⟨true, false, false, true, true⟩,
    -- casing × the truthiness/numeric axes: `of "abc"`; numeric-and-cased
    -- without non-falsy (the `"0"`-in-the-set class, reachable as
    -- `of "0" ⊓ of "1e5"`); and `of "5"`, the witness that every predicate is
    -- jointly satisfiable
    ⟨true, true, false, true, false⟩, ⟨true, false, true, true, false⟩,
    ⟨true, false, true, false, true⟩, ⟨true, true, true, true, true⟩ ]

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

/-- `Fact` is a nested inductive, so `deriving DecidableEq` has no handler for
it (see `SteinsDomain.Shape`); the tally compares with the written-out Boolean
equality, which is the same relation. -/
def optFactBeq : Option Fact → Option Fact → Bool
  | none, none => true
  | some a, some b => Fact.beq a b
  | _, _ => false

def assocTally (M : Model) (fs : List Fact) : Nat × Nat :=
  fs.foldl (fun acc a =>
    fs.foldl (fun acc b =>
      fs.foldl (fun (acc : Nat × Nat) c =>
        let lhs := joinOpt M (Fact.join M a b) (some c)
        let rhs := joinOpt M (some a) (Fact.join M b c)
        (acc.1 + 1, if optFactBeq lhs rhs then acc.2 else acc.2 + 1)) acc) acc) (0, 0)


/-! ## The shape section (ADR-0062 S2)

Appended after the scalar vectors, so every line above is untouched by the array
stratum landing. The seeds are the *raw* inputs to `normalize`, so the rendered
result shows what normalization did: sorting, the singleton-cover promotion, the
sealed-`absent` drop, the cover antichain, and the denotational `isList`
recomputation. Rows 0–8 are the ADR-0062 §3 / RFC #14939 `isList` table; 9–11 are
the A-G1 lowerings; 12–18 exercise A-G8's cover laws and the remaining
normalization invariants. -/

private def sreq : Presence := .required true
private def sint (i : Int) : Slot := some (Fact.singleton (.int i))
private def sgint : Slot := some (Fact.general .int false)
private def sealSeed (fields : List Field) : ShapeFact := normalize fields .sealed .maybe false []

def shapeSeeds : List ShapeFact :=
  [ sealSeed []                                                          -- 0  array{}
  , sealSeed [(.int 0, sreq, sint 1)]                                    -- 1  array{0: 1}
  , sealSeed [(.int 0, .optional, sint 1)]                               -- 2  array{0?: 1}
  , sealSeed [(.int 0, sreq, sint 1), (.int 1, sreq, sint 2)]            -- 3  array{0: 1, 1: 2}
  , sealSeed [(.str 0, .optional, sint 1)]                               -- 4  array{a?: 1}
  , sealSeed [(.str 0, sreq, sint 1)]                                    -- 5  array{a: 1}
  , sealSeed [(.int 1, sreq, sint 2)]                                    -- 6  array{1: 2}
  , sealSeed [(.int 0, .optional, sint 1), (.int 1, sreq, sint 2)]       -- 7  array{0?: 1, 1: 2}
  , sealSeed [(.int (-1), sreq, sint 2)]                                 -- 8  array{-1: 2}
  , plainArray                                                       -- 9  array
  , normalize [] (.unsealed .int sgint) .yes false []                -- 10 list<int>
  , normalize [(.str 0, sreq, sgint)] (.unsealed .str sgint) .maybe false []
                                                                     -- 11 tail-key fixture
  , normalize [(.int 1, sreq, sint 2)] (.unsealed .str none) .maybe false []
                                                                     -- 12 string tail, gap at 0
  , normalize [(.str 0, .optional, none), (.str 1, .optional, none)] .sealed .maybe false
      [{ keys := [.str 1, .str 0], flavor := .isset }]                -- 13 isset cover
  , normalize [(.str 0, .optional, none), (.str 1, .optional, none)] .sealed .maybe false
      [{ keys := [.str 0, .str 1], flavor := .keyExists }]            -- 14 keyExists cover
  , normalize [(.str 0, .optional, sint 1)] .sealed .maybe false
      [{ keys := [.str 0], flavor := .isset }]                        -- 15 singleton promotes
  , normalize [(.str 0, .optional, none), (.str 1, .optional, none), (.str 2, .optional, none)]
      .sealed .maybe false
      [{ keys := [.str 0, .str 1, .str 2], flavor := .keyExists },
       { keys := [.str 1, .str 0], flavor := .keyExists }]            -- 16 antichain
  , normalize [(.str 0, .absent, none), (.str 1, .absent, none)]
      (.unsealed .arrayKey none) .maybe false []                      -- 17 absent survives
  , normalize [(.str 0, .optional, sint 1)] .sealed .maybe true [] ]  -- 18 non-empty, string opt

/-- Four shape facts (one nullable) and the neighbours the mixed-base discipline
must reject. -/
def shapeFacts : List Fact :=
  let sh := fun (i : Nat) (n : Bool) => Fact.shape (shapeSeeds.getD i plainArray) n
  [ sh 1 false, sh 5 false, sh 9 false, sh 9 true
  , .singleton (.arr 0), .singleton (.arr 2), .singleton .null, .singleton (.int 1)
  , .oneOf [.arr 0, .arr 1], .oneOf [.null, .arr 1], .oneOf [.null, .int 1]
  , .general .int false ]

private def descentArrays : List Val :=
  [.arr 6, .arr 7, .arr 8, .arr 9, .arr 10, .arr 11, .arr 12, .arr 13, .arr 14]

/-- Value sets that overflow `CAP` with arrays in them. -/
def descentSeeds : List (String × List Val) :=
  [ ("allarrays", descentArrays)
  , ("withnull", descentArrays ++ [.null])
  , ("mixed", descentArrays ++ [.int 1])
  , ("assorted", [.arr 0, .arr 1, .arr 2, .arr 3, .arr 4, .arr 5, .arr 6, .arr 7, .arr 8]) ]

def arrLit : Nat → String
  | 0 => "[]"      | 1 => "[0=>1]"  | 2 => "[0=>1,1=>2]" | 3 => "[0=>1,a=>2]"
  | 4 => "[1=>2]"  | 5 => "[a=>1]"  | 6 => "[a=>2]"      | 7 => "[a=>2,b=>1]"
  | 8 => "[a=>3]"  | 9 => "[a=>4]"  | 10 => "[a=>5]"     | 11 => "[a=>6]"
  | 12 => "[a=>7]" | 13 => "[a=>8]" | 14 => "[a=>9]"     | 15 => "[b=>1]"
  | _ => "?"

def arrRanks : List Nat := List.range 16

/-- The keys the S4 narrowing operators are exercised over: exactly the keys the
array atoms use, so every operator sees both a hit and a miss. -/
def opKeys : List Key := [.int 0, .int 1, .str 0, .str 1]

def renderBool (b : Bool) : String := if b then "true" else "false"

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
    "atom str#5 \"ABC\" " ++ renderPreds (vecPreds 5),
    "atom str#6 \"abc\" " ++ renderPreds (vecPreds 6),
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
  -- The shape section.
  let shapeArrLines := arrRanks.map fun r =>
    "shapearr arr#" ++ toString r ++ " " ++ arrLit r ++ " " ++
      (if arrayIsList (vecArrEntries r) then "list" else "nolist")
  let seeds := shapeSeeds
  let idx := List.range seeds.length
  let seedAt := fun i => seeds.getD i plainArray
  let shapeLines := idx.map fun i => "shape " ++ toString i ++ " => " ++ renderShape (seedAt i)
  let shapeAdmitsLines := idx.flatMap fun i => arrRanks.map fun r =>
    "shapeadmits " ++ toString i ++ " arr#" ++ toString r ++ " => " ++
      renderBool (Fact.shapeAdmits vecModel (seedAt i) (vecArrEntries r))
  let shapeJoinLines := idx.flatMap fun i => idx.map fun j =>
    "shapejoin " ++ toString i ++ " " ++ toString j ++ " => " ++
      renderShape (Fact.shapeJoin vecModel (seedAt i) (seedAt j))
  let shapeLiftLines := arrRanks.map fun r =>
    "shapelift arr#" ++ toString r ++ " => " ++ renderShape (Fact.lift vecModel (vecArrEntries r))
  let sfs := shapeFacts
  let fidx := List.range sfs.length
  let factAt := fun i => sfs.getD i (Fact.singleton .null)
  let shapeFactLines := fidx.map fun i =>
    "shapefact " ++ toString i ++ " => " ++ renderFact (factAt i)
  let shapeFactAdmitsLines := fidx.flatMap fun i =>
    (arrRanks.map fun r =>
      "shapefactadmits " ++ toString i ++ " arr#" ++ toString r ++ " => " ++
        renderBool ((factAt i).admits vecModel (.arr r)))
    ++ [ "shapefactadmits " ++ toString i ++ " null => " ++
           renderBool ((factAt i).admits vecModel .null),
         "shapefactadmits " ++ toString i ++ " int:1 => " ++
           renderBool ((factAt i).admits vecModel (.int 1)) ]
  let shapeFactJoinLines := fidx.flatMap fun i => fidx.map fun j =>
    "shapefactjoin " ++ toString i ++ " " ++ toString j ++ " => " ++
      renderOptFact (Fact.join vecModel (factAt i) (factAt j))
  let shapeQueryLines := sfs.flatMap fun f =>
    [ "shapetruthy " ++ renderFact f ++ " => " ++ renderCert (f.truthy vecModel),
      "shapeisnull " ++ renderFact f ++ " => " ++ renderCert f.isNull ]
  let shapeDescentLines := descentSeeds.map fun p =>
    "shapedescent " ++ p.1 ++ " => " ++ renderOptFact (Fact.fromVals vecModel p.2)
  -- Soundness tallies for the array stratum, checked exhaustively on both sides
  -- exactly as `assoc` is (REPORT.md).
  let joinTally : Nat × Nat := idx.foldl (fun acc i =>
    idx.foldl (fun acc j =>
      let a := seedAt i
      let b := seedAt j
      let joined := Fact.shapeJoin vecModel a b
      arrRanks.foldl (fun (acc : Nat × Nat) r =>
        let e := vecArrEntries r
        let covered := Fact.shapeAdmits vecModel a e || Fact.shapeAdmits vecModel b e
        (acc.1 + 1,
         if covered && !Fact.shapeAdmits vecModel joined e then acc.2 + 1 else acc.2)) acc) acc)
    (0, 0)
  let liftTally : Nat × Nat := arrRanks.foldl (fun (acc : Nat × Nat) r =>
    let e := vecArrEntries r
    (acc.1 + 1,
     if Fact.shapeAdmits vecModel (Fact.lift vecModel e) e then acc.2 else acc.2 + 1)) (0, 0)
  let descentTally : Nat × Nat := descentSeeds.foldl (fun (acc : Nat × Nat) p =>
    match Fact.fromVals vecModel p.2 with
    | none => acc
    | some f => p.2.foldl (fun (acc : Nat × Nat) v =>
        (acc.1 + 1, if f.admits vecModel v then acc.2 else acc.2 + 1)) acc) (0, 0)
  let probe : List Val := (arrRanks.map (fun r => Val.arr r)) ++ [.null, .int 1]
  let factJoinTally : Nat × Nat := sfs.foldl (fun acc a =>
    sfs.foldl (fun acc b =>
      let joined := Fact.join vecModel a b
      probe.foldl (fun (acc : Nat × Nat) v =>
        let covered := a.admits vecModel v || b.admits vecModel v
        let ok := match joined with
          | none => true          -- `none` is ⊤ and admits everything
          | some g => g.admits vecModel v
        (acc.1 + 1, if covered && !ok then acc.2 + 1 else acc.2)) acc) acc) (0, 0)
  -- ADR-0062 S4: `count_range` (the S3 Lean debt) and the narrowing operators.
  let shapeCountLines := idx.map fun i =>
    "shapecount " ++ toString i ++ " => " ++ renderRange (GShape.countRange (seedAt i))
  let shapePromoteLines := idx.flatMap fun i => opKeys.flatMap fun k =>
    [ "shapepromote " ++ toString i ++ " " ++ renderKey k ++ " isset => " ++
        renderShape (Fact.shapePromotePresent vecModel (seedAt i) k true),
      "shapepromote " ++ toString i ++ " " ++ renderKey k ++ " exists => " ++
        renderShape (Fact.shapePromotePresent vecModel (seedAt i) k false) ]
  let shapeAbsentLines := idx.flatMap fun i => opKeys.map fun k =>
    "shapeabsent " ++ toString i ++ " " ++ renderKey k ++ " => " ++
      renderShape (Fact.shapeMarkAbsent (seedAt i) k)
  let shapeNonEmptyLines := idx.map fun i =>
    "shapenonempty " ++ toString i ++ " => " ++ renderShape (Fact.shapeSetNonEmpty (seedAt i))
  let shapeIsListLines := idx.flatMap fun i =>
    [Certainty.yes, Certainty.no].map fun c =>
      "shapeislist " ++ toString i ++ " " ++ renderCert c ++ " => " ++
        renderShape (Fact.shapeSetIsList (seedAt i) c)
  -- ADR-0062 S5: cover recording (A-G8) and the discharge query (A-G11). The
  -- pairs are every two-element subset of `opKeys`, so each seed sees covers
  -- whose members it declares, half-declares and does not declare at all.
  let coverPairs : List (Key × Key) :=
    [(.int 0, .int 1), (.int 0, .str 0), (.int 0, .str 1),
     (.int 1, .str 0), (.int 1, .str 1), (.str 0, .str 1)]
  let coverFlavors : List CoverFlavor := [.isset, .keyExists]
  let renderFlavor : CoverFlavor → String :=
    fun f => match f with | .isset => "i" | .keyExists => "e"
  let shapeRecordCoverLines := idx.flatMap fun i => coverPairs.flatMap fun p =>
    coverFlavors.map fun fl =>
      "shaperecordcover " ++ toString i ++ " " ++ renderKey p.1 ++ " " ++ renderKey p.2 ++ " " ++
        renderFlavor fl ++ " => " ++ renderShape (GShape.recordCover (seedAt i) [p.1, p.2] fl)
  let shapeCoverProvesLines := idx.flatMap fun i => coverPairs.flatMap fun p =>
    coverFlavors.map fun fl =>
      let s := GShape.recordCover (seedAt i) [p.1, p.2] fl
      "shapecoverproves " ++ toString i ++ " " ++ renderKey p.1 ++ " " ++ renderKey p.2 ++ " " ++
        renderFlavor fl ++ " => " ++
        (match GShape.coverProves s p.2 [p.1] with
         | none => "-"
         | some g => renderFlavor g)
  -- The narrowing law, checked exhaustively: everything the receiver admits
  -- that satisfies the guard survives the operator.
  let narrowTally : Nat × Nat := idx.foldl (fun acc i =>
    let s := seedAt i
    arrRanks.foldl (fun acc r =>
      let e := vecArrEntries r
      if !Fact.shapeAdmits vecModel s e then acc else
      let base : Nat × Nat := opKeys.foldl (fun (acc : Nat × Nat) k =>
        match Fact.entryOf e k with
        | none =>
          (acc.1 + 1,
           if Fact.shapeAdmits vecModel (Fact.shapeMarkAbsent s k) e then acc.2 else acc.2 + 1)
        | some v =>
          let acc := (acc.1 + 1,
            if Fact.shapeAdmits vecModel (Fact.shapePromotePresent vecModel s k false) e
            then acc.2 else acc.2 + 1)
          if v = Val.null then acc else
          (acc.1 + 1,
           if Fact.shapeAdmits vecModel (Fact.shapePromotePresent vecModel s k true) e
           then acc.2 else acc.2 + 1)) acc
      let base := if e.isEmpty then base else
        (base.1 + 1,
         if Fact.shapeAdmits vecModel (Fact.shapeSetNonEmpty s) e then base.2 else base.2 + 1)
      let want := Certainty.ofBool (arrayIsList e)
      (base.1 + 1,
       if Fact.shapeAdmits vecModel (Fact.shapeSetIsList s want) e then base.2 else base.2 + 1))
      acc) (0, 0)
  -- `mark_absent`'s second law, the one `unset($x[k])` needs: the result admits
  -- `v \ {k}` for every `v` the receiver admits.
  let unsetTally : Nat × Nat := idx.foldl (fun acc i =>
    let s := seedAt i
    arrRanks.foldl (fun acc r =>
      let e := vecArrEntries r
      if !Fact.shapeAdmits vecModel s e then acc else
      opKeys.foldl (fun (acc : Nat × Nat) k =>
        (acc.1 + 1,
         if Fact.shapeAdmits vecModel (Fact.shapeMarkAbsent s k) (Fact.eraseKey e k)
         then acc.2 else acc.2 + 1)) acc) acc) (0, 0)
  -- `count_range` bounds every admitted array's entry count.
  let countTally : Nat × Nat := idx.foldl (fun acc i =>
    let s := seedAt i
    arrRanks.foldl (fun (acc : Nat × Nat) r =>
      let e := vecArrEntries r
      if !Fact.shapeAdmits vecModel s e then acc else
      (acc.1 + 1,
       if (GShape.countRange s).contains (e.length : Int) then acc.2 else acc.2 + 1)) acc) (0, 0)
  -- The S5 recording law: an array that satisfies the disjunction survives the
  -- recording (the cover narrows, and a narrowing may not lose a member).
  let coverTally : Nat × Nat := idx.foldl (fun acc i =>
    let s := seedAt i
    coverPairs.foldl (fun acc p =>
      arrRanks.foldl (fun acc r =>
        let e := vecArrEntries r
        if !Fact.shapeAdmits vecModel s e then acc else
        coverFlavors.foldl (fun (acc : Nat × Nat) fl =>
          let sat := [p.1, p.2].any (fun k =>
            match Fact.entryOf e k with
            | none => false
            | some v => match fl with
              | .isset => !(v == Val.null)
              | .keyExists => true)
          if !sat then acc else
          (acc.1 + 1,
           if Fact.shapeAdmits vecModel (GShape.recordCover s [p.1, p.2] fl) e
           then acc.2 else acc.2 + 1)) acc) acc) acc) (0, 0)
  -- The A-G11 discharge law: when `coverProves` answers, the key really IS
  -- present in every admitted array whose other member fell through — where
  -- "fell through" is absent-or-null for an isset-cover and (given the caller's
  -- non-nullable-slot check) absent for a keyExists-cover.
  let dischargeTally : Nat × Nat := idx.foldl (fun acc i =>
    let s := seedAt i
    coverPairs.foldl (fun acc p =>
      coverFlavors.foldl (fun acc fl =>
        let c := GShape.recordCover s [p.1, p.2] fl
        arrRanks.foldl (fun (acc : Nat × Nat) r =>
          let e := vecArrEntries r
          if !Fact.shapeAdmits vecModel c e then acc else
          match GShape.coverProves c p.2 [p.1] with
          | none => acc
          | some g =>
            let fellThrough := match Fact.entryOf e p.1 with
              | none => true
              | some v => match g with | .isset => v == Val.null | .keyExists => false
            if !fellThrough then acc else
            let ok := match Fact.entryOf e p.2 with
              | none => false
              | some v => match g with | .isset => !(v == Val.null) | .keyExists => true
            (acc.1 + 1, if ok then acc.2 else acc.2 + 1)) acc) acc) acc) (0, 0)
  let soundLines :=
    [ "shapejoinsound " ++ toString joinTally.1 ++ " " ++ toString joinTally.2,
      "shapeliftsound " ++ toString liftTally.1 ++ " " ++ toString liftTally.2,
      "shapedescentsound " ++ toString descentTally.1 ++ " " ++ toString descentTally.2,
      "shapefactjoinsound " ++ toString factJoinTally.1 ++ " " ++ toString factJoinTally.2,
      "shapenarrowsound " ++ toString narrowTally.1 ++ " " ++ toString narrowTally.2,
      "shapeunsetsound " ++ toString unsetTally.1 ++ " " ++ toString unsetTally.2,
      "shapecountsound " ++ toString countTally.1 ++ " " ++ toString countTally.2,
      "shapecoversound " ++ toString coverTally.1 ++ " " ++ toString coverTally.2,
      "shapedischargesound " ++ toString dischargeTally.1 ++ " " ++ toString dischargeTally.2 ]
  String.intercalate "\n"
    (header ++ ["#"] ++ atomLines ++ ["#", orderLine, "#"] ++
      admitsLines ++ truthyLines ++ isNullLines ++ satisfiesLines ++ intInLines ++
      joinLines ++ [assocLine] ++
      shapeArrLines ++ shapeLines ++ shapeAdmitsLines ++ shapeJoinLines ++ shapeLiftLines ++
      shapeFactLines ++ shapeFactAdmitsLines ++ shapeFactJoinLines ++ shapeQueryLines ++
      shapeDescentLines ++ shapeCountLines ++ shapePromoteLines ++ shapeAbsentLines ++
      shapeNonEmptyLines ++ shapeIsListLines ++
      shapeRecordCoverLines ++ shapeCoverProvesLines ++ soundLines) ++ "\n"

end Vectors
end SteinsDomain
