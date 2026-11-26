// The playground's PHP thread (issue #64 S2): one PHP module, owned here and
// nowhere else, so the runtime and every PHP call stay off the main thread and
// off the analysis thread.
//
// The builder's own Worker helper (createPhpInWorker) is not used: this worker
// must be one the main thread can terminate on a deadline, and it speaks the
// replay protocol below rather than HTTP.
//
// Protocol:
//   in  { seq }                     boot probe
//   out { seq, ready, version, intSize } | { seq, error }
//   in  { seq, requests: [key, …] } answer these pending keys
//   out { seq, answers: {key: result}, failures: [key], reasons: [text] }
//     | { seq, error }
//
// No deadline lives here. A PHP busy loop starves this thread's timers, so the
// only enforceable timeout is the main thread's: it races a deadline and
// terminates this worker. Nothing here needs to clean up after that.
import createPhp from "./vendor/php-wasm/php-web.mjs";
import { createEngine } from "./php-dispatch.mjs";

const runner = (async () => {
  const response = await fetch(new URL("./vendor/runner.php", import.meta.url));
  if (!response.ok) throw new Error(`runner.php: HTTP ${response.status}`);
  return response.text();
})();

const boot = async () => createEngine(await createPhp(), await runner);
let booted = boot();

self.onmessage = async (e) => {
  const { seq, requests } = e.data;
  try {
    const engine = await booted;
    if (!requests) {
      self.postMessage({ seq, ready: true, version: engine.version, intSize: engine.intSize });
      return;
    }
    try {
      const { answers, failures, reasons } = await engine.answerMany(requests);
      self.postMessage({ seq, answers, failures, reasons });
    } catch (err) {
      // A run that exhausts the C stack aborts the module, and an aborted module
      // rejects every later call: the next batch gets a fresh one.
      booted = boot();
      throw err;
    }
  } catch (err) {
    self.postMessage({ seq, error: String(err && err.message ? err.message : err) });
  }
};
