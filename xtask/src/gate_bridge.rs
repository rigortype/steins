//! TEMPORARY (issue #775): the fp-gate tables exactly as `gate.rs` held them
//! at master 55c31202, moved here unchanged so a test can hold the TOML files
//! under `xtask/fp-gate/` to them row for row. The next commit removes it.

/// Permanent gate policy for `phpdoc.*` findings (ADR-0030 relation #1).
///
/// `phpdoc.*` findings are **contract-layer** claims: they say a proven value does
/// not inhabit a *declared* `@param`/`@return` type under the no-coercion contract
/// relation. That is a statement about the code's own documentation, **not** a
/// runtime-breakage claim (`type.*`/`effect.*`, which gate red on sight per
/// ADR-0013). TRUE `phpdoc.*` findings legitimately exist in released, working
/// corpus code — a `@param int` that a test calls with the numeric string `"5"` is
/// a real declared-contract violation even though it runs fine — so they must
/// never flip the gate red merely by existing.
///
/// Instead the gate tracks their **count per package** against this deliberately
/// hand-maintained expected-count table and acts as a **regression tripwire**: a
/// package goes red only if its `phpdoc.*` count *increases* beyond the seeded
/// expectation (a genuine new finding, or a real regression in the checker),
/// while a *decrease* is a welcome improvement that never blocks. Update an entry
/// here consciously when a change to the checker legitimately moves a count.
///
/// Seeded with the post-assertion-exemption counts (the assertion-helper exemption
/// removed ~19 monorepo findings vs. the pre-exemption 352). Packages absent from
/// this table expect **zero** `phpdoc.*` findings.
///
/// **This table counts the `phpdoc.*` CONTRACT ids only** — see [`is_phpdoc`]. The
/// docblock-hygiene ids added by ADR-0078 / issue #186 share the prefix but carry
/// the mechanics layer, so they stay red-on-sight and are pinned individually in
/// [`EXPECTED_PROOF_FINDINGS`]; no count here moved when they landed.
/// `phpstan/phpstan-src` remains absent (measured 0 under the 2026-08-08 corpus
/// scoping recorded on its `THROW_EXPECTED` row).
///
/// **Unmoved by issue #603 (2026-09-11), and the zero is worth the paragraph.**
/// The enforced-top return hint (ADR-0057 A9) widens a *Verified* envelope —
/// a bare `: array`/`: object`/`: iterable` now bounds the call — into every
/// caller in the corpus, which is exactly the shape of change this table exists
/// to catch. Measured cold+warm over all ten packages: **not one row moved**, in
/// either direction, in `phpdoc.*`, `throw.*`, `effect.*`, the possibly-grade
/// strict floor, or `EXPECTED_PROOF_FINDINGS` — the finding sets are byte-identical
/// to master `965a9b5`'s, 219 rows each.
///
/// That zero is **measured, not entailed**. The widening can move this table in
/// both directions: a newly-bound `array` can silence a row that premised on a
/// wider fact, and it can *create* one — `$x = $o->all();` (`all(): array`, no
/// docblock) handed to a `@param string $s` fires `phpdoc.param-mismatch` on the
/// branch where master, holding `$x` as unknown, said nothing (a true positive,
/// the envelope being Verified). Neither shape occurs in the public corpus: every
/// `phpdoc.*` row here premises on a scalar-or-`false` union, and no bare
/// `: array` return reaches a docblock-typed parameter. So 219 = 219 is the
/// corpus's arithmetic, and a package that does either will move its row.
///
/// Run on PHP **8.5.10**, where CI calibrates on 8.4 — the divergence that buys
/// (phpunit at 82 against its seeded 88, the four builtin `T|false` rows the 8.5
/// sidecar declines; see this table's phpunit entry) reproduced **identically on
/// master and on the branch**, so it is the engine's, not the slice's.
const PHPDOC_EXPECTED: &[(&str, usize)] = &[
    // 19 → 21 (+2) with issue #327 (an array literal keeps its fact when its
    // elements do not). `ArtifactRepositoryTest` lines 45 and 68 build
    // `['type' => 'artifact', 'url' => __DIR__ . '/Fixtures/artifacts']` and pass
    // it to `ArtifactRepository::__construct(@param array{url: string})`. The
    // undeclared key `type` under that sealed shape is a TRUE contract violation
    // — the docblock omits it — and it is the SAME finding the same file's line
    // 79 already carried on the baseline, where the array is written with a
    // literal url. Those two sites were silent only because `__DIR__ . '…'` is
    // an unproven element, which used to drop the whole argument's fact along
    // with the keys; now the keys survive it and the judgment happens.
    //
    // Triaged verbatim, not reseeded blind: the unknown `url` slot is NOT what
    // fires. Measured on a fixture — `['url' => <unknown>]` against the same
    // `array{url: string}` stays SILENT (an unknown slot is `Maybe`, and Maybe
    // is silence), while the extra key fires with the slot proven or not.
    // The issue #391 wave (2026-08-16), `phpdoc.maybe-argument-mismatch`: a
    // builtin whose declared return is `T|false`/`T|null` handed straight into a
    // native `T` with no check in between — the argument side's possibly grade on
    // an `Asserted` premise (the declared-return floor is Asserted by ADR-0069, so
    // this whole family lands on the contract id, never on `type.*`). Every one is
    // TRUE against its source, and the shape is one line of PHP each. Counted here
    // rather than in POSSIBLY_EXPECTED because the id is `Layer::Contract`, which
    // is what routes it to this bucket (ADR-0081 §8: the layer decides the bucket,
    // the floor decides the surface).
    //   21 → 24 (+3): `file_get_contents()` into `new JsonManipulator(string
    //   $contents)` (RequireCommand.php:597) and into `stripWhitespace(string
    //   $source)` (Compiler.php:227); `inet_pton()` into `ipMapTo6(string
    //   $binary)` (NoProxyPattern.php:246).
    //
    // The issue #423 wave (2026-08-17, ADR-0056 §9) is the same judgment reaching
    // two places it could not reach before, and it moves six packages. Both halves
    // land on the CONTRACT id for the same reason the #391 wave did — the premise
    // is the ADR-0069 declared-return floor, which is `Asserted` — so none of this
    // touches the proof layer, and the corpus-wide `diagnostics` column stayed 0
    // through the whole wave.
    //
    //   (a) **A builtin callee is judged now.** `strlen($maybeFalse)` used to be
    //       silent because a builtin's parameters had no type source; they have
    //       one (the sidecar's own `getParameters()`), so a `T|false` handed to a
    //       builtin's native `T` reads exactly as it already read at a project
    //       callee.
    //   (b) **A builtin call written directly in argument position carries a
    //       premise now.** `f(realpath($p))` reached none, while
    //       `$r = realpath($p); f($r)` reached one off the very same rungs — one
    //       call written two ways answering differently. The Call carrier now
    //       reads the builtin ladder in the assignment path's own order.
    //
    // Every row below was read against its source line; not one is a guarded site.
    //
    //   24 → 31 (+7), all shape (b) except the last: `realpath()` into
    //   `Filesystem::normalizePath(string $path)` (AutoloadGenerator.php:217, 218)
    //   and into `findShortestPathCode(string $from/$to)` (224, 225);
    //   `file_get_contents()` into `Locker::__construct(string
    //   $composerFileContents)` (Factory.php:428) and into
    //   `JsonFile::parseJson(?string $json)` (JsonLoader.php:44); `json_encode()`
    //   into `new JsonManipulator(string $contents)`
    //   (Test/Json/JsonManipulatorTest.php:3231).
    //
    // The issue #537 wave (2026-08-27) is the same judgment at the RETURN seam,
    // and it moves exactly one package. All three rows are `Asserted` (the arms
    // come from `Platform::getEnv()`'s `@return string|false`), so they land on
    // the contract id and the proof half stays at zero corpus-wide.
    //
    //   31 → 34 (+3), one shape three times, in `src/Composer/Factory.php`:
    //   `$x = Platform::getEnv(…); if ($x) { return $x; }` inside `getHomeDir()`
    //   (:62), `getCacheDir()` (:103) and `getDataDir()` (:148), each declaring
    //   `: string`. **FALSE at the site, and all three for one missing narrowing**
    //   — a bare truthiness guard on a `T|false` arm lane subtracts nothing, so
    //   the `false` arm the `if` just excluded is still standing at the `return`.
    //   The argument side carries the identical gap (`if ($h) { needString($h); }`
    //   reports `phpdoc.maybe-argument-mismatch` on a fixture today); it simply has
    //   no public-corpus site, so the return seam is where the corpus first shows
    //   it. Seeded rather than suppressed, on ADR-0081 §8's posture for this
    //   family: the narrowing repair is its own slice, exactly as the four repairs
    //   issue #391's measurement forced were (§A6), and the count comes back down
    //   when it lands.
    //
    //   34 → 31 (-3), 2026-08-27 with issue #557: it landed, and the count came
    //   back down. `Subtrahend::Falsy` gives the truthiness guard its own
    //   subtrahend, so all three `Factory.php` rows are consumed by the `if ($x)`
    //   that always excluded them. This row is back to what it was before the
    //   #537 wave, and the seeds were the only thing that wave moved anywhere in
    //   the public corpus — no other package's count changed in either direction.
    //
    // The issue #589 wave (2026-09-01, the cross-lane guard join): a `T|false`
    // fact that used to be ERASED at the guard join now survives it, so the
    // possibly-grade pair can finally read sites the erasure had been hiding.
    // The join itself asserts nothing new — each restored claim is a branch's
    // own carrier — so every row below is a pre-existing judgment reaching its
    // site for the first time. Read against source, one row at a time:
    //
    //   31 → 34 (+3):
    //   `Config/JsonConfigSource.php:405` (`phpdoc.maybe-argument-mismatch`) —
    //   TRUE: `$contents = file_get_contents(...)` in the file-exists branch
    //   flows into `new JsonManipulator(string $contents)` with the `false` arm
    //   unguarded on that path.
    //   `Util/Zip.php:52` (`phpdoc.maybe-return-mismatch`) — TRUE: `$content =
    //   stream_get_contents($stream)` guards the STREAM, not the read; the
    //   `false` arm reaches the `?string` return.
    //   `Json/JsonFile.php:298` (`phpdoc.maybe-return-mismatch`) — FALSE:
    //   `encode()` guards with `if (false === $json) { self::throwEncodeError(…) }`,
    //   but the helper is declared `: void` and throws on every path, so pruning
    //   the guarded branch needs the interprocedural always-throws discharge
    //   issue #599 records (leg 2). The row comes back down when that leg lands.
    //   Still 34 after issue #599 leg 1 landed (2026-09-01), by construction: leg
    //   1's premise is the callee's own NATIVE `: never`, and `throwEncodeError`
    //   declares `: void`. Leg 2 stays open with this row as its witness.
    //   34 → 35 (+1), 2026-09-10 (issue #650), the only corpus movement of the
    //   slice that walks `for`/`foreach`/`do`-`while` bodies — and it is the
    //   whole point of the slice: `PathRepository.php:175` is inside a `foreach`
    //   body, which no walk had ever entered. TRUE, and the same shape (b) as the
    //   `JsonLoader.php:44` row eight lines up: `$json = file_get_contents(…)` is
    //   `string|false` and goes straight into `JsonFile::parseJson(?string $json)`,
    //   whose `false` arm is a strict-mode `TypeError`. The `file_exists()` two
    //   lines above it is not a discharge — it proves the path exists, not that
    //   the read succeeds, and the two are separated by a real window.
    ("composer/composer", 35),
    //   8 → 12 (+4): `realpath()` into a `string` parameter four times — `new
    //   TestCase($filename)` twice in `Runner/Phpt/TestCaseTest.php`, `new
    //   PhptTestCase($filename)` in `ListTestIdsCommandTest.php`, and
    //   `ExcludeList::addDirectory($directory)` in `ExcludeListTest.php`.
    //   12 → 51 (+39), 2026-08-17 (issue #423), every one shape (b) and every one
    //   the same two-line idiom repeated across the assertion suite: a fixture is
    //   read or encoded inline and handed straight to the assertion's `string`
    //   parameter. 22 are `file_get_contents()` into `Assert::assertStringEquals
    //   File*`/`assertStringNotEqualsFile*`/`assertXmlString*XmlFile`/
    //   `assertStringEqualsStringIgnoringLineEndings`; 5 are `json_encode()` into
    //   `assertJsonStringEqualsJsonFile`/`assertJsonStringNotEqualsJsonFile`/`new
    //   JsonMatches`; 11 are `realpath()` into `Issue::from(string $file)` /
    //   `Reader::read(string $baselineFile)` across the `Runner/Baseline` tests;
    //   1 is `ini_get()` into `assertStringStartsWith(string $string)`
    //   (TextUI/PhpHandlerTest.php:41). All TRUE at the possibly grade: the false
    //   arm of each of those four builtins is real, and a test that never sees it
    //   is a path claim this grade deliberately does not make.
    //   51 → 55 measured on the gate's OWN engine (CI pins PHP 8.4): phpunit's
    //   composer.json pins `config.platform.php` at 8.4.1, so an 8.4 runtime is
    //   admitted as a witness and the sidecar answers `builtin_param_types`,
    //   while an 8.5 runtime is outside the declared target and the builtin arm
    //   declines (issue #28's posture — a runtime the project does not ship on
    //   proves nothing). The four extra rows are all builtin CALLEES with a
    //   builtin `T|false` argument, one shape and TRUE at the possibly grade:
    //   `file_get_contents()` into `json_decode()` (build/scripts/phar-manifest.php)
    //   and into `preg_match_all()` (Framework/Assert/FunctionsTest.php ×2),
    //   `getmypid()` into `posix_kill()` (end-to-end/_files/…/InterruptTest.php).
    //   A local 8.5 run therefore reads 51 here and stays under the tripwire; the
    //   seeded count is the CI engine's, which is the one the gate is calibrated on.
    //   55 → 57 (+2), 2026-09-01 with issue #589 (see the composer entry for the
    //   wave): `Util/PHP/JobRunner.php:244`, `$stdout` and `$stderr` into `new
    //   Result(string …, string …)` (`phpdoc.maybe-argument-mismatch` ×2) — both
    //   FALSE: the code guards with `assert($stdout !== false)`, but excluding
    //   `false` from an abstract `Union{string, bool}` has no value-lane spelling
    //   (`Refinement` carries Str/Int only), so the bool arm survives the assert.
    //   Issue #600 records the domain gap; both rows come back down with it.
    //   57 → 88 (+31), 2026-09-09 with issue #472 (type-alias resolution). One
    //   defect, thirty-one call sites: `src/TextUI/Configuration/Value/Source.php`
    //   declares `@phpstan-type DeprecationTriggers array{functions:
    //   list<non-empty-string>, methods: list<non-empty-string>,
    //   ignoreUndefinedTriggers: bool}` and `Source::__construct(@param
    //   DeprecationTriggers $deprecationTriggers)`, while every construction in
    //   `tests/` passes `['functions' => [], 'methods' => []]` — the required
    //   third key is absent, under a sealed shape. TRUE, and never checked by
    //   anything before: phpunit's own `phpstan.neon` reads `paths: - src`, so
    //   upstream PHPStan resolves this alias and never looks at the call sites,
    //   and Steins read the alias as a bare class name until this issue. The
    //   code's own reader is `deprecationTriggers()['ignoreUndefinedTriggers'] ??
    //   false` (TextUI/Application.php:893), so the honest repair is `?` on the
    //   alias key rather than a third key at 31 call sites — either way the
    //   declaration as written is violated. Finding-level diff against the
    //   same-day `fee437c` baseline: exactly these rows, nothing removed.
    //   As with the 51 → 55 row, the seeded count is the CI engine's (8.4); a
    //   local 8.5 run reads 84 and stays under the tripwire.
    //   88 → 86 (-2), 2026-09-10 with issue #600 and ADR-0093 §2: the two
    //   `Util/PHP/JobRunner.php:244` rows seeded above come back down. The union
    //   arm over `bool` now carries its literal member set, so `assert($stdout
    //   !== false)` subtracts in the value lane and the guarded argument reaches
    //   `new Result(string …, string …)` with no rejected arm left to report.
    //   Measured finding-level on a local 8.5 run against the branch point: 84 → 82,
    //   and the two rows that leave are exactly these two, byte-identical
    //   otherwise, with no other package moving in either direction. Both counts
    //   are the local engine's; the seeded 88 → 86 carries the same -2 to the
    //   CI 8.4 engine this line is calibrated on, whose four extra rows (the
    //   51 → 55 entry above) are elsewhere in the package.
    //   The mechanism is pinned independently of the corpus in
    //   `crates/steins-infer/tests/it/bool_literal_narrowing.rs`
    //   (`a_guarded_false_arm_no_longer_reaches_a_string_parameter`).
    //   86 → 88 (+2), 2026-09-11 with issue #637 (ADR-0070's mined certification):
    //   `tests/end-to-end/regression/5884/tests/FooTest.php:53` and `:78`,
    //   `phpdoc.maybe-argument-mismatch` on `chmod($filename, …)`. `$filename =
    //   tempnam(…)` is `non-falsy-string|false`; `file_put_contents($filename,
    //   'foo')` in the guard used to invalidate it as an uncertified callee, and
    //   `file_put_contents` declares no reference parameter, so the excuse was
    //   never true — the fact now survives into `chmod(string $filename)` under
    //   `strict_types=1`, where the `false` arm is a real `TypeError`. Shape (b),
    //   the same as the `JobRunner.php:244` rows above. TRUE. Seen on the CI 8.4
    //   engine only: the local 8.5 A/B for this slice reported no phpdoc.*
    //   movement, so this row is calibrated where the line is.
    //   88 → 104 (+16), 2026-09-12 with issue #607 (a declared float reaches the
    //   value lane): sixteen `phpdoc.maybe-argument-mismatch` rows in
    //   `tests/unit/Framework/Assert/assert{File,Directory,}Is*Test.php`, all
    //   one shape — `chmod($path, octdec('0'))` / `mkdir($path, octdec('0'))`
    //   under `strict_types=1`. `octdec` declares `int|float` (the float arm is
    //   the over-`PHP_INT_MAX` return) and the lowering used to drop any union
    //   carrying a float, so the claim never reached `int $permissions`. Same
    //   family as the `nikic/PHP-Parser` 17 → 20 rows below and TRUE at the
    //   possibly grade for the same reason; here the closure — a one-digit
    //   literal — sits in the argument itself, so the honest fix is folding
    //   `octdec`/`hexdec`/`bindec` over a literal (issue #736), which would
    //   answer `0` and retire these rows, not a wider refusal. Seen on the CI
    //   8.4 engine; the local 8.5 A/B for this slice reported +4 rows elsewhere
    //   and none here.
    ("sebastianbergmann/phpunit", 104),
    // 0 → 4 (+4), 2026-08-17 (issue #423), all shape (a) — the tempnam idiom:
    // `$certFile` / `$tmpfname` carry `non-falsy-string|false` and go straight
    // into `rename(string $from)` (Handler/CurlFactoryTest.php:4031, 4045, 4061)
    // and `unlink(string $filename)` (Handler/StreamHandlerTest.php:807). The
    // builtin sink is the only new part; the argument's type was already read.
    //   4 → 5 (+1), 2026-09-11 (issue #641), the whole corpus's only movement under
    //   the narrowed offset-write barrier — nothing was lost anywhere, and the
    //   proof-layer `diagnostics` column stayed 0. `CurlFactoryTest.php:4417`:
    //   `addDecodeResponse(): string` opens `$content = \gzencode('test');`
    //   (`string|false`, PHP's own documented signature), then writes
    //   `$headers['Content-Encoding'] = 'gzip';` inside an `if`, then
    //   `return $content;`. The write is to a DIFFERENT local, in a file with no
    //   `&` anywhere; it used to erase `$content` along with the rest of the scope,
    //   so the declared return had no premise to judge. It has one now, and the
    //   judgment is TRUE: the file declares `strict_types=1`, so the `false` arm is
    //   a real `TypeError`. Read verbatim, not reseeded blind — it is the same
    //   `T|false`-into-native-`T` shape as the four rows above it, arriving on the
    //   return side rather than the argument side.
    ("guzzle/guzzle", 5),
    // 4 → 5 (+1), 2026-08-14, with ADR-0056 §8: `resource` stopped being an
    // unmodeled spelling and became a relation. `StreamHandlerTest`'s
    // `testWriteMissingResource` constructs `new StreamHandler(null)` against
    // `@param resource|string $stream` and wraps it in
    // `expectException(\LogicException::class)` — the test exists precisely
    // because the value is invalid. `null` inhabits neither arm, so it is a TRUE
    // no-coercion violation; it was silent only because `resource` lowered to an
    // opaque `Maybe` that swallowed the union's verdict. The exact shape the
    // flysystem and symfony/console entries below already record: a deliberate
    // negative-test call site the contract layer can now read.
    // 5 → 6 (+1), 2026-08-17 (issue #423), shape (a): `json_encode()` straight
    // into `substr(string $string, …)` (Formatter/JsonFormatterTest.php:238).
    // `json_encode` really does answer `false` on malformed UTF-8, and `substr`
    // under `strict_types` really does fatal on it.
    // 6 → 7 (+1), 2026-09-01 with issue #589 (see the composer entry for the
    // wave): `Utils.php:140` (`phpdoc.maybe-return-mismatch`) — FALSE: the
    // `false` arm is guarded by `if ($json === false) { self::throwEncodeError(…) }`,
    // and `throwEncodeError` is declared `: never`, but a `: never` callee does
    // not prune its branch the way a plain `throw` does. Issue #599 (leg 1)
    // records the gap; the row comes back down when it lands.
    // 7 → 6 (-1), 2026-09-01 with issue #599 leg 1: it landed, and the row came
    // back down. `throwEncodeError(int $code, $data): never` is a `private
    // static` method, so `self::` dispatch resolves it, and the branch now
    // terminates the way the plain-`throw` spelling always did — the guard
    // subtracts, and `$json` reaches `return` as `string`. Nothing else in this
    // package moved in either direction.
    // 6 → 7 (+1), 2026-09-09 with issue #472's multi-line alias bodies. TRUE, and
    // the test says so itself: `PHPConsoleHandlerTest::testWrongOptionsThrowsException`
    // sets `expectException` and calls `new PHPConsoleHandler(['xxx' => 1])`, where
    // the constructor's `@phpstan-param InputOptions $options` names a sealed
    // 20-key optional shape that has no `xxx`. Nothing could see it before: the
    // `@phpstan-type InputOptions array{…}` declaring that shape is wrapped across
    // 22 physical lines, so reading a body off one line left it unresolvable, and
    // reading the *first* line off would have been worse than silence.
    ("Seldaek/monolog", 7),
    // 1 → 2 (+1) with ADR-0043 stage 4 (phpdoc-side class contracts). The new
    // finding is a class-value contract: `new MountManager(['valid' => 'something
    // else'])` — a plain string in the `array<string, FilesystemOperator>` value
    // position — inside a `guarding_against_mounting_invalid_filesystems` test that
    // wraps it in `expectException(UnableToMountFilesystem::class)` and carries
    // `@phpstan-ignore-next-line`. A TRUE no-coercion violation the test documents.
    // 2 → 3 (+1), 2026-08-16, issue #391: `file_get_contents()` into
    // `computeFingerPrint(string $publicKey)` (SftpConnectionProviderTest.php:189).
    // 3 → 4 (+1), 2026-09-09 with issue #472 (type-alias resolution):
    // `AdapterTestUtilities/ToxiproxyManagement.php:62`. The file declares
    // `@phpstan-type Attributes array{latency?: int, jitter?: int, rate?: int,
    // delay?: int}` and `@phpstan-type Toxic array{…, attributes: Attributes}`,
    // and `resetPeerOnRequest()` — the method directly above `addToxic(@param
    // Toxic $configuration)` — builds `'attributes' => ['timeout' => $ms]`.
    // `timeout` is not a key `Attributes` names, and the shape is sealed: a TRUE
    // disagreement between the alias and the code beside it. It needs the
    // one-level alias-names-alias rule to be visible at all, since `Toxic`'s body
    // names `Attributes`. Finding-level diff against the same-day `fee437c`
    // baseline: exactly this row.
    ("thephpleague/flysystem", 4),
    // 0 → 1 (+1) with ADR-0043 stage 4. `ChoiceQuestionTest` passes a literal array
    // `[..., null]` to `ChoiceQuestion::__construct(@param array<string|bool|int|
    // float|\Stringable> $choices)`; `null` is a member of none of the union arms —
    // a TRUE no-coercion contract violation (the docblock omits null). The sibling
    // `StringChoice` (a `__toString` object, implicit `\Stringable`) is correctly
    // *accepted*, not a finding — the is-a oracle honors the implicit interface.
    // 1 → 2 (+1), 2026-08-14, with ADR-0056 §8 — the monolog entry above's twin,
    // and the pair is the point: `StreamOutputTest` line 45 passes the literal
    // `"foo"` to `StreamOutput::__construct(@param resource $stream)` in a test
    // whose whole purpose is to assert the constructor rejects a non-resource.
    // A string is not a resource in any PHP mode, so TRUE.
    //
    // Two packages, two findings, and NOTHING else across 100,530 files: the
    // resource-VALUE half of the slice (a narrowed `fopen()` handle reaching a
    // typed parameter) fires nowhere in the corpus at all. That is the expected
    // shape rather than a disappointment — legacy PHP that still uses resources
    // is legacy PHP that does not type its parameters — and it is the soundness
    // signal too, since a wrong producer row would have lit up the well-typed
    // OSS packages first.
    // 2 → 4 (+2), 2026-09-14, with `int-mask`/`int-mask-of` becoming a relation.
    // Both are `testInvalidModes`, and both wrap the call in
    // `expectException(\InvalidArgumentException::class)`:
    // `InputArgumentTest.php:51` passes `-1` to `@param
    // int-mask-of<InputArgument::*>|null $mode` (flags 1, 2, 4), and
    // `InputOptionTest.php:114` passes the string `'-1'` to `@param
    // int-mask-of<InputOption::*>|null $mode` (flags 1, 2, 4, 8, 16). Neither
    // value is any combination of the class's constants, and `'-1'` is not an
    // int at all, so both are TRUE. The two constant sets are complete — every
    // class constant in both classes is an int literal and neither class has a
    // parent — which is what the wildcard resolver requires before it reads one.
    ("symfony/console", 4),
    // 0 → 15 (+15) with ADR-0043 stage 4. Every finding is a deliberate
    // negative-test call site (`expectException(\LogicException::class)` /
    // `\PhpParser\...`) passing a wrong-typed argument to a class-typed `@param`:
    // `new Name()` vs `(string|Identifier|Expr)` (Name is-a-No either), scalar `1`
    // /`"test"` vs `(Node|Builder)` / `(string|Identifier)`, `new stdClass()` vs a
    // `\UnitEnum`-bearing union. All in `test/PhpParser/Builder*Test.php` and
    // `NodeDumperTest.php`; each asserts the runtime `LogicException` that the
    // phpdoc contract predicts — TRUE, released, working test code.
    // 15 → 16 (+1), 2026-08-16, issue #391: `json_encode()` into
    // `JsonDecoder::decode(string $json)` (JsonDecoderTest.php:21). Its sibling in
    // this package — `preg_replace()`'s `string|null` into `indentString(string
    // $str)` — carries an all-`Verified` premise and so lands on the proof id, in
    // POSSIBLY_EXPECTED below, not here. One package, one judgment, two buckets:
    // the stratum split, visible in the gate.
    // 16 → 17 (+1), 2026-08-17 (issue #423), shape (a): `array_splice($this->
    // visitors, $index, 1, [])` in `NodeTraverser.php:56`, where `$index` is the
    // `int|string` key of a `foreach` over a declared array. `array_splice`'s
    // `$offset` is a native `int`, so the string arm fatals — TRUE at the
    // possibly grade, and contract-layer because the `int|string` is a docblock's
    // claim about the array's keys rather than anything PHP enforces.
    // 17 → 20 (+3), 2026-09-12 (issue #607): `hexdec()`/`bindec()` declare
    // `int|float` — the float arm is what an argument wider than `PHP_INT_MAX`
    // returns — and the contract lowering used to refuse any union carrying a
    // float, so the whole claim was dropped and the three call sites premised
    // nothing. `Int_::fromString` hands `hexdec($str)` and `bindec($str)` to
    // `Int_::__construct(int $value)` (`Int_.php:54`, `:59`) and the string
    // unescaper hands `hexdec(...)` to `chr(int $codepoint)`
    // (`String_.php:120`), all under `strict_types=1`, where a float argument is
    // a `TypeError`. TRUE at the possibly grade and no higher: each is closed by
    // something outside the call — PHP's own lexer emits a DNUMBER for an
    // overflowing literal, and the unescaper's regex admits at most two hex
    // digits — and neither closure is visible here. The `u` branch four lines
    // below `String_.php:120` guards the same arm by hand
    // (`\is_int($dec) ? $dec : \PHP_INT_MAX`), which is the author agreeing the
    // arm is real in general.
    ("nikic/PHP-Parser", 20),
    // 0 → 9 (+9), 2026-08-17 (issue #423), all shape (a). Two in `src/`:
    // `preg_split('/[_.-]+/', $completeLocale)` with `$completeLocale` a
    // `string|false` (AbstractTranslator.php:353), and `array_splice($arguments,
    // key($timezoneParameters), …)` where `key()` is `int|string|null` against a
    // native `int $offset` (Factory.php:757). Seven in `tests/`: four
    // `date('Y-m-d', strtotime(…))` (`int|false` into `?int`, CreateTest.php:268,
    // 273, 278, 283), two `json_decode(json_encode(Carbon::now()))`
    // (JsonSerializationTest.php:27 in both the Carbon and CarbonImmutable
    // suites), and `trim(file_get_contents(…))`
    // (CarbonInterval/ConstructTest.php:563). Each is one line of PHP and each
    // false/null arm is real.
    ("briannesbitt/Carbon", 9),
    // The private monorepo (corpus.local.toml); matched by its local project name.
    //
    // Ledger of every move, oldest first. Standing conditions unless a row says
    // otherwise: proof layer 0, every OSS package unchanged (the soundness signal —
    // a wrong checker change lights up well-typed OSS first), `throw.*` and
    // possibly-grade at their own baselines, PHPStan reporting the identical class.
    // A decrease never gates; it is adopted consciously and recorded so the next
    // reader knows the cause.
    //
    //   333 → 357  ADR-0031 branch-sensitive analysis: values previously buried in
    //              `Opaque` control flow reach the contract layer.
    //   357 → 404  ADR-0035 refined layer — native-type seeding, guard refinements,
    //              `@phpstan-assert` application. 8 abstract-fact findings, the rest
    //              concrete; all TRUE no-coercion violations in released test code.
    //              Class-shaped `@param`s stay silent against scalar facts (template
    //              safety), so no template FPs.
    //   404 → 405  ADR-0036 object state: the first `phpdoc.property-mismatch` — an
    //              int literal assigned to a `@var numeric-string` property. Property
    //              checks run only in the plain per-scope pass, never under a binding
    //              descent, whose caller values in-body guards would narrow.
    //   405 → 439  ADR-0043 stage 4 (class contracts + the enum-case/class-const
    //              resolution feeding them). 36 added, 2 pre-existing FPs removed, all
    //              34 net TRUE: class-const numeric strings into `int`/`int[]` (the
    //              ADR-0037 DB-illusion pattern); proven scalars/objects against
    //              class-typed contracts; sealed-shape violations that became provable
    //              once their const/`::class`/enum elements resolved.
    //   439 → 434  2026-07-24, LIVE-TREE DRIFT rather than a checker change — the
    //              unpinned checkout gained ~210 files during the day.
    //   434 → 477  ADR-0056 R1 builtin return facts: a uniquely-resolved builtin seeds
    //              its reflected return envelope (`trim()` ⇒ `General{String}`), which
    //              then reaches a `@param int`. All 43 triaged, every one the
    //              stringly-typed request-param → int-param pattern; two render
    //              `non-empty-string`, the envelope composing with an existing
    //              `=== ''` guard.
    //   477 → 487  2026-07-29, DR2 `is_*` guard narrowing (ADR-0064 seam v): a fact now
    //              SURVIVES a pure guard instead of being forgotten. All 10 the same
    //              idiom — `is_numeric` proves numeric-STRING-ness, not int-ness. One
    //              proof-layer FP the slice introduced (a refuted fact left standing on
    //              an unreachable branch) was caught by this gate and fixed in-slice.
    //   487 → 497  2026-08-02, ADR-0072 shape facts judged against contracts. 10/10
    //              TRUE, one class: a sealed `array{…}` `@param` under-declaring keys
    //              its call sites provably always pass.
    //   497 → 498  2026-08-02, ADR-0073 inline `@var` cast seeding (PR #121) — the same
    //              sealed-shape class reached by a new path.
    //   498 → 499  2026-08-05, CORPUS STATE rather than an engine change, attributed
    //              two ways instead of by triage: every public package is exact in the
    //              same run, and the gate re-run at the commit that seeded 498
    //              reproduces 499 against today's checkout. NOT triaged
    //              finding-by-finding — that needs the previous corpus state, which
    //              nobody retained, and which the `revision` record now closes.
    //   499 → 500  2026-08-05, ADR-0077 out-parameter seeding (PR #152): a capture
    //              group read after a guard that proved the match carries `string` into
    //              a `@param int`. The group being a digit class is what makes the
    //              annotation look plausible and is exactly no defence — PCRE hands
    //              back a string whatever matched.
    //   500 → 507  **Reseed 2026-08-09**, corpus pin moving with it (`565b106a…` →
    //              `5b026671…`, the two-file discipline). Re-measuring at the seeded
    //              revision was impossible (9.8 GB checkout, 2.1 GB free), so the
    //              accounting is indirect and recorded rather than implied: of the 316
    //              files carrying findings, 5 moved between the seeded and measured
    //              revisions and carry 14 findings — a channel wide enough to account
    //              for +7 several times over, not a proof that it did. Each
    //              analyzer-side movement was measured back-to-back at one checkout
    //              (issue #272 alone removed 4 FPs and added 3 verified debts, 508 →
    //              507). The gate had printed RED here for two sessions, which is the
    //              failure mode a stale baseline actually has: a standing red trains
    //              the reader to skim past the next real regression.
    //   507 → 513  2026-08-09, issue #288 — a project call's declared RETURN shape now
    //              seeds the caller's value lane, the mirror of ADR-0062 S3's `@param`
    //              seeding. 6/6 TRUE by reading both docblocks, all the sealed-shape
    //              under-declaration class: a 3-key HTTP wrapper shape into a 2-key
    //              `@param`; a list-of-records into a one-record `@param` (3 sites, one
    //              callee); an 11-key options builder into a 10-key post-filter (2
    //              sites), short exactly the key the caller always passes.
    //   513 → 528  issue #293, template bounds read as upper-bound contracts. Measured
    //              on the branch rebased onto the #288 merge, so the two waves are
    //              disjoint by measurement rather than assumption. 12 sites pass lists
    //              holding `null` or ints to a `@template T of list<string>` assertion
    //              helper; 2 pass the wrong shape to its `list<int>` sister; 1 is a
    //              genuine defect rather than a loose annotation — a constant asserted
    //              as string membership at one site and int at its sibling — predicted
    //              to take 2 findings with it when fixed. Seeded at what the corpus
    //              says today, not at what it should say.
    //   528 → 526  2026-08-09, and the corpus owner did this, not the analyzer: the
    //              predicted fix landed and took exactly its two findings. The
    //              prediction landing on the nose is why this was a one-line edit
    //              rather than a re-triage.
    //   526 → 536  2026-08-15, two mechanisms rather than one. Measured as a
    //              finding-level diff: the 526-seed commit (86df4c6), re-run today
    //              against the same checkout, still reports exactly 526, so the delta
    //              is the analyzer's. Twelve rows are new; two are the same two sites
    //              re-rendered (`mixed` -> `int|string` inside a shape), which is what
    //              makes twelve a +10.
    //              · 6 sites — an undeclared key under a sealed shape, the class the
    //                `composer/composer` entry records, from the same array-literal
    //                work (issue #327: a literal keeps its fact when its elements do
    //                not). A two-key literal into a one-optional-key `@phpstan-param`
    //                (2); an extra key into a six-name `@param` (1); a list of records
    //                into a `@param string[]` (3), at three more sites of a call whose
    //                fourth was already inside the 526.
    //              · 4 sites — a `string|bool` with no carrier until ADR-0085's
    //                `Fact::Union`: one base per fact meant the argument fell back to
    //                an unjudgeable `mixed`, and Maybe is silence. The value is a
    //                `string` at runtime, so this is the numeric-string archetype this
    //                table's own doc names. The two re-rendered rows are the same
    //                mechanism moving a spelling and not a count.
    //              Both families' fallout predates the reseed: `corpus.local.toml` is
    //              gitignored, so the agent worktrees that landed #303, #327 and #341
    //              measured the public packages only — a hole in the workflow, not in
    //              the analyzer. See [`EFFECT_EXPECTED`]'s row.
    //   536 → 539  2026-08-16, ADR-0057 T1 (issue #378): a static factory now rebinds
    //              an exact receiver, so three method calls that resolved to nothing
    //              before are judged against their `@param`. All three are the same
    //              shape and all three are true positives: a test deliberately hands
    //              `false` to a `@param string` / `@param (int|string)` method to
    //              exercise the callee's own assertion (the corpus marks each with a
    //              `@phpstan-ignore-next-line`), and the third row is that `false`
    //              flowing on inside the callee's descent to a private helper's
    //              `@param string`. Measured as a finding-level diff against the
    //              same-day baseline run: exactly these three rows are new, nothing
    //              moved elsewhere. Seeded at what the corpus says today.
    //   539 → 543  2026-08-16, the value IR carrying method calls (issue #386): an
    //              array literal with a method-call element no longer collapses to
    //              an unrepresentable `Other`, so four shape literals now meet the
    //              sealed `array{…}` they are declared against — two `@return`
    //              shapes and one `@param` shape at two call sites — and each
    //              carries a key the declaration does not name. The same
    //              undeclared-key-under-a-sealed-shape family the 526 → 536 row
    //              triaged, and true positives by the same PHPStan reading (a
    //              sealed shape admits no extra key). Finding-level diff against
    //              the #385 run: exactly these four rows; one `throw.*` row on
    //              phpstan-src (local) went away (21 → 20), a removal the tripwire
    //              does not gate on.
    //   543 → 551  2026-08-16, issue #391's `phpdoc.maybe-argument-mismatch` (the
    //              Asserted-premise half of the possibly-grade argument pair,
    //              strict floor, counted here because it is `Layer::Contract`).
    //              Eight rows, one shape: a builtin's `string|false` /
    //              `string|bool` / `int|null` (`file_get_contents`, `tempnam`,
    //              `realpath`, a `T|false` return read through a docblock) handed
    //              straight into a native `string`/`int` parameter under
    //              `strict_types` — the same shape the public 9 carry. Finding-level
    //              diff against the #388 run: exactly these eight rows plus the
    //              possibly-grade rows below; no other id moved (repair A's
    //              `assert(... instanceof)` widening added nothing here).
    //   551 → 553  2026-08-17, issue #423 (builtin parameter types): two
    //              `phpdoc.maybe-argument-mismatch` rows, one shape — a
    //              `tempnam()`-family `non-falsy-string|false` handed straight to
    //              `file_put_contents()` / `unlink()`'s native `string` under
    //              `strict_types`, the builtin twin of the shape #391 seeded. Both
    //              read against the source: neither is guarded. Finding-level diff
    //              against the same-day master run: exactly these two rows and the
    //              one proof-layer row baselined in `EXPECTED_PROOF_FINDINGS`;
    //              nothing else moved.
    //   553 → 617  2026-09-11, issue #472 (type aliases resolve) landing in #664.
    //              Sixty-four rows, one mechanism: a multi-line `@phpstan-type`
    //              shape. Before #472 an alias name reached `lower_identifier`'s
    //              class catch-all and was held silent by `Cx::is_known_class`'s
    //              valve; now the body is the contract, so every `@param <Alias>`
    //              in the declaring class-like is judged as the shape the author
    //              spelled, and a literal that does not inhabit it is reported.
    //              Two rows triaged against their source and both TRUE — an
    //              undeclared key under a sealed `array{…}` alias body, the family
    //              this table's `composer/composer` entry already carries — and
    //              the other sixty-two are the same shape, same id, same kind of
    //              site. They are described rather than listed: issue #670's brief
    //              scopes an individual pass over them as separate work, and
    //              nothing here depends on which of the sixty-two are true.
    //
    //              Measured as a **finding-level A/B** between two runs over the
    //              same checkout, not by running the private gate: issue #658
    //              records that `cargo xtask fp-gate` does not complete on the
    //              larger private corpus, so the gate's own number for this
    //              package is not what moved the row. The count is the diff of the
    //              two finding sets; the reseed is what keeps the tripwire from
    //              reading #472's intended, triaged effect as a regression.
    ("pxxxx-monorepo", 617),
];

