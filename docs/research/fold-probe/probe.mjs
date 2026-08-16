#!/usr/bin/env node
// Differential width probe harness (issue #354, the ADR-0066 discipline).
//
// Runs a list of `(name, args)` fold requests through the SAME `steins_handle`
// dispatch core on both machines:
//
//   * 64-bit — the local `php` over the runner's own NDJSON protocol.
//   * 32-bit — vendored php-wasm 0.1.0 (PHP 8.5.2, PHP_INT_SIZE = 4) through
//     PhpNode, over the dispatch `apps/playground/php-dispatch.mjs` performs
//     (base64 request, `steins_handle`, the runner's own encode flags),
//     reimplemented here for one reason only: it must hand back the response
//     text UNPARSED.
//
// Comparing parsed JSON would be unsound. Array elements cross the seam bare
// (`steins_encode_array` carries no per-element type tag), so an `int` on one
// engine and a `float` on the other are distinguished ONLY by
// JSON_PRESERVE_ZERO_FRACTION's `3000000000` vs `3000000000.0` — and
// `JSON.parse` erases exactly that. The comparison is therefore on the response
// bytes, which is also what the Rust decoder actually reads.
//
// Usage: node probe.mjs <tuples.json> <repo-root> [--json out.json] [--strict]
//
// Driven by `cargo xtask fold-probe`, which generates the tuples from the mined
// parameter facts and formats the disposition table. Runnable by hand too.
// tuples.json: [{"name":"range","args":[1,3],"note":"…"}, …]
// args are in the runner's wire form (scalars bare; arrays as
// {"__steins_array": [[key, value], …]}).

