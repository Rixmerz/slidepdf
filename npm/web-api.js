// Browser / Workers / Deno API over a wasm-bindgen "web" module.
import { wrap, search, SlidepdfError } from "./common.js";

export function makeWebApi(init, raw, defaultWasmUrl) {
  let api = null;
  let loading = null;
  const need = () => {
    if (!api) throw new Error("slidepdf: call `await ready()` before using the converter");
    return api;
  };
  return {
    ready(input) {
      if (api) return Promise.resolve();
      if (!loading) loading = init({ module_or_path: input ?? defaultWasmUrl }).then(() => { api = wrap(raw); });
      return loading;
    },
    convert: (pptx, options) => need().convert(pptx, options),
    convertStream: (source, options, callbacks) => need().convertStream(source, options, callbacks),
    inspect: (pptx) => need().inspect(pptx),
    // After a memory-limit trap the instance is dead and ready() cannot
    // rebuild it: in browsers and Workers, set a limit only where the
    // isolate can be thrown away (a Worker per job, a reloaded page).
    setMemoryLimit: (mb) => need().setMemoryLimit(mb),
    memoryStats: () => need().memoryStats(),
    SlidepdfError,
    assemble: (parts) => need().assemble(parts),
    assembleStream: (next, onPdf, opts) => need().assembleStream(next, onPdf, opts),
    split: (pdf) => need().split(pdf),
    fragmentHash: (pdf) => need().fragmentHash(pdf),
    verifyFragment: (pdf, hash) => need().verifyFragment(pdf, hash),
    search,
  };
}
