# Throw damming: the one effect that dies, and how it dies

The `throw` color is the only dammable effect; its semantics land together
with `try` structuring (ADR-0031 stage 2:
`Try { try_trace, catches, finally_trace }`).

1. **Absorption is a Certainty judgment**: `catch (FooException)` absorbs
   `throw<E>` iff `E <: FooException` resolves Yes through the project
   inheritance chain. On Maybe, the safe side **inverts by consumer**:
   envelope checking (`throw.undeclared`) stays silent (escape unproven —
   zero-FP), while exhaustiveness (`…?`) names the lower bound. This
   asymmetry is the design's load-bearing joint.
2. **Four sources of throw facts**: explicit `throw new X` (resolved
   class); the catalog plus *measured* fold results (ADR-0024's
   `{throw: class}` — `intdiv(1,0)` seeds `DivisionByZeroError`
   empirically); propagated callee sets under ADR-0007's checked
   accounting (Error/LogicException families never count against
   envelopes); and **rethrow**: `throw $e` of a catch parameter re-emits
   exactly the absorbed subset, while wrap-and-throw emits the new class.
3. `catch (Throwable)` absorbs hierarchically. **`finally` absorbs
   nothing**; its effects join; its control-overriding semantics
   (return-in-finally) are explicitly deferred.
4. **Liskov applies** (ADR-0033 standing rule): overrides and interface
   implementations may not declare or infer broader checked throws than
   the abstraction's `@throws`.
5. Unannotated functions are never envelope-checked (opt-in principle),
   but inferred throw sets always propagate — callers' checks and
   annotate need them.

Reserved IDs: `throw.undeclared` (checked escape past a written
`@throws`), `throw.impossible-catch` (a catch whose absorbable set is
provably empty — dead catch, policy layer).

## Amendment (2026-10-01): a `new` propagates its constructor's throws (issue #849)

**Status: PENDING ratification.** Designed autonomously under the owner's
standing delegation, as the throw-lane twin of issue #804's effect edge.

§2's sources of throw facts are silent on constructors, so `new C(...)`
contributed nothing: a constructor that throws, by `@throws` or by inference,
was invisible to the function that instantiates the class, and
`throw new X(...)` recorded `X` but not what `X`'s constructor throws.

1. **Every `new` is a propagation edge to the constructor it runs**, filtered
   through the guards around the `new` like any call's. A thrown `new` records
   both the thrown class (§2's first source) and that edge. An anonymous
   class's arguments are its enclosing frame's calls.
2. **One resolution for both lanes.** The edge resolves through the resolver
   issue #804's effect edge uses, so the two lanes cannot disagree about which
   constructor a `new` runs: `Foo`, `self` and `parent` exactly; `static` only
   in a final class or to a final constructor; an inherited project
   constructor is the one that runs; a chain the project holds end to end
   with no constructor contributes nothing. An anonymous class runs its
   parent's constructor when its body cannot bring one of its own.
3. **An engine constructor answers from the catalog's throw rows**
   (`method_throws`, the class-world twin of `builtin_throws`), and
   `parent::__construct(...)` into an engine class answers from the same row,
   so a project exception forwarding to the engine's constructor stays
   exhaustive. `new PDO(...)` throws `PDOException` whenever the connection
   fails; every engine exception constructs without throwing. An engine class
   without a row, and an unknown, dynamic or ambiguous class, taint
   exhaustiveness the way a dynamic call does, and report nothing.
4. **`DateTime`, `DateTimeImmutable` and `DateInterval` get no row.** They
   throw only for an argument that does not parse, so `new
   DateTimeImmutable()` never does, and the class they throw is `Exception`
   before PHP 8.3 and a `DateMalformed*Exception` from 8.3 on. An
   argument-blind row would manufacture a checked throw (ADR-0007) at sites
   that cannot raise it, the same reason `json_decode`'s `JsonException`
   stays unattached until flags are read. They taint until a row can read
   the argument.
5. **`PDOException` joins the frozen throw tree** under `RuntimeException`.
   The projection stays frozen (ADR-0043 §5) except for a class a curated row
   raises, so the `PDO` row's throw is judged checked and a
   `catch (RuntimeException)` provably absorbs it.

Measured on the public corpora with `--no-cache` on PHP 8.5: the default
profile moves nothing; `strict` gains seven `throw.undeclared` and loses none,
and nothing outside `throw.*` moves. Five are true positives: composer's
`PlatformRepository` constructor throwing `UnexpectedValueException` into
`RequireCommand::execute()` (two throw sites), and symfony/console's
`TerminalInputHelper` constructor throwing the global `RuntimeException` into
three `QuestionHelper` methods that declare the package's own
`RuntimeException`. One has the shape of nine findings already in composer's
baseline: `Silencer::call`'s rethrown `Exception`, now reaching
`Factory::createComposer()` through `new HttpDownloader(...)`. One is false:
phpunit's `catch (PHPUnit\Runner\Exception)` around `new PhptTestCase(...)`
absorbs the constructor's `CodeCoverageFileExistsException`, which implements
that interface, but `throw_subtype` walks `extends` only and judges the catch
`No`. That defect predates this amendment; the constructor edge only gives it
one more path.

## Amendment (2026-10-02): the throw lane reads sites, and a missing row is not throwless — PENDING ratification

ADR-0099 (issue #865). §2's four sources of throw facts are read off one site
record that the effect lane reads too, so the two lanes cannot disagree about
what runs at a site. A known builtin with no throw row is a coverage gap,
never throwless; a throw row may be omitted only where php-src shows the name
raises nothing for the arguments its types admit, argument checking aside, and
each such name is audited and witnessed (issue #864, ADR-0099 §3.3). An
argument or operand that may reach user code, and `eval` or an inclusion,
make the throw set non-exhaustive, because unknown user code may throw
anything.
