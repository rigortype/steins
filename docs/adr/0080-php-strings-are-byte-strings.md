# A PHP string value carries bytes, not a lossy UTF-8 decoding

Issue #208 (audit), #187 (the first instance). Status: PENDING ratification
(autonomous design under the owner's post-hoc-ratification mode, per the
ADR-0063/0067/0076/0077/0079 precedent). Context: ADR-0002 (the zero-FP proof
layer), ADR-0003 (the syntax-tree contract Steins owns), ADR-0035 (the
four-layer value domain), ADR-0062 (array semantics and key normalization),
ADR-0066 (the replay fold transport).

## 1. The defect

A PHP string is a byte string. It has no encoding, `"\xC0"` is the one-byte
value `0xC0`, and `"\xC0" === "\xD0"` is `false`. Steins lowered every string
literal through `String::from_utf8_lossy` (`lower_literal`'s `Literal::String`
arm, via `bytes_to_string`), so every byte that is not valid UTF-8 became one
U+FFFD. Distinct PHP values arrived in the value IR as the **same**
`ArgValue::Str`, and Rust's `String` equality — which the whole analyzer uses
as PHP value equality — answered `true` for them.

This is not a rendering blemish. Equality on lowered string values is a proof
premise: it decides array-key identity (ADR-0062), constant folds, `===` value
facts, offset in/absence, and match/switch arm reachability. Three wrong
answers were confirmed against `steins check` at `3787cd8`, each with the PHP
runtime as the oracle:

1. **A manufactured false positive.** In `$s = $c ? "\xC0" : "\xD0";` the
   ternary join collapses `OneOf` to `Singleton("\u{FFFD}")` (`Fact::from_vals`
   sorts and dedups), after which `php_identical` folds **both** of `$s ===
   "\xD0"` and `$s === "\xC0"` to `Certainty::Yes`. The analyzer walks the
   null-assigning branch and the dereference as one proven path and reports
   `call.on-null` — *"proven Error"* — on code where no path dereferences null.
   A proof-layer finding on a state the program cannot reach is exactly what
   ADR-0002 forbids.
2. **A false negative.** `$a = ["\xC0" => 1]; $b = $a["\xD0"];` is silent;
   PHP warns `Undefined array key`. The ASCII control (`['a'=>1]`, read `'b'`)
   reports `offset.missing` as designed. `array_has_key` compared collapsed
   keys.
3. **Wrong constants.** `strlen("\xC0")` folded to `3` (PHP: `1`) because the
   lossy string is what crosses the fold wire and U+FFFD is three bytes;
   `count(["\xC0" => 1, "\xD0" => 2])` folded to `1` (PHP: `2`) because
   `normalize_array_with`'s last-wins fold dropped an entry whose key only
   *appeared* to repeat.

Issue #187 met instance (3)'s key-collapse in the `array.duplicate-key` scan
(`corpus/symfony__console/Helper/QuestionHelper.php:356`) and fixed it with a
guard that skips U+FFFD-bearing keys **in that one scan**. That guard was, until
this ADR, the only one in the workspace, and it sat on the *reporting* lane
while its proof-lane sibling 180 lines away (`normalize_array_with`) stayed
unguarded. The audit in #208 enumerates ~30 further unguarded consumers across
four crates. The lesson is not that the guard was wrong; it is that a
per-site guard cannot be the mechanism, because the obligation falls on every
future consumer and this repo already demonstrated it does not generalize.

## 2. Decisions

1. **`PhpStr` is the type of a lowered PHP string value.** A byte string with a
   two-arm representation, `Utf8(String)` and `Bytes(Vec<u8>)`, under the
   canonical invariant **`Bytes` if and only if the bytes are not valid
   UTF-8**. Canonicality means one representation per value, so equality is
   exact and the overwhelmingly common ASCII/UTF-8 path keeps a `String` with
   no allocation change and no per-comparison cost.
2. **Equality, ordering and hashing are byte-exact and hand-written over
   `as_bytes()`**, not derived over the enum. Derived impls would order every
   `Utf8` before every `Bytes` and would silently depend on the canonical
   invariant for correctness; going through the bytes is byte-lexicographic,
   is the order a reader expects, and stays correct even if a future
   constructor forgets to canonicalize. The domain's existing note stands —
   this order is *representational* (set semantics for `Fact`), and PHP-level
   `==`/`===` continue to live in the condition evaluator.
3. **`PhpStr` lives in `steins-domain`, and `steins-syntax` gains a dependency
   on it.** `steins-domain` is the value vocabulary and carries no `steins-*`
   dependency of its own, so the edge is acyclic and adds no build surface;
   a PHP string value is domain vocabulary by definition. The considered
   alternative — a new leaf crate holding one type, to keep `steins-syntax`
   free of `steins-*` edges — buys architectural purity at the price of a
   workspace member per vocabulary type, and is refused. This is the ADR-0003
   contract's first admission that the *value IR* is domain-typed while the
   CST behind it stays Mago's.
4. **Every lowered-string carrier changes payload together**: `ArgValue::Str`,
   `ArrayKey::Str`, `NormKey::Str`, `ConcatVal::Str` in `steins-syntax`;
   `Val::Str` and `Key::Str` in `steins-domain`; `VKey::Str` and the shape
   field key in the offset/shape lanes; `ContractTy::LitStr` in
   `steins-contract`. Partial migration is not an option — a boundary where
   `PhpStr` degrades back to `String` reintroduces the collapse at that
   boundary, which is the defect.
5. **Name lanes keep `&str` and decline on non-UTF-8.** Class names, function
   and method names, effect labels, include paths and index keys are looked up
   in `String`-keyed maps. They read the value through `as_str() -> Option<&str>`
   and answer `None` — silence, never a guess — when the value is `Bytes`.
   A byte-string class name resolves to nothing, which is the sound direction
   and matches what PHP would do with it in practice.
