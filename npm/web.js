// Browser / Workers / Deno entry (full build). Call `await ready()` once.
// In Cloudflare Workers, import the .wasm and pass the module:
//   import wasm from "slidepdf/dist/full-web/slidepdf_wasm_bg.wasm";
//   await ready(wasm);
import init, * as raw from "./dist/full-web/slidepdf_wasm.js";
import { makeWebApi } from "./web-api.js";

const api = makeWebApi(init, raw, new URL("./dist/full-web/slidepdf_wasm_bg.wasm", import.meta.url));
export const { ready, convert, convertStream, assemble, assembleStream, split, fragmentHash, verifyFragment, search } = api;
