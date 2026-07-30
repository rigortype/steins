// The PHP half of the replay loop: boot a php-wasm instance on the sidecar
// runner and answer one request key with it.
//
// Shared by `php-worker.js` (browser, PhpWeb) and `smoke-replay.mjs` (Node,
// PhpNode) — both php-wasm hosts expose the same `writeFile`/`run` surface and
// the same `output`/`error` event streams, so the dispatch lives here exactly
// once and the smoke suite exercises the code the browser runs.
//
// Two spike facts are load-bearing:
//
//   * `exec()` is unusable — on a parse error it returns a stale value and folds
//     diagnostics into the return. Everything here goes through `run()` plus the
//     output stream.
//   * Diagnostics arrive on the ERROR stream (the runner routes them there via
//     log_errors + error_log, ADR-0066 §5). That stream is DISCARDED: a notice
//     must never reach a response.
//
// There is no timeout here, and there cannot be: a PHP busy loop starves JS
// timers in the same thread. The caller enforces a deadline from another thread.

const RUNNER_PATH = "/steins-runner.php";
const encoder = new TextEncoder();

// The runner's own encode flags, verbatim. The table value has to be
// byte-equivalent to what the native NDJSON loop would have produced.
const JSON_FLAGS = "JSON_PRESERVE_ZERO_FRACTION|JSON_UNESCAPED_SLASHES|JSON_UNESCAPED_UNICODE";

function base64Utf8(text) {
  const bytes = encoder.encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/**
 * Boot `runnerSource` inside a php-wasm instance and return an answerer.
 *
 * @param php          a php-wasm instance (PhpWeb in a worker, PhpNode in Node).
 * @param runnerSource the text of crates/steins-sidecar/runner.php, unmodified.
 * @returns { version, intSize, answer(key) }
 */
export async function createEngine(php, runnerSource) {
  let out = [];
  php.addEventListener("output", (e) => out.push(e.detail.join("")));
  php.addEventListener("error", () => {}); // diagnostics: read and dropped
  const take = () => {
    const text = out.join("");
    out = [];
    return text;
  };

  await php.writeFile(RUNNER_PATH, runnerSource);
  // The runner runs UNMODIFIED, stdin loop and all: php-wasm's stdin is empty,
  // so `fgets` hits EOF on the first read and the loop falls through, leaving
  // every function defined. Definitions persist across later `run()` calls on
  // this instance (they would not survive `refresh()`, which is never called).
  await php.run(`<?php require '${RUNNER_PATH}';`);
  take();

  await php.run(
    `<?php echo json_encode(['ready' => function_exists('steins_handle'), 'version' => PHP_VERSION, 'int_size' => PHP_INT_SIZE]);`,
  );
  const probeText = take().trim();
  let probe;
  try {
    probe = JSON.parse(probeText);
  } catch {
    throw new Error(`the PHP engine's boot probe did not answer (${JSON.stringify(probeText.slice(0, 200))})`);
  }
  if (probe.ready !== true) throw new Error("the sidecar runner did not define steins_handle");

  return {
    version: probe.version,
    intSize: probe.int_size,

    /**
     * Answer one pending key. The key IS the request — `{"method":…,"params":…}`
     * (ADR-0066 §2) — and is handed to the engine verbatim; the caller echoes it
     * back as the table key.
     *
     * @returns the raw JSON-RPC `result` object `steins_handle` produced.
     */
    async answer(key) {
      const request = JSON.parse(key);
      if (typeof request !== "object" || request === null || typeof request.method !== "string") {
        throw new Error(`not a request key: ${key}`);
      }
      // The request travels as a base64 literal: no PHP string escaping to get
      // wrong, and the source may contain quotes, newlines, or any byte.
      await php.run(
        `<?php $req = json_decode(base64_decode('${base64Utf8(key)}'), true);\n` +
          `echo json_encode(steins_handle($req['method'], isset($req['params']) && is_array($req['params']) ? $req['params'] : []), ${JSON_FLAGS});`,
      );
      const text = take().trim();
      try {
        return JSON.parse(text);
      } catch {
        throw new Error(`the engine's answer was not JSON (${JSON.stringify(text.slice(0, 200))})`);
      }
    },
  };
}
