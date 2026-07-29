// The JS half of the steins-wasm C ABI (ADR-0065). ~60 lines, dependency-free,
// runs in a browser, a Web Worker, or Node — the whole reason the module ships
// a hand-rolled ABI instead of wasm-bindgen glue.
//
// Contract (crates/steins-wasm/src/lib.rs):
//   sw_alloc(len) -> ptr          caller writes UTF-8 into wasm memory
//   sw_check(sp, sl, pp, pl)      envelope -> result buffer
//   sw_annotate(sp, sl)           envelope -> result buffer
//   sw_result_ptr() / sw_result_len()
//   sw_dealloc(ptr, len)
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
  };
}
