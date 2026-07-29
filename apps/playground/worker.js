// The playground's analysis thread (ADR-0065 §5): the wasm module lives here so
// a multi-millisecond check never blocks typing. One instance serves the whole
// session — the smoke suite pins repeated checks on one instance as the
// supported pattern.
//
// Protocol: { seq, source, profile, wantAnnotate } in,
//           { seq, check, annotate } out. `seq` is the main thread's stale-
// response guard; the worker itself is a pure function of the request.
import { loadSteins } from "./steins.mjs";

const ready = (async () => {
  const resp = await fetch(new URL("./steins_wasm.wasm", import.meta.url));
  return loadSteins(resp);
})();

self.onmessage = async (e) => {
  const { seq, source, profile, wantAnnotate } = e.data;
  try {
    const steins = await ready;
    const check = steins.check(source, profile || "");
    const annotate = wantAnnotate ? steins.annotate(source) : null;
    self.postMessage({ seq, check, annotate });
  } catch (err) {
    self.postMessage({ seq, check: { ok: false, error: String(err) }, annotate: null });
  }
};
