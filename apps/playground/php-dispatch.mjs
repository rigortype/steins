// The PHP half of the replay loop: run the sidecar runner inside a
// php-wasm-builder module and answer a batch of request keys with it.
//
// Shared by `php-worker.js` (browser, php-web.mjs) and `smoke-replay.mjs`
// (Node, php-node.mjs): both builds expose the same `FS`, `runCode` and
// `runFile`, so the dispatch lives here exactly once and the smoke suite
// exercises the code the browser runs.
//
// The runner runs as the native sidecar runs it: the batch goes in as NDJSON on
// stdin and comes back as one JSON-RPC response line per request on stdout.
// Diagnostics go to stderr (the runner routes them there via log_errors +
// error_log, ADR-0066 §5), which comes back as its own string and is
// DISCARDED: a notice must never reach a response.
//
// Every `runFile` starts from fresh PHP state, so nothing depends on what an
// earlier batch defined. A fatal ends the run mid-batch, exactly as it ends the
// native child: the first request without a response is the one that died, it
// is reported as a failure, and the requests after it are asked again.
//
// There is no timeout here, and there cannot be: a PHP busy loop starves JS
// timers in the same thread. The caller enforces a deadline from another thread.

const RUNNER_PATH = "/steins-runner.php";

const PROBE = "echo json_encode(['version' => PHP_VERSION, 'int_size' => PHP_INT_SIZE]);";

/**
 * Install `runnerSource` in a php-wasm-builder module and return an answerer.
 *
 * @param php          a started module (`await createPhp()` from php-web.mjs or
 *                     php-node.mjs).
 * @param runnerSource the text of crates/steins-sidecar/runner.php, unmodified.
 * @returns { version, intSize, answerMany(keys) }
 */
export async function createEngine(php, runnerSource) {
  php.FS.writeFile(RUNNER_PATH, runnerSource);

  const probe = await php.runCode(PROBE);
  let info;
  try {
    info = JSON.parse(probe.stdout);
  } catch {
    throw new Error(`the PHP engine's boot probe did not answer (${JSON.stringify(probe.stdout.slice(0, 200))})`);
  }

  // One runner run over `keys`. Returns the results by position; a position
  // with no response line is one the run never reached.
  async function run(keys) {
    let stdin = "";
    keys.forEach((key, id) => {
      const request = JSON.parse(key);
      if (typeof request !== "object" || request === null || typeof request.method !== "string") {
        throw new Error(`not a request key: ${key}`);
      }
      // The key IS the request (ADR-0066 §2); the id is only its position here.
      stdin += JSON.stringify({ jsonrpc: "2.0", id, method: request.method, params: request.params ?? {} }) + "\n";
    });
    const { stdout } = await php.runFile(RUNNER_PATH, { stdin });
    const results = new Map();
    for (const line of stdout.split("\n")) {
      if (line.trim() === "") continue;
      let response;
      try {
        response = JSON.parse(line);
      } catch {
        // A fatal's own text lands on stdout after the last whole response.
        continue;
      }
      if (Number.isInteger(response.id) && "result" in response) results.set(response.id, response.result);
    }
    return results;
  }

  return {
    version: info.version,
    intSize: info.int_size,

    /**
     * Answer pending keys. Each answer is the raw JSON-RPC `result` object
     * `steins_handle` produced, stored under its key.
     *
     * @returns { answers: {key: result}, failures: [key], reasons: [text] }
     */
    async answerMany(keys) {
      const answers = Object.create(null);
      const failures = [];
      const reasons = [];
      let rest = [...new Set(keys)];
      while (rest.length > 0) {
        const results = await run(rest);
        const died = rest.findIndex((_, id) => !results.has(id));
        rest.forEach((key, id) => {
          if (results.has(id)) answers[key] = results.get(id);
        });
        if (died < 0) break;
        failures.push(rest[died]);
        reasons.push("the engine ended the run on this request (a PHP fatal)");
        rest = rest.slice(died + 1).filter((key) => !(key in answers));
      }
      return { answers, failures, reasons };
    },
  };
}
