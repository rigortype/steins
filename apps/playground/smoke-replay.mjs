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

// 2. A snippet with a builtin call converges — the engine answers, the loop
//    stops, and it stops well inside the cap.
const FLAGSHIP = `<?php
function greet(int $times, string $name): string {
    return str_repeat("Hello, " . $name . "! ", $times);
}
\\PHPStan\\dumpType(greet(2, "World"));
`;

const table = Object.create(null);
const flagship = await driveReplay({ analyze: analyzer(FLAGSHIP), answer, table });
console.log(`flagship: ${flagship.status} in ${flagship.iterations} iteration(s); batches ${JSON.stringify(asked)}`);
assert(flagship.status === "converged", `the loop converges over the real engine (got ${flagship.status}${flagship.reason ? `: ${flagship.reason}` : ""})`);
assert(flagship.iterations <= ITERATION_CAP, `the fixpoint came inside the cap (${flagship.iterations} <= ${ITERATION_CAP})`);
assert(flagship.value.pending.length === 0, "the rendered envelope has no pending requests");

// 3. `env` is asked exactly once, on the first iteration, and never again — the
//    property the whole memo table exists for.
assert(asked.length > 0 && asked[0].includes(ENV_KEY), "the first iteration asks the environment");
assert(asked.length === 1, `one engine batch answered the whole analysis (got ${asked.length})`);
assert(
  asked.slice(1).every((batch) => !batch.includes(ENV_KEY)),
  "later iterations do not re-ask the environment",
);
assert(ENV_KEY in table, "the env answer is in the table");
assert(typeof table[ENV_KEY].php_version === "string", `the env answer carries a php_version (${table[ENV_KEY] && table[ENV_KEY].php_version})`);
assert(table[ENV_KEY].int_size === engine.intSize, "the env answer's width agrees with the boot probe");

// 4. The absence family: structurally silent without the engine, witnessed with
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

// 5. The table is session-global and monotone: the second analysis reused the
//    first one's env answer instead of asking again.
const batchesBefore = asked.length;
const again = await driveReplay({ analyze: analyzer(ABSENT), answer, table });
assert(again.status === "converged", "a repeat analysis converges");
assert(asked.length === batchesBefore, "a repeat analysis asks the engine nothing at all");

// 6. The annotate lane rides the same table.
const annotate = steins.annotateReplay(FLAGSHIP, table);
assert(annotate.ok === true, "annotate replay envelope ok");
assert(annotate.pending.length === 0, "annotate reaches its fixpoint on the check's table");
assert(annotate.lines.length > 0, `annotate returns margin facts (${annotate.lines.length} lines)`);

// 7. The cap is the caller's, and exhausting it is a status, not a hang: an
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

// 8. A structured engine failure, not a throw, for a request the engine cannot
//    make sense of.
const bogus = await answer(['{"method":"nope","params":{}}']);
console.log(`bogus dispatch: ${JSON.stringify(bogus)}`);
assert(bogus.failures.length === 0 && bogus.answers['{"method":"nope","params":{}}'] !== undefined, "an unknown method still answers structurally");

process.exit(failures === 0 ? 0 : 1);
