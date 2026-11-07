// A pool of worker threads, one conversion per worker at a time, with a
// memory cap, a timeout and cancellation per job. This is the API for
// servers: a conversion that blows its budget fails on its own, the
// process stays up, and workers that grew past `recycleAboveMb` are
// replaced (WebAssembly memory never shrinks, so that is the only way to
// give it back).
//
// Timing stays flat across workers because the pool compiles the module
// once, eagerly, and hands the same compiled module to every worker: V8
// then shares the optimised code, so a fresh worker runs as fast as a warm
// one (measured: 4.9 s, 4.8 s, 4.8 s on a 123 MB deck for three fresh
// workers, against 6.7 s cold and 4.7 s warm without it).
import { readFileSync } from "node:fs";
import v8 from "node:v8";
import { Worker } from "node:worker_threads";
import { SlidepdfError } from "./common.js";

const MB = 1048576;

export function makePool(variant, {
  size = 1,
  timeoutMs = 0,
  memoryMb = 0,
  recycleAboveMb = 64,
  killGraceMs = 5000,
  eagerCompile = true,
} = {}) {
  if (!(size >= 1)) throw new RangeError("size must be at least 1");
  const workers = []; // { worker, stop, busy, highMb, job }
  const queue = [];
  let nextId = 1;
  let closed = false;
  let module = null;

  function compiled() {
    if (module) return module;
    if (eagerCompile) {
      // Process-wide V8 flag: compile every function up front instead of
      // on first call, so the first conversion is as fast as the rest.
      // Must be set before the module is compiled.
      try { v8.setFlagsFromString("--no-wasm-lazy-compilation"); } catch { /* best effort */ }
    }
    const url = new URL(`./dist/${variant}-web/slidepdf_wasm_bg.wasm`, import.meta.url);
    module = new WebAssembly.Module(readFileSync(url));
    return module;
  }

  function spawn() {
    const stop = new SharedArrayBuffer(4);
    const entry = { worker: null, stop: new Int32Array(stop), busy: false, highMb: 0, job: null };
    const worker = new Worker(new URL("./worker-thread.js", import.meta.url), {
      workerData: { variant, module: compiled(), stop },
    });
    entry.worker = worker;
    worker.on("message", (m) => onMessage(entry, m));
    worker.on("error", (e) => fail(entry, new SlidepdfError("worker", `worker crashed: ${e.message}`)));
    worker.on("exit", (code) => {
      if (entry.job) fail(entry, new SlidepdfError("worker", `worker exited with code ${code}`));
      drop(entry);
      pump();
    });
    workers.push(entry);
    return entry;
  }

  function drop(entry) {
    const i = workers.indexOf(entry);
    if (i >= 0) workers.splice(i, 1);
  }

  function retire(entry) {
    drop(entry);
    entry.worker.terminate().catch(() => {});
  }

  function fail(entry, error) {
    const job = settle(entry);
    if (job) job.reject(error);
  }

  function settle(entry) {
    const job = entry.job;
    entry.job = null;
    entry.busy = false;
    if (job) {
      clearTimeout(job.timer);
      clearTimeout(job.killer);
      job.signal?.removeEventListener("abort", job.onAbort);
    }
    return job;
  }

  function onMessage(entry, m) {
    const job = entry.job;
    if (!job || m.id !== job.id) return;
    if (m.progress) {
      job.onProgress?.(m.progress.done, m.progress.total);
      return;
    }
    settle(entry);
    // What the worker really holds is its linear memory, which only grows.
    const heldMb = Math.max(m.memory?.linear ?? 0, m.memory?.peak ?? 0) / MB;
    entry.highMb = Math.max(entry.highMb, heldMb);
    if (m.dead || entry.highMb > recycleAboveMb) retire(entry);
    if (m.ok) {
      job.resolve(m.result && typeof m.result === "object" && m.result.stats
        ? { ...m.result, stats: { ...m.result.stats, peakBytes: m.memory.peak } }
        : m.result);
    } else {
      const aborted = job.aborted && m.error.code === "cancelled";
      job.reject(new SlidepdfError(aborted ? "aborted" : m.error.code, aborted ? "conversion aborted" : m.error.message, { peakBytes: m.memory?.peak ?? 0 }));
    }
    pump();
  }

  function pump() {
    while (queue.length && !closed) {
      let entry = workers.find((w) => !w.busy);
      if (!entry) {
        if (workers.length >= size) return;
        entry = spawn();
      }
      const job = queue.shift();
      if (job.signal?.aborted) {
        job.reject(new SlidepdfError("aborted", "conversion aborted"));
        continue;
      }
      entry.busy = true;
      entry.job = job;
      Atomics.store(entry.stop, 0, 0);
      // Asks the worker to stop after the current slide; if it does not
      // answer within the grace period (one slide stuck in a huge image),
      // the worker is killed and replaced.
      const stopSoon = (reason) => {
        Atomics.store(entry.stop, 0, 1);
        job.killer = setTimeout(() => {
          const code = reason === "aborted" ? "aborted" : "timeout";
          const what = reason === "aborted" ? "conversion aborted" : `conversion exceeded ${job.timeoutMs} ms`;
          fail(entry, new SlidepdfError(code, `${what} (worker killed)`));
          retire(entry);
        }, killGraceMs);
      };
      if (job.timeoutMs > 0) job.timer = setTimeout(() => stopSoon("timeout"), job.timeoutMs);
      if (job.signal) {
        job.onAbort = () => { job.aborted = true; stopSoon("aborted"); };
        job.signal.addEventListener("abort", job.onAbort, { once: true });
      }
      entry.worker.postMessage({
        id: job.id,
        op: job.op,
        args: job.args,
        memoryMb: job.memoryMb,
        progress: Boolean(job.onProgress),
      });
    }
  }

  function run(op, args, { timeoutMs: t = timeoutMs, memoryMb: m = memoryMb, signal, onProgress } = {}) {
    if (closed) return Promise.reject(new SlidepdfError("closed", "pool is closed"));
    return new Promise((resolve, reject) => {
      // The in-worker deadline is the first line; the pool's timer is the backstop.
      if (op === "convertFile") args = [args[0], args[1], { ...(args[2] ?? {}), timeoutMs: t > 0 ? t : undefined }];
      queue.push({ id: nextId++, op, args, timeoutMs: t, memoryMb: m, signal, onProgress, resolve, reject, aborted: false });
      pump();
    });
  }

  return {
    convertFile: (input, outDir, options = {}) => {
      const { timeoutMs: t, memoryMb: m, signal, onProgress, ...opts } = options;
      return run("convertFile", [input, outDir, opts], { timeoutMs: t, memoryMb: m, signal, onProgress });
    },
    inspectFile: (input, options = {}) => run("inspectFile", [input], options),
    assembleFiles: (inputs, outPath, options = {}) => run("assembleFiles", [inputs, outPath], options),
    /** Spawns the workers now, so the first jobs do not wait for start-up. */
    warmUp() {
      compiled();
      while (workers.length < size) spawn();
    },
    stats: () => ({
      size,
      workers: workers.length,
      busy: workers.filter((w) => w.busy).length,
      queued: queue.length,
      highMb: workers.map((w) => Math.round(w.highMb * 10) / 10),
    }),
    async close() {
      closed = true;
      for (const job of queue.splice(0)) job.reject(new SlidepdfError("closed", "pool is closed"));
      await Promise.all(workers.splice(0).map((w) => w.worker.terminate().catch(() => {})));
    },
  };
}

/** One-off: a single worker for one conversion, closed afterwards. */
export async function convertFileInWorker(variant, input, outDir, options = {}) {
  const pool = makePool(variant, { size: 1 });
  try {
    return await pool.convertFile(input, outDir, options);
  } finally {
    await pool.close();
  }
}