/// The expected `phpdoc.*` count for a package/local-project name (0 if untabled).
fn phpdoc_expected(name: &str) -> usize {
    PHPDOC_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for `throw.*` findings (ADR-0040/0007), identical in
/// spirit to [`PHPDOC_EXPECTED`]: an undeclared **checked** throw escaping a
/// written `@throws`, or a Liskov-widened override, is a contract-layer claim
/// about the code's own documentation, not a runtime-breakage proof. Such
/// findings saturate working code (the checked-exception volume ADR-0007 keeps
/// quiet by default), so they are held in measurement mode and gate only as a
/// per-package **increase** tripwire. Packages absent expect **zero**.
///
/// Seeded from the first landing run of the throw system (ADR-0040); the monorepo
/// count is dominated by two pervasive base exceptions thrown far below
/// `@throws`-annotated controllers. It rose 35614 → 43963 with the closure wave
/// (ADR-0033), which propagates throws through higher-order-builtin callbacks and
/// body-local `$fn()` closures that were previously opaque taints — triaged
/// 5-sample, every one a TRUE undeclared checked throw through a real callback
/// edge, with the by-ref-invalidation guard keeping `$fn()` resolution sound.
// Reconciled to actual after closure-wave Stage D (interface/parent `@throws`
// Liskov + `implements` lowering): the increases are new `throw.liskov-widened`
// (phpunit +4, pxxxx +1 — an override declaring a narrower exception than the
// abstraction it implements), and the decreases (symfony/console 12→10, nikic
// 2→1) are `undeclared` counts that fell because lowering `implements` enriched
// the class chain, letting subtype/absorption checks resolve where they widened.
const THROW_EXPECTED: &[(&str, usize)] = &[
    ("composer/composer", 93),
    ("sebastianbergmann/phpunit", 84),
    ("guzzle/guzzle", 2),
    ("Seldaek/monolog", 7),
    ("symfony/console", 10),
    ("thephpleague/flysystem", 3),
    ("nikic/PHP-Parser", 1),
    // Registered 2026-07-24 (v0.1.0 run, oracle idea A): PHPStan's own `src/` as a
    // local project. First run 0 proof-layer / 0 phpdoc.* / 20 throw.undeclared,
    // triaged verbatim — homogeneous checked-exception debt (a `ShouldNotHappen`
    // escaping a narrower `@throws`), the exact ADR-0040/0007 pattern tripwire mode
    // exists for.
    //   20 → 21  2026-07-26, the checkout advanced (192 src/ files). One rewritten
    //            extension grew a second `ShouldNotHappenException` escaping a caller
    //            declaring something else. Triaged against the pre-advance tree: that
    //            file held one before and two now, the other 20 unchanged in file,
    //            line and escaping method — a transitive catch, which is what the
    //            damming machinery is for.
    //   2026-08-08 (#186): scope moved from an `exclude` denylist to the positive
    //            `paths = ["src", "vendor"]` key — PHPStan's `tests/` is that
    //            project's own rule-fixture corpus, INPUTS written to be broken, so
    //            outside a gate whose bar is zero FPs on working code (ADR-0079 §2.3
    //            applied to a whole tree). Re-measured unchanged at 21 / 0, because
    //            the denylist already named the same directories. The key buys
    //            enforcement: an allowlist cannot silently readmit a fixture tree
    //            added later, a denylist can.
    ("phpstan/phpstan-src", 21),
    // Ledger. Every move on this row has been corpus state, not a checker change,
    // and the standing class is the same: an `@throws`-annotated declaration with an
    // undeclared base-exception escape, arriving with new application code. The
    // proof layer stayed 0 across all of them.
    //   43964 → 44372  2026-07-24, live-tree drift (~210 files gained: 84,038 →
    //                  84,248). 3-sample verbatim triage, all TRUE.
    //   44372 → 44343  2026-08-01, the standing DOWNWARD drift, reseeded in its own
    //                  pass (never inside a fix commit). Observed unchanged across
    //                  every session since 2026-07-25 — cross-commit stability plus
    //                  the #63 determinism fix rule out a checker cause.
    //   44343 → 44374  2026-08-05, the corpus-state movement `PHPDOC_EXPECTED`
    //                  records in the same run, established the same two ways (every
    //                  public count exact; the gate at the 44343-seed commit
    //                  reproduces 44374 today). NOT triaged finding-by-finding — at
    //                  this volume the attribution is the only honest evidence, and a
    //                  finding-level diff needs a corpus state nobody retained.
    //   44374 → 43886  **Reseed 2026-08-09, downward**, with the corpus pin, and this
    //                  one has a direct attribution: the measured revision replaces a
    //                  bespoke exception class with concrete assertions across the
    //                  tree, and an assertion where a `throw` used to be is exactly
    //                  one `throw.undeclared` fewer. A baseline 488 above the truth
    //                  would swallow the next 488 real regressions in silence.
    //   43886 → 44112  2026-08-19, **two components, measured separately** so that
    //                  neither hides behind the other:
    //                  * +206 is live-tree drift. Unmodified master measures 44092
    //                    at the corpus revision this run saw, against a baseline
    //                    seeded at an earlier one — established by running the gate
    //                    on master itself, not inferred. Not triaged
    //                    finding-by-finding; at this volume the attribution is the
    //                    only honest evidence, as the 2026-08-05 row already records.
    //                  * +20 is issue #433's `\UnhandledMatchError` origin, and this
    //                    half IS triaged finding-by-finding: 4 distinct sites,
    //                    multiplied to 20 by propagation to declaring callers. Every
    //                    one is a `default`-less `match` over a subject the engine
    //                    types wider than its docblock does — native `string` with a
    //                    `@phpstan-param 'asc'|'desc'`, and its kin. The docblock is
    //                    unenforced, so the throw is real, which is exactly the cell
    //                    ADR-0088 §5 gates on the Verified grade.
    ("pxxxx-monorepo", 44112),
];

/// The expected `throw.*` count for a package/local-project name (0 if untabled).
fn throw_expected(name: &str) -> usize {
    THROW_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for the `effect.*` **contract** ids
/// (`effect.envelope-exceeded`, `effect.liskov-widened`, and since issue #311
/// `effect.interop-unknown-label`), the ADR-0050 §9 recorded delta: these gate as
/// the same per-package **increase** tripwire as [`PHPDOC_EXPECTED`] /
/// [`THROW_EXPECTED`], matching their declared-contract semantics (a proven
/// behavior exceeds an envelope the code *declares* about itself; it still runs).
///
/// Seeded **empty** in 2026-08, on the reading that no ADR-0006 envelope exists in
/// the corpus. The reading was about *Steins* annotations: all three ids fire on
/// upstream's spellings too — `@phpstan-impure` since #311, and the envelope pair
/// since #303 taught the scanner `@phpstan-all-methods-pure` — so "no envelopes in
/// the corpus" stopped being true the day it was read for upstream's vocabulary.
const EFFECT_EXPECTED: &[(&str, usize)] = &[
    // **0 → 4442, 2026-08-15**, the first row in this table and it carries no Steins
    // annotation: the private monorepo declares `@phpstan-all-methods-pure` on three
    // classes, and #303 (master 7b3ecab, 2026-08-12) made that an operative bound.
    // Measured, not inferred — a binary at 93eff42 (the merge before #303) reports 0
    // over the 5041-file subtree holding all 4442; one at 7b3ecab reports 4442.
    //
    // NOT the resource-value run (#341), which the first reading blamed: findings
    // over that subtree are byte-identical on both sides of it (27,332 = 27,332),
    // and the gate re-run at 040658c — the commit whose record claims green at
    // 526/0 — reproduces 536/4442 today. `corpus.local.toml` is gitignored, so an
    // agent worktree measures public packages only; this was simply the first run
    // with the private corpus mounted since 2026-08-12.
    //
    // One root, one shape. Names below are **placeholders**, since the corpus's own
    // identifiers are not written into this repo — do not grep for them. Every
    // finding sits in one of the three declaring classes (`MyApp\Route\UrlBuilder`
    // 4376, `MyApp\Util\Validator` 54, `MyApp\Util\Filter` 12), across 795 call
    // sites in 555 declared-pure methods, and each reaches the same house logger
    // `MyApp\Log\Debug`: an assertion helper delegates to `Validator`, which
    // delegates to `Filter`, whose `true`-rejecting arm logs. One finding per
    // (label, origin) group, six for most sites — which is how 795 becomes 4442.
    //
    // TRUE, and the corpus says so in its own source: that logging call carries a
    // `@phpstan-ignore impure.methodCall`, so upstream reports the same violation at
    // the same line and suppresses it there. PHPStan then reads the callee as pure
    // for everyone above it; the proven lane does not (ADR-0067 — a declaration
    // neither manufactures nor erases a finding), so the effect travels to every
    // caller declaring purity over it.
    //
    // What the count is made of: 2148 `nondet.time` (a datetime utility wrapping
    // `\time`/`\date`), 716 `nondet.random` (`mt_rand`, the log's sampling gate),
    // 716 `global.read` (a `getenv`), 86 `io` (a `file_exists`), and 776
    // `io.output.buffer` — the one soft class, where `print_r($x, true)` /
    // `var_export($x, true)` are pure in return-mode and the source proves the
    // `true` at each of the three origins. That label is this catalog's
    // deliberately arg-blind row, which `effect_labels`' own doc already calls an
    // over-approximation; the fix is a sibling of `narrowed_stream_labels` and will
    // move this row DOWN, which never trips the tripwire.
    //
    // 2026-09-12, issue #352: that fix landed. `narrowed_output_labels` drops the
    // label at a call site that proves the `true`, so those 776 findings are gone
    // and this row should read roughly 3666. It is deliberately NOT reseeded here:
    // `corpus.local.toml` is gitignored, the agent worktree that landed the change
    // measures public packages only, and the seeding discipline for this row is a
    // count re-MEASURED rather than computed. Leaving 4442 standing is safe in the
    // meantime — the gate is an increase tripwire and a decrease never trips it —
    // and costs only this row's sharpness until someone with the corpus mounted
    // re-runs and reseeds.
    //
    // Seeding rather than fixing is the zero-FP posture, not a retreat from it: the
    // violations are real, and what makes 4442 of them is a house logger three hops
    // under an assertion helper — the shape ADR-0084's `[effects]` attribution
    // exists to discharge on the corpus owner's side. Until then this row's job is
    // to notice the 4443rd.
    ("pxxxx-monorepo", 4442),
];

/// The expected `effect.*`-contract count for a package/local-project name (0 if
/// untabled — the all-zero seed).
fn effect_expected(name: &str) -> usize {
    EFFECT_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// Permanent gate policy for the **possibly-grade** proof ids (ADR-0081 §8):
/// `variable.maybe-undefined`, `property.maybe-undefined` and
/// `type.return-maybe-missing`, selected by floor rather than by name (see
/// [`gate_bucket`]). Same per-package **increase** tripwire as
/// [`PHPDOC_EXPECTED`] / [`THROW_EXPECTED`] / [`EFFECT_EXPECTED`]; absent = zero.
///
/// A count and not per-finding pins, because a possibly-grade id claims only that
/// *a* path reaches the site — the shape defensive house styles produce on purpose,
/// and why the registry floors these at `strict`. Pinning each would have meant
/// hundreds of rows asserting that working code works. (A definite proof id claims
/// the program breaks, so its count belongs at zero with each exception triaged
/// into [`EXPECTED_PROOF_FINDINGS`].)
///
/// Seeded 2026-08-09 from the binding-presence run, at the revisions
/// `corpus.lock.toml` / `corpus.local.toml` record. The five classes that triage
/// found, so a reader knows what a count is made of:
///
/// 1. **Conditional binding** (`if`/`elseif` with no `else`) — TRUE, the id's point.
/// 2. **A loop variable read after its loop** — TRUE; the zero-iteration path
///    really does reach it unbound.
/// 3. **Correlated conditions** — bound under `if (count($c) > 1)` and read under a
///    textually identical one whose body reassigns `$c` between. TRUE under the
///    syntactic reading this id is defined over; proving the conditions agree is
///    path feasibility, which nothing here attempts.
/// 4. **A never-returning callee** (`$this->fail()`, `markTestSkipped()`) — FALSE.
///    `stmt_end` reads a statement-position call as falling through because
///    deciding otherwise needs the project index; ADR-0081 §9 defers the refinement
///    to the emitter side, where the index lives.
///
///    **Half of this class closed, 2026-09-01 (issue #599 leg 1), and the split is
///    worth naming**: one symptom, two seams. A `type.maybe-*` row is a WALK
///    finding, and the walk holds the index, so a statement-position call to a
///    resolved native `: never` callee terminates its branch there now and those
///    rows are gone. A `variable.maybe-undefined` row is not: its firing set is
///    `Scope::maybe_undefined_reads`, and `stmt_end` — quoted above — belongs to
///    the binding-presence pass, which runs at lowering and is index-free by
///    ADR-0081 §1. §9's deferral therefore still stands for exactly the rows it
///    was written about, and discharging it still means what §9 says it means:
///    publishing enough branch structure for `check_undefined_variables` to
///    re-subtract an arm after the fact.
/// 5. **A binding in an argument of a throwing call inside `try`** — FALSE. PHP
///    evaluates arguments before entering the callee, so the binding is done before
///    anything can throw; the pass weakens at statement granularity.
/// 6. **A builtin's `T|false`/`T|null` into a native `T`**
///    (`type.maybe-argument-mismatch`, issue #391) — TRUE, and the first class in
///    this bucket that is not a binding claim: the argument's own declared type
///    has an arm the parameter rejects. Only the all-`Verified` half lands here;
///    an `Asserted` arm routes the same judgment to `phpdoc.maybe-argument-mismatch`
///    and thus to [`PHPDOC_EXPECTED`].
const POSSIBLY_EXPECTED: &[(&str, usize)] = &[
    // 1 — `PluginManager.php:525`, class 1.
    // 1 → 3, 2026-09-11 with issue #619 (the declaration-only dispatch path,
    // ADR-0049 A16): two `type.maybe-argument-mismatch` rows at
    // `Downloader/DownloadManager.php:159` and `:161`. Both are TRUE at the
    // possibly grade and both are the amendment's own yield — the receiver is a
    // `PackageInterface $package` parameter, so dispatch refused it and the return
    // envelope was unreadable until now. `PackageInterface::getDistType()` and
    // `::getSourceType()` are declared `: ?string`; `DownloadManager::
    // getDownloader(string $type)` is not nullable and the file is
    // `strict_types=1`, so the null arm is a TypeError. The premise is a NATIVE
    // return hint on an interface declaration, all-`Verified` under PHP's
    // class-declaration-time covariance rule (A16), which is what routes it here
    // rather than to PHPDOC_EXPECTED. Whether the arm is inhabited on a live path
    // — composer sets `distType` whenever `installationSource` is `'dist'` — is
    // exactly what the possibly grade does not claim.
    ("composer/composer", 3),
    // 1 — `Application.php:409`, class 4 (`exitWithErrorMessage`).
    // 1 → 2, 2026-09-01 with issue #589 (the cross-lane guard join; see
    // PHPDOC_EXPECTED's composer entry for the wave): a second class-4 row in the
    // same file, `TextUI/Application.php:472` (`type.maybe-argument-mismatch`) —
    // FALSE: `$configurationFile`'s `false` arm is guarded by
    // `exitWithErrorMessage(): never` three lines up, and the `: never` callee
    // does not prune its branch, so the arm reaches `realpath(string $path)`.
    // The premise is all-`Verified` (a native declaration), which is what routes
    // it here rather than to PHPDOC_EXPECTED. Issue #599 records the pruning gap
    // (the class-4 deferral, ADR-0081 §9); both class-4 rows in this file come
    // back down when it lands.
    // 2 → 1 (-1), 2026-09-01 with issue #599 leg 1. Only ONE of the two came
    // down, and the split is the point: the class-4 taxonomy above names one
    // symptom sitting on TWO seams.
    //   `:472` clears. It is a walk finding, and the walk holds the project
    //   index, so a statement-position `$this->exitWithErrorMessage(…)` on a
    //   `private … : never` method now terminates its branch and the `false` arm
    //   never reaches `realpath()`.
    //   `:409` STAYS, and is expected to. `variable.maybe-undefined` fires off
    //   `Scope::maybe_undefined_reads`, computed by the binding-presence pass at
    //   LOWERING (`lower_presence.rs`), which is index-free by ADR-0081 §1 — its
    //   `stmt_end` still reads the `catch` arm's never call as falling through,
    //   so `$cliConfiguration` is `Maybe` at the `return`. §9's deferral is to
    //   the EMITTER side (`check_undefined_variables`), and discharging it needs
    //   the presence pass to publish enough branch structure for the checker to
    //   re-subtract an arm after the fact — a slice of its own, unchanged here.
    // The same seam split is why `symfony/process` and `briannesbitt/Carbon`
    // below do not move either; see their entries.
    // 1 → 3 (+2), 2026-09-11 with issue #637 (the mined by-value certification).
    // Both new rows are `variable.maybe-undefined` on `$tmpFile` in the vendored
    // `phar-utils/src/Linter.php` (`:66` and `:85`), and both are FALSE: the
    // variable is assigned inside `if ($isWindows = defined('PHP_WINDOWS_VERSION_BUILD'))`
    // and read inside `if ($isWindows)`, a branch correlation through a variable
    // that the binding-presence pass does not follow — the same class as the
    // `symfony/console` rows below, and unchanged by this issue.
    // What this issue changed is why they were SILENT. `check_undefined_variables`
    // subtracts every argument a call might be *writing* (`out_param_argument_spans`),
    // and an uncertified callee counts as "might be": `file_put_contents($tmpFile, …)`
    // at `:66` was excused as a possible out-parameter, which also BOUND `$tmpFile`
    // for everything downstream, taking `:85` with it. `file_put_contents(string
    // $filename, …)` has no reference parameter at `PINNED_PHP`, so the excuse was
    // never true; certifying the name removes it and the pre-existing correlation
    // FP surfaces at both reads. A false excuse hiding a false positive is not a
    // reason to keep either, and the proof layer is untouched (0 diagnostics).
    ("sebastianbergmann/phpunit", 3),
    // 10 — six class 1/2/3 in `Application.php`, `CompletionInput.php` and
    // `SymfonyStyle.php`; four class 5 in `Tests/`.
    // 10 → 11, 2026-09-11 with issue #619: one `type.maybe-argument-mismatch` at
    // `SingleCommandApplication.php:61`, a `$this->` receiver in an open class —
    // the commonest receiver in any corpus, and the one `resolve_guarded` refused
    // on the declaring class's own non-finality. `Command::getName(): ?string`
    // goes straight into `Application::setDefaultCommand(string $commandName)`.
    // TRUE at the possibly grade: the line above assigns a name through
    // `setName($_SERVER['argv'][0])`, so the null arm is closed at runtime by a
    // property write this analyzer does not track through `$this` — and the
    // author's own `$this->getName() ?: 'UNKNOWN'` five lines up says the arm was
    // considered real. Coercive mode, and `null` into a non-nullable parameter of a
    // USER function is a `TypeError` there too (the 8.1 deprecation is internal
    // functions only) — the possibly grade is the right one.
    ("symfony/console", 11),
    // 6 — every row class 4 (`$this->fail()` in `ProcessTest.php`).
    // Unmoved by issue #599 leg 1 (2026-09-01), and twice over. Every row is
    // `variable.maybe-undefined` on the `$e` of a `try { …; $this->fail(…); }
    // catch (X $e) {}` idiom, so it sits on the binding-presence seam leg 1 does
    // not reach (see the phpunit entry). And the callee would decline anyway:
    // the corpus checkout ships no `vendor/`, so `TestCase`'s chain leaves the
    // project and dispatch answers nothing — whatever the shipped PHPUnit
    // declares for `Assert::fail()` is not a fact this index holds.
    ("symfony/process", 6),
    // 2 — both class 4 (`markTestSkipped()` in the serialization tests).
    // 2 → 3, 2026-08-17 with issue #423 (ADR-0056 §9): the wave's one proof-layer
    // row, and the one that pays for the whole slice. `CarbonInterval::
    // createFromFormat(string $format, ?string $interval)` calls
    // `explode($match[1], $interval)` at `CarbonInterval.php:739` — a NATIVE
    // `?string` straight into `explode`'s native `string $string`, so the premise
    // is all-`Verified` and the id is `type.maybe-argument-mismatch`. The
    // normalization that would have saved it (`$interval ??= '';`) sits four lines
    // BELOW the call, inside a `preg_match` branch the null value reaches, so
    // `createFromFormat('H:i:s.v', null)` fatals on a released, strict-types
    // library. TRUE, unguarded, and reachable through the public API — exactly the
    // shape a builtin parameter surface exists to see.
    // Unmoved by issue #599 leg 1 (2026-09-01), for the two reasons the
    // `symfony/process` entry gives: the two class-4 rows are
    // `variable.maybe-undefined` on the binding-presence seam, and
    // `$this->markTestSkipped()` resolves to nothing anyway with no `vendor/` in
    // the checkout. The `CarbonInterval.php:739` row is TRUE and untouched.
    // 3 → 4 (+1), 2026-09-12 (issue #607): `type.maybe-return-mismatch` at
    // `CarbonTimeZone.php:60`. `getDateTimeZoneNameFromMixed(string|int|float
    // $timezone): string` reassigns `$timezone = preg_replace(…)` inside its
    // `is_string` branch, so `null` joins the union, and the bare `return
    // $timezone;` at the end is that union. The claim existed in the arm lane
    // before; what it lacked was a lowering — `arm_lane_premise` folds the arms
    // through `to_fact`, which refused any union with a float member, so the
    // premise was silently dropped. TRUE at the possibly grade on the **null**
    // arm: PCRE answers `null` on a pattern or backtrack failure, `is_numeric
    // (null)` is `false` so the guard above does not close it, and returning
    // `null` from a `: string` function under `strict_types=1` is a `TypeError`
    // — the same reasoning the `nikic` `preg_replace` row below already carries.
    // The message also names the `int` and `float` arms, which `is_numeric`
    // ought to have subtracted; that is a narrowing gap in the message's
    // explanation, not in its verdict, and it does not make the row false.
    ("briannesbitt/Carbon", 4),
    // 0 → 1, 2026-08-16 with issue #391 — and a sixth class, the first that is not
    // a binding claim at all: `type.maybe-argument-mismatch` on
    // `PrettyPrinter/Standard.php:1100`. `preg_replace()` declares `string|null`
    // (natively, so the premise is all-`Verified` and the finding is proof-layer),
    // and `$escaped` goes straight into `indentString(string $str)`. TRUE at the
    // possibly grade: PCRE answers `null` only on a pattern/backtrack failure, so
    // the arm is real and its inhabitation on a live path is exactly the part this
    // grade does not claim. The package's other issue #391 finding rides an
    // `Asserted` premise and is counted in PHPDOC_EXPECTED instead.
    ("nikic/PHP-Parser", 1),
    // 10 — 8 `variable.maybe-undefined` (classes 1 and 4) plus the 2
    // `type.return-maybe-missing` rows absorbed from `EXPECTED_PROOF_FINDINGS`.
    // 10 → 11, 2026-08-16 with issue #391: one `type.maybe-argument-mismatch` —
    // a `string|null` (a nullable native declaration) handed to a native `string`
    // parameter; the null arm is real, its inhabitation is what the grade does
    // not claim.
    ("phpstan/phpstan-src", 11),
    // 120 (111 `variable.maybe-undefined` over 85,282 files + 9 absorbed
    // `type.return-maybe-missing`) → 121, 2026-08-14 with issue #330 PR2:
    // `array_merge` joined the fold allowlist, so it stopped being an uncatalogued
    // name whose by-ref parameters might WRITE an argument, and an argument read
    // became a read (the issue #77 bind-free distinction). The row is a data
    // provider returning `array_merge($a, $b)` where `$b` is bound only inside a
    // `foreach` — the third sibling of two already-baselined rows of that shape in
    // the same function, not a new class of claim. TRUE at the possibly grade.
    // 121 → 131, 2026-08-16 with issue #391: ten `type.maybe-argument-mismatch`
    // rows on a Verified premise — a natively declared `string|null` / `int|null`
    // / `string|false` handed to a native `string`/`int` parameter under
    // `strict_types` (four sites are one helper called with the same nullable
    // local four times). Every pre-existing `variable.maybe-undefined` and
    // `type.return-maybe-missing` count is unchanged.
    ("pxxxx-monorepo", 131),
];

/// The expected possibly-grade count for a package/local-project name (0 if
/// untabled).
fn possibly_expected(name: &str) -> usize {
    POSSIBLY_EXPECTED.iter().find(|(n, _)| *n == name).map_or(0, |(_, c)| *c)
}

/// A triaged TRUE proof-layer positive the corpus legitimately contains: real
/// broken code Steins correctly proves. Unlike measurement-mode `phpdoc.*`/
/// `throw.*`, this is runtime-layer (standing bar: zero, ADR-0013), so an
/// entry is a recorded exception matched at finding precision (package + id +
/// path + line + message fingerprint) — any drift re-reds the gate.
struct ExpectedProofFinding {
    /// Package / local-project name the finding belongs to.
    package: &'static str,
    /// The diagnostic id (e.g. `type.argument-mismatch`).
    id: &'static str,
    /// A suffix of the finding's project-relative path.
    path_suffix: &'static str,
    /// The 1-based line.
    line: u32,
    /// A stable substring of the message (the acceptance fingerprint).
    message_contains: &'static str,
}

/// Triaged TRUE proof-layer positives (ADR-0043 §5). Each row's triage is in
/// the comment beside it; adding a row is a conscious, orchestrator-visible act.
const EXPECTED_PROOF_FINDINGS: &[ExpectedProofFinding] = &[
    // monolog's own test deliberately constructs `new MongoDBHandler(new
    // \stdClass, …)` and asserts the TypeError; `stdClass` is a mined root with
    // no supers (ADR-0043), so it is-a-NOs the `Client|Manager` union. TRUE.
    ExpectedProofFinding {
        package: "Seldaek/monolog",
        id: "type.argument-mismatch",
        path_suffix: "tests/Monolog/Handler/MongoDBHandlerTest.php",
        line: 27,
        // Source-cased since `TypeMember::Instance` grew its `display` field
        // (diagnostics render the declared casing; matching stays lowercased).
        message_contains: "cannot become MongoDB\\Client|MongoDB\\Driver\\Manager",
    },
    // ADR-0049 S2: `call.undefined-method` fired 10 times, all monorepo, all
    // static calls into a final/trait-free/fully-enumerated chain — genuine
    // `Error: Call to undefined method` fatals. TRUE, all 10. OSS packages:
    // zero. A DAO batch calls a legacy accessor removed/renamed from its
    // final DAO class.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "Batch/UnifyThumbnailSchemaBatch.php",
        line: 16,
        message_contains: "getLegacyArticleThumbnailArticleIds() — hierarchy fully enumerated",
    },
    // A sample test drifted out of sync with its sample class: `Sample_Common`
    // (`final`, methods `get`/`addData`/`swapData` only) is called with eight names
    // it never declares. Each `Sample_Common::x()` would fatal when the test runs —
    // exactly the ROADMAP gap-1 adoption case (a checker silent here is not adopted).
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 79,
        message_contains: "Sample_Common::getByHogeIds() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 105,
        message_contains: "Sample_Common::isPrime() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 134,
        message_contains: "Sample_Common::throwException() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 164,
        message_contains: "Sample_Common::getValuesFromExternalServer() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 179,
        message_contains: "Sample_Common::printToStandardOutput() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 192,
        // Method name omitted (it carries a private-corpus token); line 192 +
        // path + id keep this row 1:1.
        message_contains: "— hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 211,
        message_contains: "Sample_Common::setSampleCookie() — hierarchy fully enumerated",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "tests/SampleTest.php",
        line: 229,
        message_contains: "Sample_Common::hasCookie() — hierarchy fully enumerated",
    },
    // `OAuth2Model::checkPassword()` is called statically, but exists only as an
    // instance method on the caller `OAuth2ClientModel`; `OAuth2Model` is final
    // with no such method — genuine undefined-static-method fatal.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.undefined-method",
        path_suffix: "util/src/Model/Auth/OAuth2ClientModel.php",
        line: 106,
        message_contains: "OAuth2Model::checkPassword() — hierarchy fully enumerated",
    },
    // ADR-0049 S5: `call.too-few-arguments` fired twice on the monorepo, both
    // TRUE ArgumentCountErrors (two grouped-`use` `Query::__construct` FPs from
    // an unlowered import are fixed in the paired commit and no longer fire).
    // An admin mail-preview handler calls `getEmailExamples($lang)` with no args.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "email_preview.php",
        line: 64,
        message_contains: "getEmailExamples(): 0 passed, 1 required",
    },
    // A test script calls `requestToAllAppApiEndpoints($host, $header, $token)`
    // with one argument — ArgumentCountError (1 passed, 3 required).
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "call.too-few-arguments",
        path_suffix: "test/testall.php",
        line: 14,
        message_contains: "AppApi_Testing::requestToAllAppApiEndpoints(): 1 passed, 3 required",
    },
    // ADR-0078 #190: `dataProviderThatTriggersPhpError` (`$foo = []; $foo->bar();`)
    // is issue 5451's reproduction fixture — genuinely fatals by design. TRUE.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "call.on-non-object",
        path_suffix: "regression/5451/Issue5451Test.php",
        line: 20,
        message_contains: "proven array on this path",
    },
    // ADR-0078 #184: issue 6294's reproduction fixture deliberately weakens a
    // parent's visibility to observe the engine fatal. TRUE, same class as above.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "override.visibility-weakened",
        path_suffix: "regression/6294/B.php",
        line: 17,
        message_contains: "weakens the visibility",
    },
    // ADR-0078 #187: `array.duplicate-key` fired 19 times, all monorepo, all
    // TRUE: 1 config key overwritten (dead value), 12 duplicate allowlist ids,
    // 1 series-options key, 1 analytics path key, 3 test-fixture keys, 1
    // view-parameter key. Mechanics/red-on-sight (ADR-0050 §1); a corpus
    // reseed may move these lines. A config accessor literal binds
    // 'x_restricts' twice below; the earlier value is dead.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Illust/Common.php",
        line: 2495,
        message_contains: "array key 'x_restricts' is declared twice",
    },
    // An append-grown integer allowlist literal: 12 ids duplicated across its
    // history, each overwriting an identical value — churn, no information.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 1680,
        message_contains: "array key 8317821 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2450,
        message_contains: "array key 8279354 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2451,
        message_contains: "array key 8317785 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2452,
        message_contains: "array key 8318880 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2453,
        message_contains: "array key 7886722 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2454,
        message_contains: "array key 8267865 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2455,
        message_contains: "array key 8168208 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2456,
        message_contains: "array key 8315952 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2457,
        message_contains: "array key 8240621 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2458,
        message_contains: "array key 8204566 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2459,
        message_contains: "array key 8214002 is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "Novel/NovelsAllowedCoverReupload.php",
        line: 2460,
        message_contains: "array key 8166826 is declared twice",
    },
    // A series-options literal binds 'ai_type' twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "NovelSeries/Common.php",
        line: 1727,
        message_contains: "array key 'ai_type' is declared twice",
    },
    // An analytics referer-config literal binds the same path twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "UserAnalytics/RefererConfig.php",
        line: 554,
        message_contains: "array key '/novel/index.php' is declared twice",
    },
    // A test fixture rebinds 'illust_sanity_level' 3x across 3 literals — copy-paste drift.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 29,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 54,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "SanityLevelTest.php",
        line: 79,
        message_contains: "array key 'illust_sanity_level' is declared twice",
    },
    // A controller's view-parameter literal binds 'tag' twice.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "array.duplicate-key",
        path_suffix: "AllController.php",
        line: 236,
        message_contains: "array key 'tag' is declared twice",
    },
    // ADR-0078 #183: the declaration-fatal tracer's one corpus finding, TRUE.
    // A ClockMock test double extends a `final` class, relying on ext-uopz to
    // strip `final` at runtime; Steins's analyzed PHP has no uopz loaded, so the
    // fatal is real. Issue #205 tracks demoting this once the sidecar reports a
    // final-stripping extension.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "class.extends-final",
        path_suffix: "tests/lib/ExDateTimeImmutableMock.php",
        line: 14,
        message_contains: "cannot extend final class ExDateTimeImmutable",
    },
    // Docblock hygiene (ADR-0078 / issue #186), triaged 2026-08-08: six
    // mechanics ids, red-on-sight. All 11 public-corpus sites read TRUE at
    // source; the private corpus is measured only, not pinned here. The
    // fuzzing driver captures `$lexer` into a closure that never reads it
    // (works off the parser it also captures) — a dead, by-value capture.
    ExpectedProofFinding {
        package: "nikic/PHP-Parser",
        id: "closure.unused-use",
        path_suffix: "tools/fuzzing/target.php",
        line: 111,
        message_contains: "`use ($lexer)` is never read",
    },
    // The mock generator stacks three `@var` docblocks above one `return`.
    // ADR-0073: only the LAST of a run adopts, so `$className` and `$type`
    // (the first two) are inert — correctly silent on the third. TRUE.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/Framework/MockObject/Generator/Generator.php",
        line: 577,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/Framework/MockObject/Generator/Generator.php",
        line: 578,
        message_contains: "sits where nothing adopts it",
    },
    // The same virtual-parameter idiom as the symfony group below, in a
    // vendored `symfony/filesystem` copy inside a fixture tree:
    // `tempnam($dir, $prefix/*, $suffix=''*/)` documents `@param $suffix` for
    // an argument read via `func_get_arg(2)`. TRUE. Pinned, not
    // vendor-suppressed, because pinned packages are analyzed whole
    // (ADR-0015's vendor split is local-project only).
    ExpectedProofFinding {
        package: "composer/composer",
        id: "phpdoc.stale-param",
        path_suffix: "installed-versions2/vendor/symfony/filesystem/Filesystem.php",
        line: 586,
        message_contains: "`@param $suffix` names no parameter",
    },
    // Two deliberate idioms that still leave a tag declaring nothing:
    //   * `@return list<\SIG*>` wildcards the `SIG*` constant family — no
    //     PHPDoc grammar admits it (PHPStan's parser rejects it too). TRUE.
    //   * `SymfonyStyle`'s progress helpers document a virtual `$format` param
    //     (real signature comments it out for BC, read via `func_get_arg(1)`) —
    //     names no parameter of the declaration. TRUE; PHPStan reports
    //     `parameter.notFound` on the same shape.
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.unparsable",
        path_suffix: "Command/SignalableCommandInterface.php",
        line: 24,
        message_contains: "does not parse (expected CloseAngle, found Wildcard)",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 305,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 327,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "phpdoc.stale-param",
        path_suffix: "Style/SymfonyStyle.php",
        line: 355,
        message_contains: "`@param $format` names no parameter",
    },
    ExpectedProofFinding {
        package: "symfony/process",
        id: "phpdoc.unparsable",
        path_suffix: "Process.php",
        line: 1276,
        message_contains: "does not parse (expected CloseAngle, found Wildcard)",
    },
    // Three inert `@var` casts in `MountManager`, two shapes: `move()`/`copy()`
    // (245, 262) follow a real `/** @var */` with a single-star `/* @var */`
    // (a plain comment), breaking ADR-0073 adjacency; `determineFilesystemAndPath()`
    // (358) stacks two docblocks, shadowing the first (phpunit shape above). TRUE.
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 245,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 262,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "thephpleague/flysystem",
        id: "phpdoc.misplaced-var",
        path_suffix: "src/MountManager.php",
        line: 358,
        message_contains: "sits where nothing adopts it",
    },
    // The monorepo's five, verified at source 2026-08-08, all TRUE. Path
    // suffixes are cut to the shortest 1:1-keying fragment (private-corpus
    // naming rule) — a checkout re-cut can move a line and re-red the gate,
    // which is the pin working, not drift to paper over. A `@phpstan-param`
    // below names a parameter the signature lacks — refactor rot.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.stale-param",
        path_suffix: "AppApi/IllustRecommend.php",
        line: 35,
        message_contains: "names no parameter",
    },
    // A stacked duplicate `@phpstan-var`: its twin adopts the property below, and
    // this one adopts nothing at all.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "Search/Illust.php",
        line: 105,
        message_contains: "sits where nothing adopts it",
    },
    // Two `@var` casts as the last statement of a branch, meant for the code
    // after `}` — but ADR-0073 next-statement adoption ends at the brace, so
    // both are inert as written.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "View/NovelCreateBookController.php",
        line: 313,
        message_contains: "sits where nothing adopts it",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.misplaced-var",
        path_suffix: "View/NovelCreateBookController.php",
        line: 317,
        message_contains: "sits where nothing adopts it",
    },
    // A pseudo-tuple `@return [$total, $illust_ids]` — no PHPDoc grammar admits
    // it, so the tag declares nothing.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "phpdoc.unparsable",
        path_suffix: "Controller/V1SearchWorks.php",
        line: 91,
        message_contains: "does not parse",
    },
    // `type.return-missing` (ADR-0078 §5, issue #199), triaged 2026-08-08: every
    // row is a deliberately-stub test double — an empty/never-returning body
    // carrying a real return type, which would fatal if invoked. Not bugs, not
    // FPs — fixtures. Its `maybe-` sibling moved to `POSSIBLY_EXPECTED` under
    // ADR-0081 §8 (a possibly-grade id claims only that *a* path reaches the site).

    // Two macro bodies registered only so the PHPStan extension under test can
    // read their declared return type; the closure body is `{}`, never called.
    ExpectedProofFinding {
        package: "briannesbitt/Carbon",
        id: "type.return-missing",
        path_suffix: "tests/PHPStan/MacroExtensionTest.php",
        line: 66,
        message_contains: "Return value must be of type CarbonInterval, none returned",
    },
    ExpectedProofFinding {
        package: "briannesbitt/Carbon",
        id: "type.return-missing",
        path_suffix: "tests/PHPStan/MacroExtensionTest.php",
        line: 238,
        message_contains: "Return value must be of type Carbon, none returned",
    },
    // Eight `FnStream`/`MockHandler` decorator closures whose body is
    // `self::fail(...)` — reaching one IS the test's failure condition.
    // `Assert::fail(): never` would silence these, but PHPUnit is outside
    // guzzle's analysed universe here. Expected to disappear once cross-package
    // callee resolution reaches them — that disappearance is a gate event.
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/CurlFactoryTest.php",
        line: 6812,
        message_contains: "Return value must be of type bool, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/CurlFactoryTest.php",
        line: 6815,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/RequestFramingTest.php",
        line: 417,
        message_contains: "Return value must be of type bool, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/RequestFramingTest.php",
        line: 423,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/StreamHandlerTest.php",
        line: 4516,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/Handler/StreamHandlerTest.php",
        line: 4519,
        message_contains: "Return value must be of type string, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/PrepareBodyMiddlewareTest.php",
        line: 329,
        message_contains: "Return value must be of type ResponseInterface, none returned",
    },
    ExpectedProofFinding {
        package: "guzzle/guzzle",
        id: "type.return-missing",
        path_suffix: "tests/PrepareBodyMiddlewareTest.php",
        line: 380,
        message_contains: "Return value must be of type ResponseInterface, none returned",
    },
    // A `tests/_files` fixture implementing `Event` with two empty method
    // bodies carrying the interface's declared return types.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "type.return-missing",
        path_suffix: "tests/_files/DummyEvent.php",
        line: 17,
        message_contains: "DummyEvent::telemetryInfo(): Return value must be of type Info",
    },
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "type.return-missing",
        path_suffix: "tests/_files/DummyEvent.php",
        line: 21,
        message_contains: "DummyEvent::asString(): Return value must be of type string",
    },
    // `NonStringInput`: an `Input` subclass whose three overrides are empty
    // bodies carrying the parent's return types; the test drives the listener,
    // never these methods.
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 118,
        message_contains: "NonStringInput::getFirstArgument(): Return value must be of type ?string",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 122,
        message_contains: "NonStringInput::hasParameterOption(): Return value must be of type bool",
    },
    ExpectedProofFinding {
        package: "symfony/console",
        id: "type.return-missing",
        path_suffix: "Tests/EventListener/ErrorListenerTest.php",
        line: 126,
        message_contains: "NonStringInput::getParameterOption(): Return value must be of type mixed",
    },
    // `variable.undefined` (ADR-0078, issue #194), triaged 2026-08-08. Eleven
    // TRUE positives — reads of a name bound nowhere in scope. Two FP-shaped
    // sites from the same run (a same-variable `empty($x)?:` ternary) are fixed
    // at source (both isset/empty arms now shield) and not pinned here. The ten
    // monorepo ones: 4 exception-message typos (incl. `$this->$mode` for
    // `->mode`, `$withd` for `$width`), 1 renamed logger field, 1 KVS setter
    // always storing 0 (live defect), 2 accidental-pass `$offset` reads
    // (`array_splice(…, null)` degrades gracefully), 1 deleted-parameter read,
    // 1 stale fixture. Path suffixes are kept short — full paths carry the
    // private project's name. `$a = $b;` below (no `$b` anywhere) is written
    // to make PHPUnit observe a PHP warning; the `@`-suppressed twin beneath
    // stays silent, correctly.
    ExpectedProofFinding {
        package: "sebastianbergmann/phpunit",
        id: "variable.undefined",
        path_suffix: "event/_files/PhpWarningTest.php",
        line: 18,
        message_contains: "$b is never bound",
    },
    // Exception-message reads of names that do not exist.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Model/Illust/ContentsType.php",
        line: 29,
        message_contains: "$type is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Model/Illust/IllustModel.php",
        line: 134,
        message_contains: "$withd is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Ranking.php",
        line: 109,
        message_contains: "$mode is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "PPoint/PaymentMethodCode.php",
        line: 71,
        message_contains: "$payment_method_code is never bound",
    },
    // The two `$offset` reads below rely on that same accidental degrade.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "script/get_event_circle_user.php",
        line: 78,
        message_contains: "$offset is never bound",
    },
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "script/get_event_circle_user.php",
        line: 82,
        message_contains: "$offset is never bound",
    },
    // A KVS setter storing `(int)$count` of a never-bound name: always zero.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Group/Base.php",
        line: 1381,
        message_contains: "$count is never bound",
    },
    // A deleted parameter, still read on a return path.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "stacc/api.php",
        line: 574,
        message_contains: "$display_m is never bound",
    },
    // A stale test fixture.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "tests/SampleTest.php",
        line: 215,
        message_contains: "$cookie_store is never bound",
    },
    // A logger field silently null since its variable was renamed.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "variable.undefined",
        path_suffix: "Log/FluentdLogger.php",
        line: 41,
        message_contains: "$write_time is never bound",
    },
    // Builtin parameter types (issue #423): a date helper forwards its
    // `$timestamp` to `\date()`, and its own test hands it a non-numeric string
    // on purpose — the test's `expectException(TypeError::class)` names exactly
    // this "must be of type ?int, string given". A non-numeric string is a
    // TypeError for an `int` parameter in BOTH modes, so the descent-bound
    // literal convicts the forwarding line. TRUE, and asserted by its own test.
    ExpectedProofFinding {
        package: "pxxxx-monorepo",
        id: "type.argument-mismatch",
        path_suffix: "Util/DateTime.php",
        line: 65,
        message_contains: "to date() cannot become ?int $timestamp",
    },
];

