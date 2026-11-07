// Node.js entry (full build): the WebAssembly module loads synchronously on import.
import { makeNodeApi, loadRaw } from "./node-api.js";

const api = makeNodeApi(loadRaw("./dist/full-node/slidepdf_wasm.js"));
export const { convert, convertStream, convertFile, assemble, assembleStream, assembleFiles, split, fragmentHash, verifyFragment, search, ready } = api;