import { readFile, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
// The repo root is argv[3] — `cargo xtask fold-probe` passes its own, so this
// runs from any worktree instead of the one it was written in.
const REPO = process.argv[3] ?? process.cwd();
const RUNNER = `${REPO}/crates/steins-sidecar/runner.php`;
const PHP_WASM = `${REPO}/apps/playground/vendor/php-wasm/PhpNode.mjs`;
const { PhpNode } = await import(PHP_WASM).catch(() => {
  console.error(
    `php-wasm is not vendored at ${PHP_WASM}\n` +
      "  run `sh apps/playground/build.sh` first — its second half vendors the\n" +
      "  exact pinned engine the browser gets (gitignored build product).",
  );
  process.exit(2);
});
const JSON_FLAGS = "JSON_PRESERVE_ZERO_FRACTION|JSON_UNESCAPED_SLASHES|JSON_UNESCAPED_UNICODE";

// JS has one number type, so `3000000000.0` round-trips through JSON.stringify
// as `3000000000` and `json_decode` hands the runner an INT — a different probe
// than the one written, and one the range guard would have refused. A tuple
// spells a genuine float argument as the string "@@3000000000.0@@", which is
// substituted back into the request text as a raw JSON token.
const RAW = /"@@(.*?)@@"/g;
const encodeRequest = (obj) => JSON.stringify(obj).replace(RAW, "$1");

// ── the 64-bit side: the runner as the native sidecar drives it ──────────────
function nativeEngine() {
  const child = spawn("php", [RUNNER], { stdio: ["pipe", "pipe", "ignore"] });
  const lines = createInterface({ input: child.stdout });
  const pending = [];
  lines.on("line", (line) => {
    const waiter = pending.shift();
    if (waiter) waiter(line);
  });
  // A resident runner can DIE mid-run: an argument that makes PHP allocate more
  // than its memory_limit is a fatal error, not an exception, and the process is
  // gone. Without this the pending promise never settles and the whole run hangs
  // — which reads as "slow", forever, and is how a probe run silently stops
  // being evidence. Answer the waiter with a marker instead; the caller counts
  // it as `engine-died`, which is neither agreement nor divergence.
  const died = JSON.stringify({ kind: "engine-died" });
  child.on("exit", (code, signal) => {
    while (pending.length) pending.shift()(`{"result":${died}}`);
    console.error(`!! the 64-bit runner exited (code=${code}, signal=${signal})`);
  });
  let id = 0;
  return {
    // Returns the raw `result` JSON text, sliced out of the response line by
    // re-encoding nothing: the runner writes `{"jsonrpc":"2.0","id":N,"result":…}`
    // with the result last, so the text after the `"result":` key to the final
    // `}` is the result verbatim.
    ask(method, params) {
      return new Promise((resolve) => {
        pending.push((line) => {
          const at = line.indexOf('"result":');
          resolve(line.slice(at + '"result":'.length, -1));
        });
        child.stdin.write(encodeRequest({ jsonrpc: "2.0", id: id++, method, params }) + "\n");
      });
    },
    close() {
      child.stdin.end();
    },
  };
}

// ── the 32-bit side: php-wasm, dispatched exactly as php-dispatch.mjs does ───
async function wasmEngine(runnerSource) {
  const php = new PhpNode({ version: "8.5" });
  let out = [];
  php.addEventListener("output", (e) => out.push(e.detail.join("")));
  php.addEventListener("error", () => {});
  const take = () => {
    const text = out.join("");
    out = [];
    return text;
  };
  await php.writeFile("/steins-runner.php", runnerSource);
  await php.run("<?php require '/steins-runner.php';");
  take();
  await php.run(
    "<?php echo json_encode(['version' => PHP_VERSION, 'int_size' => PHP_INT_SIZE]);",
  );
  const probe = JSON.parse(take().trim());
  return {
    version: probe.version,
    intSize: probe.int_size,
    async ask(method, params) {
      const key = encodeRequest({ method, params });
      const b64 = Buffer.from(key, "utf8").toString("base64");
      await php.run(
        `<?php $req = json_decode(base64_decode('${b64}'), true);\n` +
          `echo json_encode(steins_handle($req['method'], $req['params']), ${JSON_FLAGS});`,
      );
      return take().trim();
    },
  };
}

// ── comparison ───────────────────────────────────────────────────────────────
// A fold reply is one of value / throw / widen. Two replies AGREE when their
// response bytes match. A DECLINE is 64-bit answering a value where 32-bit does
// not (sound: the browser loses precision). A REVERSE is the opposite
// (unsound). A SILENT divergence is two values that differ (unsound).
function classify(wideText, narrowText) {
  if (wideText.includes('"engine-died"') || narrowText.includes('"engine-died"')) {
    return "engine-died";
  }
  if (wideText === narrowText) return "agree";
  const wide = JSON.parse(wideText);
  const narrow = JSON.parse(narrowText);
  if (wide.kind === "value" && narrow.kind === "value") return "silent";
  if (wide.kind === "value") return "decline";
  if (narrow.kind === "value") return "reverse";
  // Neither answered a value; they only spell the refusal differently. Not a
  // soundness hazard (both widen on the Rust side), but shown.
  return "agree-decline-differs";
}

const tuplesPath = process.argv[2];
// The call site's calling convention (#390): a portability verdict has to hold
// for whichever mode the request names, so a row is probed both ways.
const STRICT = process.argv.includes("--strict") || process.env.STRICT === "1";
const jsonOutFlag = process.argv.indexOf("--json");
const tuples = JSON.parse(await readFile(tuplesPath, "utf8"));

const native = nativeEngine();
const nativeEnv = JSON.parse(await native.ask("env", {}));
const runnerSource = await readFile(RUNNER, "utf8");
const wasm = await wasmEngine(runnerSource);

console.log(`64-bit: PHP ${nativeEnv.php_version}, PHP_INT_SIZE=${nativeEnv.int_size}`);
console.log(`32-bit: PHP ${wasm.version}, PHP_INT_SIZE=${wasm.intSize}`);
if (nativeEnv.int_size !== 8 || wasm.intSize !== 4) {
  console.error("the two engines are not the 64/32 pair this harness compares");
  process.exit(1);
}

// The fold seam's range guard, `fold_arg_fits_i32` in steins-infer: every
// INTEGER (values and array keys, recursively) must lie inside ±(2^31 − 1).
// Floats, strings, bools and null are unguarded. A tuple that fails this is not
// a probe — the 32-bit lane would never have been asked it — so the harness
// refuses to count one, which is the mistake a JS float literal silently makes.
const I32 = 2147483647;
const fitsI32 = (a) => {
  if (typeof a === "number") return Number.isInteger(a) ? Math.abs(a) <= I32 : true;
  if (typeof a === "string") return !/^@@-?\d+@@$/.test(a); // a raw INT token
  if (a && typeof a === "object" && a.__steins_array) {
    return a.__steins_array.every(
      ([k, v]) => (typeof k !== "number" || Math.abs(k) <= I32) && fitsI32(v),
    );
  }
  return true;
};

const rows = [];
const tally = new Map();
for (const t of tuples) {
  if (!t.args.every(fitsI32)) {
    console.log(`?? INADMISSIBLE ${t.name}(${t.args.map((a) => JSON.stringify(a)).join(", ")})`);
    tally.set("inadmissible", (tally.get("inadmissible") ?? 0) + 1);
    continue;
  }
  const params = { function: t.name, args: t.args, strict: STRICT };
  const wide = await native.ask("fold", params);
  let narrow;
  try {
    narrow = await wasm.ask("fold", params);
  } catch (err) {
    narrow = JSON.stringify({ kind: "widen", reason: `engine failure: ${err.message}` });
  }
  const verdict = classify(wide, narrow);
  tally.set(verdict, (tally.get(verdict) ?? 0) + 1);
  rows.push({ ...t, wide, narrow, verdict });
  const call = `${t.name}(${t.args.map((a) => JSON.stringify(a)).join(", ")})`;
  const mark = verdict === "agree" ? "  " : verdict === "decline" ? "· " : "!!";
  console.log(`${mark} ${verdict.padEnd(10)} ${call.length > 150 ? call.slice(0, 150) + "…" : call}`);
  if (verdict !== "agree") {
    console.log(`     64: ${wide.slice(0, 400)}`);
    console.log(`     32: ${narrow.slice(0, 400)}`);
  }
}

native.close();
console.log("\n--- tally ---");
for (const [k, v] of [...tally].sort()) console.log(`${k}: ${v}`);

const byName = new Map();
for (const r of rows) {
  const acc = byName.get(r.name) ?? { total: 0, silent: 0, reverse: 0, decline: 0 };
  acc.total++;
  if (r.verdict === "silent") acc.silent++;
  if (r.verdict === "reverse") acc.reverse++;
  if (r.verdict === "decline") acc.decline++;
  byName.set(r.name, acc);
}
console.log("\n--- per name (silent/reverse/decline of total) ---");
for (const [name, a] of byName) {
  console.log(`${name.padEnd(16)} ${a.total} (${a.silent}/${a.reverse}/${a.decline})`);
}

if (jsonOutFlag > 0) {
  await writeFile(process.argv[jsonOutFlag + 1], JSON.stringify(rows, null, 2));
}
process.exit(0);
