#!/usr/bin/env node
// Node end-to-end over the REAL engine (issue #64 S2): the built steins-wasm
// module and a real php-wasm instance, driven by the very replay loop the
// frontend runs (`replay.mjs`) over the very dispatch the frontend uses
// (`php-dispatch.mjs`). `smoke.mjs` pins the ABI against a canned table; this
// pins the loop against php-src.
//
// It runs the vendored php-wasm — the same 8.5 binary the browser downloads —
// through PhpNode, so there is nothing to npm-install and nothing that can drift
// from what ships.
//
//   ./apps/playground/build.sh
//   node apps/playground/smoke-replay.mjs [path/to/steins_wasm.wasm]

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { PhpNode } from "./vendor/php-wasm/PhpNode.mjs";
import { loadSteins } from "./steins.mjs";
import { createEngine } from "./php-dispatch.mjs";
import { driveReplay, ITERATION_CAP } from "./replay.mjs";
import { renderBoundaryHtml } from "./boundary.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath =
  process.argv[2] ?? join(here, "../../target/wasm32-unknown-unknown/release/steins_wasm.wasm");

const ENV_KEY = '{"method":"env","params":{}}';

let failures = 0;
function assert(cond, label) {
  if (cond) {
    console.log(`ok - ${label}`);
  } else {
    failures += 1;
    console.error(`FAIL - ${label}`);
  }
}

const steins = await loadSteins(await readFile(wasmPath));
const runner = await readFile(join(here, "vendor/runner.php"), "utf8");

// 1. Boot: the unmodified runner, stdin loop and all, inside php-wasm.
const t0 = Date.now();
const engine = await createEngine(new PhpNode({ version: "8.5" }), runner);
console.log(`engine: PHP ${engine.version}, PHP_INT_SIZE=${engine.intSize}, boot ${Date.now() - t0} ms`);
assert(engine.version.startsWith("8.5"), `the vendored build is PHP 8.5 (got ${engine.version})`);
assert(engine.intSize === 4 || engine.intSize === 8, "the boot probe reports an integer width");

// The loop's two adapters. `asked` records every batch so the suite can pin
// which questions the analysis asks, and when.
const asked = [];
async function answer(keys) {
  asked.push([...keys]);
  const answers = Object.create(null);
  const failed = [];
  for (const key of keys) {
    try {
      answers[key] = await engine.answer(key);
    } catch (err) {
      console.error(`  engine failure on ${key}: ${err.message}`);
      failed.push(key);
    }
  }
  return { answers, failures: failed };
}

function analyzer(source, profile = "") {
  return (table) => {
    const check = steins.checkReplay(source, table, profile);
    return { ok: check.ok, pending: check.ok ? check.pending : [], value: check };
  };
}

// 2. THE FLAGSHIP (issue #64 acceptance criterion 1). `dumpType(greet(2,
//    "World"))` inlines through the real engine: a project call in argument
//    position (#60) whose body concatenates (#59) and folds `str_repeat` — which
//    is on the verified portable subset, so it folds on this 32-bit build.
const FLAGSHIP = `<?php
function greet(int $times, string $name): string {
    return str_repeat("Hello, " . $name . "! ", $times);
}
\\PHPStan\\dumpType(greet(2, "World"));
`;
const GREETING = "Hello, World! Hello, World! ";

const table = Object.create(null);
const flagship = await driveReplay({ analyze: analyzer(FLAGSHIP), answer, table });
console.log(`flagship: ${flagship.status} in ${flagship.iterations} iteration(s); batches ${JSON.stringify(asked)}`);
assert(flagship.status === "converged", `the loop converges over the real engine (got ${flagship.status}${flagship.reason ? `: ${flagship.reason}` : ""})`);
assert(flagship.iterations <= ITERATION_CAP, `the fixpoint came inside the cap (${flagship.iterations} <= ${ITERATION_CAP})`);
assert(flagship.value.pending.length === 0, "the rendered envelope has no pending requests");
// TWO batches, exactly: one to learn the machine (and reflect `greet`), one to
// fold `str_repeat` now that the width gate admits it. Before S1.5 this was one
// batch because the fold was refused outright — the extra round trip IS the
// flagship lighting up, so the count is pinned rather than loosened.
const flagshipBatches = asked.length;
assert(flagshipBatches === 2, `the flagship takes two engine batches (got ${flagshipBatches})`);