6. **The fold lane declines a non-UTF-8 argument** at `arg_to_fold_within`
   rather than shipping it. `FoldArg::Str`/`FoldKey::Str` and the sidecar wire
   stay `String` in this step, so `steins-sidecar` and the PHP runner are
   untouched; `strlen("\xC0")` stops folding to the wrong `3` and folds to
   nothing. Making it fold to the correct `1` is §3.1, a strict improvement on
   top, not a prerequisite for soundness.
7. **A diagnostic spells a non-UTF-8 string in PHP's own escape notation** —
   `"\xC0"`, byte by byte for the invalid bytes — instead of printing U+FFFD.
   The rendering lane was never load-bearing, but a message naming `'�'` when
   the source says `"\xC0"` is a message the reader cannot act on.
8. **The #187 U+FFFD guard retires with this change.** It exists only to
   compensate for the collapse; once keys compare by bytes, a genuine
   `"\u{FFFD}"` literal duplicated in one array literal is a true positive
   again, and the accepted silence it bought is repaid. The corpus pin it
   produced (`the_symfony_console_shape_is_silent`) is re-derived from the
   real byte keys: those four keys are genuinely distinct, so the site stays
   silent for the right reason, and the test is rewritten to assert that
   reason.
9. **Interim palliatives are refused.** Lowering an invalid-UTF-8 literal to an
   unknown value would kill the false positive in a handful of lines, but its
   correct form needs a marker that is provably a string and provably
   non-numeric (otherwise an unknown array key poisons the auto-increment
   counter and regresses the #187 non-poisoning invariant) — which is the same
   plumbing as this ADR, paid twice, with every true positive on byte strings
   surrendered in between.

## 3. Deferred with design

1. **The fold wire (`FoldArg::Str`).** JSON strings cannot carry arbitrary
   bytes, so folding a byte string needs an explicit encoding — a tagged
   variant carrying base64, with the PHP runner reconstructing exact bytes
   before it calls the function, and the answer travelling back the same way.
   That restores `strlen("\xC0") === 1` and every other byte-level builtin on
   the `PORTABLE` allowlist (`substr`, `strrev`, `str_pad`, `md5`,
   `base64_encode`, `htmlspecialchars`). It changes the ADR-0024 protocol and
   the ADR-0066 replay transport, so it is its own change with its own
   version handshake. Until then, decision §2.6 holds and the lane is silent.

   *Annotation (2026-08-14, ADR-0028's array-results amendment):* the version
   handshake is **not** required, and budgeting for it overstates this work.
   `runner.php` ships embedded in the binary (`include_str!`), the browser
   executes that same text unmodified, and ADR-0066's table is built live per
   session rather than recorded and shipped — so an old encoder never meets a
   new decoder. The tagged envelope is its own discriminator, and the array
   result decoder is already shaped so a bytes tag is a sibling inside it
   rather than a new wire. What remains of this item is the encoding itself.
2. **The read-time file decode.** Source files are read with
   `String::from_utf8_lossy` before parsing, so a *physically* non-UTF-8 file
   (as opposed to an ASCII file spelling `"\xC0"` as an escape, which is what
   the parser decodes to a raw byte and what all three confirmed fixtures use)
   collapses one layer earlier, outside this ADR's reach. Moving the source
   text to bytes touches every span, position and edit surface — the LSP
   offsets, the ADR-0034 splice contract, the fix-it byte ranges — and is a
   separate decision. Recorded here so the boundary is explicit: this ADR makes
   the *value lane* byte-exact; the *source lane* remains UTF-8-lossy, and a
   file that is not valid UTF-8 keeps its existing behaviour.
3. **Salsa backdating.** `SourceTree` derives `Eq`, and two file revisions
   differing only in invalid bytes inside a string literal currently produce
   equal trees, so downstream queries are backdated and findings go stale after
   a meaning-changing edit. This is the same root cause reaching the
   incremental engine (ADR-0009) rather than the value lane; it is fixed by
   §3.2, not by this ADR.

## 4. Consequences

- Every equality consumer enumerated in #208 — `php_identical`, `php_str_eq`,
  the array `===`/`==` legs, `walk_match` and `refine_match_arm`,
  `Refine::Exact`, `Fact::from_vals`, `array_has_key`, `ShapeFact::field` and
  its dedup, `guard_key`, `refine_fact_for_in_array`, `exclude_member`, the
  fold and `BindingKey` memo keys — is fixed **by construction**, at once, and
  no future consumer can reintroduce the defect by forgetting a guard. That
  property, not the line count, is why the representation moves.
- The migration is wide and shallow: roughly 160 construction and
  pattern-match sites in non-test code, compiler-guided, with the interesting
  work concentrated in three places — the lowering constructor, the `as_str()`
  decline sites in the name lanes, and rendering.
- True positives return that the collapse was suppressing: the `offset.missing`
  in §1.2 fires, byte-string array literals keep their real length, and
  byte-string keys participate in duplicate detection honestly.
- Findings disappear that were never earned: the §1.1 `call.on-null` and any
  other verdict standing on a forged string identity. The fp-gate corpus must
  be re-swept and its delta triaged as part of the change, not after it.
- `strlen` and its allowlist siblings go quiet on byte-string arguments until
  §3.1 lands. This trades a wrong constant for silence, which is the ADR-0002
  direction, and `not-implemented.md` records it as known imprecision.
- One new inter-crate edge (`steins-syntax → steins-domain`) and one new public
  vocabulary type. The wasm surface is unaffected — `steins-domain` has no
  platform dependencies.