#[test]
fn the_toml_tables_are_the_rust_constants_row_for_row() {
    let new = super::Baselines::load().unwrap_or_else(|e| panic!("{e}"));
    type Old = &'static [(&'static str, usize)];
    // Name, old constant, new table, old lookup, the file's text.
    type Family<'a> = (&'a str, Old, &'a super::CountTable, fn(&str) -> usize, &'a str);
    let counts: [Family; 4] = [
        (
            "PHPDOC_EXPECTED",
            PHPDOC_EXPECTED,
            &new.phpdoc,
            phpdoc_expected,
            include_str!("../fp-gate/phpdoc_expected.toml"),
        ),
        (
            "THROW_EXPECTED",
            THROW_EXPECTED,
            &new.throw,
            throw_expected,
            include_str!("../fp-gate/throw_expected.toml"),
        ),
        (
            "EFFECT_EXPECTED",
            EFFECT_EXPECTED,
            &new.effect,
            effect_expected,
            include_str!("../fp-gate/effect_expected.toml"),
        ),
        (
            "POSSIBLY_EXPECTED",
            POSSIBLY_EXPECTED,
            &new.possibly,
            possibly_expected,
            include_str!("../fp-gate/possibly_expected.toml"),
        ),
    ];
    for (name, old, table, old_lookup, text) in counts {
        // Every field: the same names with the same counts, and nothing else.
        let mut want: Vec<(String, usize)> =
            old.iter().map(|(n, c)| ((*n).to_owned(), *c)).collect();
        want.sort();
        let got: Vec<(String, usize)> = table.0.iter().map(|(n, c)| (n.clone(), *c)).collect();
        assert_eq!(got, want, "{name}: rows differ");
        // Row order: the file lists its rows in the constant's order.
        let file_order: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix('"'))
            .map(|l| &l[..l.find('"').expect("closing quote")])
            .collect();
        let old_order: Vec<&str> = old.iter().map(|(n, _)| *n).collect();
        assert_eq!(file_order, old_order, "{name}: row order differs");
        // The one lookup answers what the old one did, tabled or not.
        for n in old.iter().map(|(n, _)| *n).chain(["no/such-package", ""]) {
            assert_eq!(table.expected(n), old_lookup(n), "{name}: {n}");
        }
        let old_total: usize = old.iter().map(|(_, c)| *c).sum();
        assert_eq!(table.total(), old_total, "{name}: total");
        assert_eq!(table.is_empty(), old.is_empty(), "{name}: emptiness");
        eprintln!("{name}: {} rows, total {old_total}, identical and in order", old.len());
    }
    assert_eq!(new.proof.len(), EXPECTED_PROOF_FINDINGS.len(), "EXPECTED_PROOF_FINDINGS rows");
    for (i, (o, n)) in EXPECTED_PROOF_FINDINGS.iter().zip(&new.proof).enumerate() {
        assert_eq!(
            (o.package, o.id, o.path_suffix, o.line, o.message_contains),
            (
                n.package.as_str(),
                n.id.as_str(),
                n.path_suffix.as_str(),
                n.line,
                n.message_contains.as_str()
            ),
            "EXPECTED_PROOF_FINDINGS row {i}"
        );
    }
    eprintln!(
        "EXPECTED_PROOF_FINDINGS: {} rows, every field identical and in order",
        EXPECTED_PROOF_FINDINGS.len()
    );
}
