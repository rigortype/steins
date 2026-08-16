#!/usr/bin/env node
// Node smoke over the built wasm module (ADR-0065; the counterpart of
// rigor-playground's wasm/smoke.mjs). This is the CI gate before any artifact
// upload: it proves the module instantiates in a plain wasm VM, that a check
// returns real findings with stable ids, that annotate returns margin facts,
// and that a broken snippet degrades instead of trapping.
//
//   cargo build -p steins-wasm --target wasm32-unknown-unknown --release
//   node apps/playground/smoke.mjs [path/to/steins_wasm.wasm]

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { loadSteins } from "./steins.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath =
  process.argv[2] ??
  join(here, "../../target/wasm32-unknown-unknown/release/steins_wasm.wasm");

let failures = 0;
function assert(cond, label) {
  if (cond) {
    console.log(`ok - ${label}`);
  } else {
    failures += 1;
    console.error(`FAIL - ${label}`);
  }
}

const bytes = await readFile(wasmPath);
console.log(`module: ${wasmPath} (${(bytes.length / 1024 / 1024).toFixed(2)} MB)`);
const steins = await loadSteins(bytes);

// 1. A seeded snippet with a proof-layer finding.
const snippet = `<?php
function greet(int $n): int { return $n; }
greet("abc");
`;
const check = steins.check(snippet);
assert(check.ok === true, "check envelope ok");
assert(typeof check.notice === "string" && check.notice.includes("sound subset"), "sound-subset notice travels as data");
assert(check.profile === "default", "default profile resolved");
const ids = check.findings.map((f) => f.id);
assert(ids.includes("type.argument-mismatch"), `stable id present (got: ${ids.join(", ")})`);
const finding = check.findings.find((f) => f.id === "type.argument-mismatch");
assert(finding.line === 3 && finding.level === "fail" && finding.layer === "proof", "finding carries line/level/layer");

// 2. annotate through the same module.
const ann = steins.annotate(snippet);
assert(ann.ok === true, "annotate envelope ok");
assert(Array.isArray(ann.lines) && ann.lines.length > 0, `annotate returns margin facts (${ann.lines.length} lines)`);

// 3. The rung ladder resolves; unknown profile is data, not a trap.
for (const p of ["default", "contracts", "throws-direct", "strict"]) {
  assert(steins.check(snippet, p).ok === true, `profile ${p} resolves`);
}
const bad = steins.check(snippet, "nope");
assert(bad.ok === false && bad.error.includes("unknown profile"), "unknown profile is a structured error");

// 4. Broken syntax: recovered analysis + reported parse errors, no trap.
const broken = steins.check("<?php\nfunction f( {\n");
assert(broken.ok === true, "broken snippet does not trap");
assert(broken.parse_errors.length > 0, "parse errors are reported in the envelope");

// 4b. Deep expression nesting (issue #264). The module's shadow stack is fixed at
// link time and the host VM's call stack is not ours to raise, so this is the one
// surface where the analyzer refuses by name instead of buying headroom. A trap
// here is a dead module and a JavaScript error naming neither PHP nor a line —
// which is exactly what shipped before the headroom guard landed, at depths a
// real repository reaches: phpstan-src's own tests/bench/data/nullsafe-chain-walk.php
// is 1,000 levels of `->`, and the pre-fix ceiling was between 300 and 600.
const chain = (n) => `<?php\n$n = new stdClass();\n$x = $n${"->next".repeat(n)};\n`;
for (const depth of [100, 300, 1000]) {
  let deep;
  try {
    deep = steins.check(chain(depth));
  } catch (e) {
    deep = null;
    assert(false, `${depth}-level chain traps the module: ${e}`);
  }
  if (deep === null) continue;
  assert(deep.ok === true, `${depth}-level chain returns an envelope`);
  const refused = deep.parse_errors.some((e) => e.message.includes("nests deeper than the analyzer can walk"));
  const ids = deep.findings.map((f) => f.id);
  if (depth <= 100) {
    // Comfortably inside the budget: answered in full, nothing manufactured.
    assert(!refused && deep.parse_errors.length === 0, `${depth} levels are analyzed, not refused`);
  } else {
    // Past it: a named silence in the syntax family, not a trap and not a guess.
    assert(refused, `${depth} levels are refused by name (got: ${JSON.stringify(deep.parse_errors)})`);
    assert(ids.includes("syntax.unparsable"), `${depth} levels surface as syntax.unparsable (got: ${ids.join(", ")})`);
  }
}

// The module is still alive after the refusal — the whole point of refusing.
assert(steins.check(snippet).findings.length > 0, "the instance still answers after a refused file");

// 5. Repeated calls on one instance (the playground's debounce pattern).
for (let i = 0; i < 50; i++) steins.check(snippet + `// edit ${i}\n`);
assert(true, "50 repeated checks on one instance");

