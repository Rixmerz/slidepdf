// Node.js entry (full build): the WebAssembly module loads on first use.
import { makeNodeApi } from "./node-api.js";
import { makePool, convertFileInWorker as inWorker } from "./pool.js";

const GLUE = "./dist/full-node/slidepdf_wasm.js";
const api = makeNodeApi(GLUE);
export const { convert, convertStream, convertFile, inspect, inspectFile, assemble, assembleStream, assembleFiles, split, fragmentHash, verifyFragment, search, setMemoryLimit, memoryStats, ready } = api;
export { SlidepdfError } from "./common.js";

/** A pool of worker threads with a memory cap, timeout and cancellation per job. */
export const createPool = (options) => makePool("full", options);
/** One conversion in its own worker thread, so its memory is released afterwards. */
export const convertFileInWorker = (input, outDir, options) => inWorker("full", input, outDir, options);