const dump = flagship.value.findings.find((f) => f.id === "debug.type");
console.log(`flagship dump: ${dump && dump.message}`);
assert(
  dump !== undefined && dump.message === `dumped type: '${GREETING}'`,
  `the flagship inlines through the real engine (got: ${dump && dump.message})`,
);

assert(
  steins.check(FLAGSHIP).findings.some((f) => f.message === "dumped type: string"),
  "…and without the engine the same snippet is the sound subset's `string`",
);

// …and issue #61's own table, in the margin the "Show types" overlay renders.
// Those two rows are the ones that reported `unknown` in the browser and a value
// on the CLI; through the real engine the browser now agrees with the CLI.
for (const [src, want] of [
  ['<?php\n$a = strtoupper("ab");\n', '$a = "AB"'],
  ['<?php\n$a = str_repeat("ab", 2);\n', '$a = "abab"'],
]) {
  const run = await driveReplay({ analyze: analyzer(src), answer, table });
  const margin = steins.annotateReplay(src, table);
  const texts = margin.lines.map((l) => l.text);
  assert(
    run.status === "converged" && margin.pending.length === 0 && texts.includes(want),
    `issue #61's table row folds in the margin: ${want} (got ${JSON.stringify(texts)})`,
  );
  assert(
    !steins.annotate(src).lines.map((l) => l.text).includes(want),
    `…and is NOT there without the engine (${src.trim().split("\n").pop()})`,
  );
}

// 3. THE UNION FOLD (issue #74): an argument that is a bounded union of
//    constants is folded once per member combination, and the members ride the
//    replay loop as ordinary pending requests — no new wire machinery at all.
//    The whole product is asked in ONE batch, which is the property worth
//    pinning: the loop learns every member per round trip, not one member per
//    round trip, so a union costs the browser the same two iterations a single
//    constant does.
const UNION = `<?php
function f(bool $c): void {
    $x = $c ? 'a' : 'b';
    \\PHPStan\\dumpType(strtoupper($x));
}
`;
const askedBeforeUnion = asked.length;
const union = await driveReplay({ analyze: analyzer(UNION), answer, table });
const unionBatches = asked.slice(askedBeforeUnion);
console.log(`union: ${union.status} in ${union.iterations} iteration(s); batches ${JSON.stringify(unionBatches)}`);
assert(union.status === "converged", `the union fold converges over the real engine (got ${union.status}${union.reason ? `: ${union.reason}` : ""})`);
assert(union.value.pending.length === 0, "the rendered envelope has no pending requests");
assert(unionBatches.length === 1, `the whole product is asked in one batch (got ${unionBatches.length}: ${JSON.stringify(unionBatches)})`);
const memberFolds = (unionBatches[0] ?? []).filter((k) => k.includes('"method":"fold"'));
console.log(`union member folds: ${JSON.stringify(memberFolds)}`);
assert(memberFolds.length === 2, `both members appear as pending fold requests (got ${JSON.stringify(memberFolds)})`);
const unionDump = union.value.findings.find((f) => f.id === "debug.type");
console.log(`union dump: ${unionDump && unionDump.message}`);
assert(
  unionDump !== undefined && unionDump.message === "dumped type: 'A'|'B'",
  `the members compose to a union (got: ${unionDump && unionDump.message})`,
);
const unionPlain = steins.check(UNION).findings.find((f) => f.id === "debug.type");
assert(
  unionPlain !== undefined && unionPlain.message !== "dumped type: 'A'|'B'",
  `…and without the engine the same snippet takes a lower rung (got: ${unionPlain && unionPlain.message})`,
);

