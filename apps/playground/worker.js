// The playground's analysis thread (ADR-0065 §5): the wasm module lives here so
// a multi-millisecond check never blocks typing. One instance serves the whole
// session — the smoke suite pins repeated checks on one instance as the
// supported pattern.
//
// Protocol: { seq, source, profile, wantAnnotate } in,
//           { seq, check, annotate } out. `seq` is the main thread's stale-
// response guard; the worker itself is a pure function of the request.
//
// With a `table` in the request (issue #64), the same call runs the REPLAY pair
// instead (ADR-0066): one iteration of check + annotate over the supplied
// answers, replying with the union of what neither could answer as `pending`.
// The loop that answers it lives on the main thread, which owns the PHP worker.
import { loadSteins } from "./steins.mjs";

const ready = (async () => {
  const resp = await fetch(new URL("./steins_wasm.wasm", import.meta.url));
  return loadSteins(resp);
})();

self.onmessage = async (e) => {
  const { seq, source, profile, wantAnnotate, table } = e.data;
  try {
    const steins = await ready;
    if (!table) {
      const check = steins.check(source, profile || "");
      const annotate = wantAnnotate ? steins.annotate(source) : null;
      self.postMessage({ seq, check, annotate });
      return;
    }
    // Both lanes ride ONE table: asking annotate in the same iteration collects
    // its questions too, so the loop converges in the number of rounds check
    // alone would have needed.
    const check = steins.checkReplay(source, table, profile || "");
    let annotate = null;
    const pending = new Set(check.ok ? check.pending : []);
    if (check.ok && wantAnnotate) {
      annotate = steins.annotateReplay(source, table);
      if (annotate.ok) for (const key of annotate.pending) pending.add(key);
    }
    self.postMessage({ seq, check, annotate, pending: [...pending] });
  } catch (err) {
    self.postMessage({ seq, check: { ok: false, error: String(err) }, annotate: null, pending: [] });
  }
};