// 6. The replay round trip (ADR-0066). No php-wasm here yet — the answers are a
// canned table captured from a real `php` by the differential oracle in
// crates/steins-infer/tests/replay_fold.rs — so what this pins is the ABI: the
// pending contract, the key format the JS loop echoes back, and the fold landing
// in the envelope once the table is complete.
const flagship = `<?php
function greet(int $times, string $name): string {
    return str_repeat("Hello, " . $name . "! ", $times);
}
\\PHPStan\\dumpType(greet(2, "World"));
`;

const ANSWERS = {
  '{"method":"env","params":{}}': {
    php_version: "8.5.8",
    extensions: ["Core", "standard"],
    sapi: "cli",
    int_size: 8,
  },
  '{"method":"fold","params":{"function":"str_repeat","args":["Hello, World! ",2],"strict":false}}': {
    kind: "value",
    value: "Hello, World! Hello, World! ",
    type: "string",
  },
  '{"method":"reflect","params":{"target":"greet"}}': {
    kind: "reflection",
    target: "greet",
    exists: false,
    function: false,
    class_like: false,
    return_type: null,
    return_type_tentative: false,
  },
};

const empty = steins.checkReplay(flagship, {});
assert(empty.ok === true, "replay envelope ok with an empty table");
assert(Array.isArray(empty.pending) && empty.pending.length > 0, "an empty table reports pending requests");
assert(
  empty.pending.every((k) => {
    const req = JSON.parse(k);
    return typeof req.method === "string" && req.params !== undefined;
  }),
  "every pending key parses as {method, params}",
);
assert(
  empty.findings.every((f) => f.message !== "dumped type: 'Hello, World! Hello, World! '"),
  "a degraded run does not carry the folded value",
);

// The loop, exactly as the frontend will run it: answer pending from the canned
// table, re-call, stop when pending is empty. The cap is defensive.
let table = {};
let result = null;
let iterations = 0;
for (; iterations < 8; iterations++) {
  result = steins.checkReplay(flagship, table);
  if (result.pending.length === 0) break;
  let answered = 0;
  for (const key of result.pending) {
    if (key in ANSWERS) {
      table[key] = ANSWERS[key];
      answered += 1;
    }
  }
  if (answered === 0) break; // no progress: the canned table is incomplete
}
assert(result.pending.length === 0, `the loop reached a fixpoint (${iterations} iterations)`);
assert(iterations < 8, "the fixpoint came well inside the cap");
const folded = result.findings.find((f) => f.id === "debug.type");
assert(
  folded !== undefined && folded.message === "dumped type: 'Hello, World! Hello, World! '",
  `the flagship folds through the replay loop (got: ${folded && folded.message})`,
);

// 7. The boot object (issue #64 S3): the engine surface as data, so the page can
// state its precision boundary instead of asserting a stale one. The canned env
// above is 64-bit at the pinned minor, so every lane is live — and the refused
// folds are not named because on this machine there are none.
assert(empty.boot !== undefined, "a replay envelope always carries a boot object");
assert(empty.boot.fold_lane === "declined", "before the env answer, nothing folds");
assert(empty.boot.label === null && empty.boot.int_size === null, "…and nothing is described");
const boot = result.boot;
assert(boot.php_version === "8.5.8" && boot.int_size === 8, `boot reports the engine (${JSON.stringify(boot)})`);
assert(boot.fold_lane === "full", `a 64-bit engine folds the whole allowlist (got ${boot.fold_lane})`);
assert(boot.curated_rows === true && boot.absence_family === true, "every lane is live at the pin");
assert(boot.refused_folds === undefined, "nothing is refused on the full lane");
assert(boot.unverified_folds === undefined, "…and nothing is unverified there either");
// 65 = 53 portable + 12 refused + 0 unverified. The unverified class is EMPTY
// since issue #382 measured its last two rows (`array_merge`, `explode`) and
// both left for portable — the allowlist did not grow, a debt was paid.
// `steins-catalog`'s partition test owns these numbers; this asserts they
// travel. On the full lane every one of the 65 folds, so what a narrow engine
// would decline is invisible here.
assert(boot.fold_total === 65 && boot.fold_portable === 53, "the catalog's own counts travel");
assert(steins.check(flagship).boot === undefined, "the sound-subset envelope carries no boot key");

// annotate rides the same loop.
const ann2 = steins.annotateReplay(flagship, table);
assert(ann2.ok === true && ann2.pending.length === 0, "annotate reaches its fixpoint on the same table");
assert(ann2.lines.length > 0, "annotate replay returns margin facts");

// A malformed table is data, not a trap.
const badTable = steins.checkReplay(flagship, "not an object");
assert(badTable.ok === false && badTable.error.includes("replay table"), "a malformed table is a structured error");

process.exit(failures === 0 ? 0 : 1);
