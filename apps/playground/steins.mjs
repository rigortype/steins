// The JS half of the steins-wasm C ABI (ADR-0065). ~60 lines, dependency-free,
// runs in a browser, a Web Worker, or Node — the whole reason the module ships
// a hand-rolled ABI instead of wasm-bindgen glue.
//
// Contract (crates/steins-wasm/src/lib.rs):
//   sw_alloc(len) -> ptr          caller writes UTF-8 into wasm memory
//   sw_check(sp, sl, pp, pl)      envelope -> result buffer
//   sw_annotate(sp, sl)           envelope -> result buffer
//   sw_check_replay(sp, sl, pp, pl, tp, tl)   + a fold-answer table
//   sw_annotate_replay(sp, sl, tp, tl)        + a fold-answer table
//   sw_result_ptr() / sw_result_len()
//   sw_dealloc(ptr, len)
//
// The replay pair (ADR-0066) takes a JSON object mapping request key -> raw
// JSON-RPC `result`, and returns the envelope with two extra keys: `boot` — the
// engine surface as the analysis' own gates see it (version, PHP_INT_SIZE, fold
// lane, whether curated rows and the absence family are live, and the folds a
// narrow engine is refused), which is what makes the precision boundary
// renderable — and `pending`: the requests the run could not answer. A NON-EMPTY
// `pending` means the results are
// NoFold-degraded and must NOT be rendered — answer the pending keys (each parses
// as {"method", "params"}), put the answers back under the same key strings, and
// call again. The answered set strictly grows, so the loop terminates; the
// iteration cap is the caller's, and exhausting it means falling back to the
// non-replay call, never to showing a half-converged run.
//
// Views into wasm memory are recreated after every call: a call can grow the
// memory, and growth detaches every existing ArrayBuffer view.

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function loadSteins(source) {
  // Accept raw bytes (Node: fs.readFile) or a Response (browser: fetch), using
  // streaming instantiation when the environment offers it.
  let instance;
  if (typeof Response !== "undefined" && source instanceof Response) {
    ({ instance } = await WebAssembly.instantiateStreaming(source, {}));
  } else {
    ({ instance } = await WebAssembly.instantiate(source, {}));
  }
  const ex = instance.exports;

  function writeString(s) {
    const bytes = encoder.encode(s);
    const ptr = ex.sw_alloc(bytes.length);
    new Uint8Array(ex.memory.buffer, ptr, bytes.length).set(bytes);
    return { ptr, len: bytes.length };
  }

  function readResult() {
    const ptr = ex.sw_result_ptr();
    const len = ex.sw_result_len();
    const bytes = new Uint8Array(ex.memory.buffer, ptr, len);
    return JSON.parse(decoder.decode(bytes));
  }

  return {
    check(source, profile = "") {
      const src = writeString(source);
      const prof = writeString(profile);
      try {
        ex.sw_check(src.ptr, src.len, prof.ptr, prof.len);
        return readResult();
      } finally {
        ex.sw_dealloc(src.ptr, src.len);
        ex.sw_dealloc(prof.ptr, prof.len);
      }
    },
    annotate(source) {
      const src = writeString(source);
      try {
        ex.sw_annotate(src.ptr, src.len);
        return readResult();
      } finally {
        ex.sw_dealloc(src.ptr, src.len);
      }
    },
    // One replay iteration. `table` maps request key -> raw JSON-RPC `result`;
    // `{}` starts a loop. The envelope's `pending` says whether it is finished.
    checkReplay(source, table = {}, profile = "") {
      const src = writeString(source);
      const prof = writeString(profile);
      const tbl = writeString(JSON.stringify(table));
      try {
        ex.sw_check_replay(src.ptr, src.len, prof.ptr, prof.len, tbl.ptr, tbl.len);
        return readResult();
      } finally {
        ex.sw_dealloc(src.ptr, src.len);
        ex.sw_dealloc(prof.ptr, prof.len);
        ex.sw_dealloc(tbl.ptr, tbl.len);
      }
    },
    annotateReplay(source, table = {}) {
      const src = writeString(source);
      const tbl = writeString(JSON.stringify(table));
      try {
        ex.sw_annotate_replay(src.ptr, src.len, tbl.ptr, tbl.len);
        return readResult();
      } finally {
        ex.sw_dealloc(src.ptr, src.len);
        ex.sw_dealloc(tbl.ptr, tbl.len);
      }
    },
  };
}
