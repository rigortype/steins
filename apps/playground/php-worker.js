// The playground's PHP thread (issue #64 S2): one php-wasm instance, owned here
// and nowhere else, so the ~16 MB runtime and every PHP call stay off the main
// thread and off the analysis thread.
//
// This worker is hand-written on purpose: php-wasm 0.1.0 ships no PhpWorker.mjs
// (only a .d.mts stub — a packaging bug), and its php-tags entrypoints import a
// symbol the package does not export. PhpWeb + the flat ESM graph is the part
// that works, and it is all this needs.
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
import { PhpWeb } from "./vendor/php-wasm/PhpWeb.mjs";
import { createEngine } from "./php-dispatch.mjs";

// php-wasm's WEB build assumes a document. Its Emscripten runtime evaluates
// `specialHTMLTargets = [0, document, window]` while constructing the module —
// unguarded, so in a Worker it throws `document is not defined` before PHP ever
// starts. Those globals feed the HTML5/SDL event-target machinery, which is
// dead code for a headless PHP run: nothing here creates a canvas, an audio
// context, or a DOM event. Two stubs are the whole shim. (The -node build has
// no such reference, which is why `smoke-replay.mjs` needs none of this.)
//
// It goes here rather than in php-dispatch.mjs because it is a property of the
// host, not of the dispatch: the runtime is constructed below, and this file is
// the only place that constructs a PhpWeb.
if (typeof self.window === "undefined") self.window = self;
if (typeof self.document === "undefined") {
  self.document = {
    querySelector: () => null,
    getElementById: () => null,
    fullscreenEnabled: false,
    webkitFullscreenEnabled: false,
    addEventListener() {},
    removeEventListener() {},
  };
}

const booted = (async () => {
  const response = await fetch(new URL("./vendor/runner.php", import.meta.url));
  if (!response.ok) throw new Error(`runner.php: HTTP ${response.status}`);
  const runner = await response.text();
  // 8.5 is explicit: the constructor's default is 8.4.
  return createEngine(new PhpWeb({ version: "8.5" }), runner);
})();

self.onmessage = async (e) => {
  const { seq, requests } = e.data;
  try {
    const engine = await booted;
    if (!requests) {
      self.postMessage({ seq, ready: true, version: engine.version, intSize: engine.intSize });
      return;
    }
    const answers = Object.create(null);
    const failures = [];
    const reasons = [];
    for (const key of requests) {
      try {
        answers[key] = await engine.answer(key);
      } catch (err) {
        failures.push(key);
        reasons.push(String(err && err.message ? err.message : err));
      }
    }
    self.postMessage({ seq, answers, failures, reasons });
  } catch (err) {
    self.postMessage({ seq, error: String(err && err.message ? err.message : err) });
  }
};
