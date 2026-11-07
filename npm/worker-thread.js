// Runs inside a worker_threads Worker spawned by pool.js. One job at a
// time; the pool owns timeouts, cancellation and recycling.
//
// The pool hands over an already compiled WebAssembly.Module, so this
// uses the "web" glue (which accepts one) rather than the Node glue (which
// compiles its own copy and would lose V8's shared optimised code).
import { parentPort, workerData } from "node:worker_threads";
import { makeNodeApi } from "./node-api.js";

const glue = await import(`./dist/${workerData.variant}-web/slidepdf_wasm.js`);
glue.initSync({ module: workerData.module });
// After a trap the instance cannot be rebuilt here: the job reports
// `dead` and the pool replaces the worker.
let dead = false;
const api = makeNodeApi({ get: () => glue, reload: () => { dead = true; } });
// Shared with the pool: non-zero means "stop after the current slide".
const stop = new Int32Array(workerData.stop);

parentPort.on("message", ({ id, op, args, memoryMb, progress }) => {
  Atomics.store(stop, 0, 0);
  const safeStats = () => {
    try { return api.memoryStats(); } catch { return { inUse: 0, peak: 0, limit: 0, linear: 0 }; }
  };
  try {
    if (dead) throw Object.assign(new Error("worker is dead after a trap"), { code: "worker" });
    api.setMemoryLimit(memoryMb ?? 0);
    let result;
    switch (op) {
      case "convertFile": {
        const [input, outDir, options] = args;
        result = api.convertFile(input, outDir, {
          ...options,
          shouldCancel: () => Atomics.load(stop, 0) !== 0,
          onProgress: progress ? (done, total) => parentPort.postMessage({ id, progress: { done, total } }) : undefined,
        });
        break;
      }
      case "inspectFile":
        result = api.inspectFile(...args);
        break;
      case "assembleFiles":
        result = api.assembleFiles(...args);
        break;
      default:
        throw new Error(`unknown op ${op}`);
    }
    parentPort.postMessage({ id, ok: true, result, memory: safeStats(), dead });
  } catch (e) {
    parentPort.postMessage({
      id,
      ok: false,
      error: { code: e.code ?? "convert", message: e.message ?? String(e) },
      // A trap took the instance's stats with it: use what the error captured.
      memory: e.peakBytes ? { ...safeStats(), peak: e.peakBytes } : safeStats(),
      dead,
    });
  }
});