// 4. The boot object (issue #64 S3): the engine surface as the analysis' own
//    gates see it, which is what the page renders its boundary from. On the
//    machine the browser actually gets — php-wasm's 32-bit 8.5 — that is the
//    portable fold subset, no curated rows, absence family live.
const boot = flagship.value.boot;
console.log(`boot: ${JSON.stringify(boot)}`);
assert(boot !== undefined && boot !== null, "a replay envelope carries a boot object");
assert(boot.php_version === engine.version, `boot.php_version is the engine's own (${boot.php_version} vs ${engine.version})`);
assert(boot.int_size === engine.intSize, `boot.int_size is the engine's own (${boot.int_size} vs ${engine.intSize})`);
assert(boot.fold_lane === "portable_subset", `a 32-bit engine folds the portable subset (got ${boot.fold_lane})`);
// This engine's SHARE of the allowlist is the number worth pinning by hand, and
// it is the only one here: the total is derived below from the boot object's own
// parts, and `steins-catalog`'s partition test owns it upstream. What moved it:
// ADR-0028's 2026-08-14 wave 1 added `array_merge` and `explode` as the first
// UNVERIFIED rows, so the allowlist grew and this engine's share did not; issue
// #354 then probed the five names that wave deferred and moved BOTH counts —
// `str_split`, `array_fill` and `array_unique` fold here now, `range` and
// `preg_split` are named below; the alias slice added `join`/`chop`/`sizeof`/
// `doubleval`, second spellings of names already folding here, so only the safe
// count moved; and wave 2 added `strpos`/`stripos`/`strrpos` and
// `round`/`floor`/`ceil`, six names whose only integer parameter declines rather
// than diverges on the narrow engine, so both counts moved together (44/57 →
// 50/63) and nothing new was refused; and issue #382 added `array_filter` here
// (50 → 51) with `preg_match` refused beside `preg_split` for the same PCRE
// build option (11 → 12 refusals, the allowlist 63 → 65); and the generated
// probe then measured the two rows ADR-0028 had admitted UNMEASURED, so
// `array_merge` and `explode` fold here too (51 → 53) while the allowlist
// stands still — nothing was admitted, a debt was paid.
assert(boot.fold_portable === 53, `this engine's share comes from the catalog (${boot.fold_portable})`);
// `refused_folds` stays the REFUSED rows — the ones with a recorded
// divergence, which is what the boundary panel's sentence about them claims. The
// unverified rows decline on the same gate with nothing on record, so they are not
// merged in here: ADR-0028's 2026-08-14 amendment §4 gives them their own field,
// and the panel gives them their own sentence.
assert(
  Array.isArray(boot.refused_folds) &&
    boot.refused_folds.join(",") ===
      "abs,intval,sprintf,dechex,decbin,decoct,bindec,hexdec,version_compare,range,preg_split,preg_match,preg_match_all,json_decode,json_encode",
  `the refused folds are named (got ${JSON.stringify(boot.refused_folds)})`,
);
// …and beside the names, WHY. The panel groups by `axis` and quotes `witness`,
// so a refused row added on a new axis changes the page without the page being
// edited — the property the hand-written sentence did not have.
assert(
  Array.isArray(boot.refusals) && boot.refusals.length === boot.refused_folds.length,
  `every refused row carries its reason (got ${JSON.stringify(boot.refusals)})`,
);
assert(
  boot.refusals.every((r) => r.name && r.axis && r.witness.includes(" / ")),
  "each reason names the row, its axis, and both engines' answers",
);
assert(
  boot.refusals.filter((r) => r.axis === "build_option").map((r) => r.name).join(",") === "preg_split",
  `preg_split is the row that is not about the word size (got ${JSON.stringify(boot.refusals.filter((r) => r.axis === "build_option"))})`,
);
// …and the panel the visitor reads is composed from that, checked here rather
// than only in a browser. The boot object carrying eleven reasons and the panel
// showing two is exactly the shape of defect this catches: assert on the
// RENDERED text, per row.
const panel = renderBoundaryHtml(boot);
for (const r of boot.refusals) {
  assert(
    panel.includes(r.name) && panel.includes(r.witness.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;")),
    `the panel shows ${r.name}'s own witness, not a neighbour's`,
  );
}
// An axis the page has no sentence for must fall back to a neutral one rather
// than borrow another axis's explanation — the property that lets the catalog
// grow `RefusalAxis` without the page going false.
const invented = renderBoundaryHtml({
  ...boot,
  refused_folds: ["fictional_fn"],
  refusals: [{ name: "fictional_fn", axis: "operating_system", witness: "fictional_fn() is A / B" }],
});
assert(
  invented.includes("a divergence is on record for each") && invented.includes("fictional_fn() is A / B"),
  "an unknown refusal axis renders neutrally, with its witness intact",
);
assert(
  !invented.includes("how the engine was compiled"),
  "…and does not borrow the build-option sentence",
);
// `__proto__` is a legal string and reaches an object literal's prototype, so
// the axis lookup is a Map. Before it was, this threw instead of falling back.
const proto = renderBoundaryHtml({
  ...boot,
  refused_folds: ["proto_fn"],
  refusals: [{ name: "proto_fn", axis: "__proto__", witness: "proto_fn() is A / B" }],
});
assert(
  proto.includes("a divergence is on record for each") && proto.includes("proto_fn() is A / B"),
  "an axis spelled `__proto__` falls back like any other unknown one",
);

// The `declined` lane folds NOTHING. A two-armed "full or else" gave it the
// portable-subset sentence, so the panel claimed a fold count in its first list
// and "Nothing folds" in its second — both at once, about the same engine.
const declined = renderBoundaryHtml({ ...boot, int_size: null, fold_lane: "declined" });
assert(
  !declined.includes(`of the ${boot.fold_total} builtins`),
  "a declined lane claims no folded values",
);
assert(declined.includes("Nothing folds"), "…and says so once, in the second list");
// The control: the lane that DOES fold a subset still says how many.
assert(
  renderBoundaryHtml(boot).includes(`${boot.fold_portable} of the ${boot.fold_total} builtins`),
  "the portable-subset lane still reports its share",
);

// The class is EMPTY now, and the field still travels: "nothing is unmeasured"
// is a claim, and a different one from a missing field ("this lane has no
// opinion"). The panel has to say the first without inventing the second.
assert(
  Array.isArray(boot.unverified_folds) && boot.unverified_folds.length === 0,
  `nothing is unmeasured any more (got ${JSON.stringify(boot.unverified_folds)})`,
);
// The total, derived rather than transcribed: every allowlisted name is either
// portable on this engine or one of the two kinds of decline named above, so the
// three fields have to add up to it. A wave that grows the allowlist and forgets
// to say which class it grew fails here without anyone editing a number.
assert(
  boot.fold_total === boot.fold_portable + boot.refused_folds.length + boot.unverified_folds.length,
  `the boot object's own counts add up (${boot.fold_portable} + ${boot.refused_folds.length} + ${boot.unverified_folds.length} vs ${boot.fold_total})`,
);
assert(boot.curated_rows === false, "a curated row is pinned to a machine, not only a version");
assert(boot.absence_family === true, "existence is not arithmetic — the absence family is live");
assert(typeof boot.label === "string" && boot.label.includes(engine.version), `boot.label names the boot surface (${boot.label})`);
assert(steins.annotateReplay(FLAGSHIP, table).boot.fold_lane === boot.fold_lane, "both lanes report the same engine");
assert(steins.check(FLAGSHIP).boot === undefined, "the engine-free envelope carries no boot object at all");

// 5. …and the boundary is honest in the other direction: `abs` is a REFUSED
//    name on a 32-bit engine (`abs("3000000000")` is int there and float here —
//    the type tag flips), so it must not fold. What comes back is a TYPE, not a
//    wrong value: the fold declines, the reflected `int|float` envelope is not a
//    single fact either, and the answer falls to ADR-0069's Asserted declared-
//    return floor. Since issue #79 that floor states functionMap's own
//    `positive-int|0|float` — a multi-base union #73 counted and dropped — and the
//    `(asserted)` marker is what says a declaration answered rather than a fold.
const REFUSED = '<?php\n\\PHPStan\\dumpType(abs(-3));\n';
const refused = await driveReplay({ analyze: analyzer(REFUSED), answer, table });
assert(refused.status === "converged", `the refused-fold snippet converges (got ${refused.status})`);
const refusedDump = refused.value.findings.find((f) => f.id === "debug.type");
console.log(`refused dump: ${refusedDump && refusedDump.message}`);
assert(
  refusedDump !== undefined && refusedDump.message.endsWith(" (asserted)"),
  `a width-refused builtin widens to a declared type instead of folding (got: ${refusedDump && refusedDump.message})`,
);
assert(
  refusedDump.message !== "dumped type: 3",
  "the value a 64-bit engine would have folded never appears on a 32-bit one",
);

// 5b. Issue #354, both verdicts against the REAL engine rather than a table.
//     `str_split` probed clean, so the browser folds it — and this is the first
//     time it folds a builtin whose result is an ARRAY, since every array-
//     returning name before this slice was refused or unverified here. `range`
//     probed dirty and must not fold: `range("3000000000", "3000000000")` is a
//     list of int on a 64-bit engine and of float on this one, which is exactly
//     the argument below, so a regression admitting the name would show up as a
//     value rather than as a type.
const SAFE_354 = '<?php\n\\PHPStan\\dumpType(str_split("abcdef", 2));\n';
const safe354 = await driveReplay({ analyze: analyzer(SAFE_354), answer, table });
const safeDump = safe354.value.findings.find((f) => f.id === "debug.type");
console.log(`str_split dump: ${safeDump && safeDump.message}`);
assert(
  safeDump !== undefined && safeDump.message === "dumped type: list{'ab', 'cd', 'ef'}",
  `a newly portable array result folds in the browser (got: ${safeDump && safeDump.message})`,
);

const REFUSED_354 = '<?php\n\\PHPStan\\dumpType(range("3000000000", "3000000000"));\n';
const refused354 = await driveReplay({ analyze: analyzer(REFUSED_354), answer, table });
const refused354Dump = refused354.value.findings.find((f) => f.id === "debug.type");
console.log(`range dump: ${refused354Dump && refused354Dump.message}`);
assert(
  refused354Dump !== undefined && !refused354Dump.message.startsWith("dumped type: list{"),
  `the width-refused \`range\` widens to a type here (got: ${refused354Dump && refused354Dump.message})`,
);

// 6. `env` is asked exactly once, on the first iteration, and never again — the
//    property the whole memo table exists for. The flagship takes TWO batches:
//    one to learn the machine (and reflect `greet`), one to fold `str_repeat`
//    now that the width gate admits it — the round trip S1.5 bought.
assert(asked.length > 0 && asked[0].includes(ENV_KEY), "the first iteration asks the environment");
assert(
  asked.slice(1).every((batch) => !batch.includes(ENV_KEY)),
  "later iterations do not re-ask the environment",
);
assert(ENV_KEY in table, "the env answer is in the table");
assert(typeof table[ENV_KEY].php_version === "string", `the env answer carries a php_version (${table[ENV_KEY] && table[ENV_KEY].php_version})`);
assert(table[ENV_KEY].int_size === engine.intSize, "the env answer's width agrees with the boot probe");

// 7. The absence family: structurally silent without the engine, witnessed with
//    it. This is the surface lighting up, not a fold — it is what a 32-bit build
//    still proves (ADR-0066 §4).
const ABSENT = "<?php\ntyop();\n";
const plain = steins.check(ABSENT);
const withEngine = await driveReplay({ analyze: analyzer(ABSENT), answer, table });
assert(withEngine.status === "converged", `the absence snippet converges (got ${withEngine.status})`);
const plainIds = plain.findings.map((f) => f.id);
const engineIds = withEngine.value.findings.map((f) => f.id);
assert(!plainIds.includes("call.undefined-function"), `the plain run is silent on absence (got: ${plainIds.join(", ") || "nothing"})`);
assert(
  engineIds.includes("call.undefined-function"),
  `the engine witnesses the absence (got: ${engineIds.join(", ") || "nothing"})`,
);
const absence = withEngine.value.findings.find((f) => f.id === "call.undefined-function");
console.log(`absence finding: ${absence && absence.message}`);
assert(absence !== undefined && absence.line === 2, "the absence finding lands on the call's line");

// 8. The table is session-global and monotone: a repeat analysis reuses the
//    table's answers instead of asking again.
const batchesBefore = asked.length;
const again = await driveReplay({ analyze: analyzer(ABSENT), answer, table });
assert(again.status === "converged", "a repeat analysis converges");
assert(asked.length === batchesBefore, "a repeat analysis asks the engine nothing at all");

// 9. The annotate lane rides the same table.
const annotate = steins.annotateReplay(FLAGSHIP, table);
assert(annotate.ok === true, "annotate replay envelope ok");
assert(annotate.pending.length === 0, "annotate reaches its fixpoint on the check's table");
assert(annotate.lines.length > 0, `annotate returns margin facts (${annotate.lines.length} lines)`);

// 10. The cap is the caller's, and exhausting it is a status, not a hang: an
//    answerer that answers nothing usable must stop the loop, not spin it.
const stubborn = await driveReplay({
  analyze: analyzer(FLAGSHIP),
  answer: async (keys) => ({ answers: Object.create(null), failures: [...keys] }),
  table: Object.create(null),
  cap: 4,
});
assert(stubborn.status === "failed", `a failing answerer stops the loop (got ${stubborn.status})`);
const silent = await driveReplay({
  analyze: analyzer(FLAGSHIP),
  answer: async () => ({ answers: Object.create(null), failures: [] }),
  table: Object.create(null),
  cap: 4,
});
assert(silent.status === "exhausted", `an answerer that never answers hits the cap (got ${silent.status})`);
// An answer the analysis cannot parse does not spin the loop either: the wasm
// side treats an unusable table entry the way it treats a dead sidecar — it
// declines, and the run completes as the sound subset. Nothing false is added,
// which is why an intermediate iteration is safe to compute at all.
const garbling = await driveReplay({
  analyze: analyzer(FLAGSHIP),
  answer: async (keys) => {
    const answers = Object.create(null);
    for (const key of keys) answers[key] = { not: "a result" };
    return { answers, failures: [] };
  },
  table: Object.create(null),
  cap: 4,
});
assert(
  garbling.status === "converged" && garbling.value.pending.length === 0,
  `an unusable answer degrades to a sound run instead of spinning (got ${garbling.status})`,
);
assert(
  garbling.value.findings.every((f) => !String(f.message).includes("Hello, World! Hello, World! ")),
  "the degraded run carries no folded value",
);

// 11. A structured engine failure, not a throw, for a request the engine cannot
//    make sense of.
const bogus = await answer(['{"method":"nope","params":{}}']);
console.log(`bogus dispatch: ${JSON.stringify(bogus)}`);
assert(bogus.failures.length === 0 && bogus.answers['{"method":"nope","params":{}}'] !== undefined, "an unknown method still answers structurally");

process.exit(failures === 0 ? 0 : 1);
