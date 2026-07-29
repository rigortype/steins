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

// 5. Repeated calls on one instance (the playground's debounce pattern).
for (let i = 0; i < 50; i++) steins.check(snippet + `// edit ${i}\n`);
assert(true, "50 repeated checks on one instance");

process.exit(failures === 0 ? 0 : 1);
