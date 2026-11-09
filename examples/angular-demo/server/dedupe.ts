// Conversion dedupe for the demo API: an on-disk LRU of finished results and a
// registry that lets identical uploads share one running job.
// Kept free of any slidepdf import so it can be tested without the WebAssembly build.

import { renameSync } from "node:fs";
import { mkdir, readdir, rm, stat } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { join } from "node:path";

export type CacheResult = "hit" | "miss" | "joined";

/** A pinned cache entry: it cannot be evicted until every holder has released it. */
export interface Lease {
  dir: string;
  release(): void;
}

interface Entry {
  bytes: number;
  pins: number;
}

/** Total size of the files directly inside `dir` (a result folder is flat). */
export async function dirBytes(dir: string): Promise<number> {
  let total = 0;
  for (const name of await readdir(dir)) total += (await stat(join(dir, name))).size;
  return total;
}

/**
 * Finished conversions on disk, keyed by content hash and preset, evicted
 * least recently used first once they pass `maxBytes`. One server per `root`:
 * the folder is not safe to share between processes.
 */
export async function createResultCache(root: string, maxBytes: number) {
  const entriesDir = join(root, "entries");
  // Staging area on the same filesystem, so a finished job moves into place with one rename.
  const tmpDir = join(root, "tmp");
  await rm(tmpDir, { recursive: true, force: true });
  await mkdir(tmpDir, { recursive: true });
  await mkdir(entriesDir, { recursive: true });

  // Map order is recency order, oldest first.
  const entries = new Map<string, Entry>();
  let total = 0;

  // Entries survive a restart; their recency is rebuilt from the folder mtimes.
  const found = [];
  for (const name of await readdir(entriesDir)) {
    const dir = join(entriesDir, name);
    found.push({ name, mtime: (await stat(dir)).mtimeMs, bytes: await dirBytes(dir) });
  }
  for (const f of found.sort((a, b) => a.mtime - b.mtime)) {
    entries.set(f.name, { bytes: f.bytes, pins: 0 });
    total += f.bytes;
  }

  const entryDir = (key: string) => join(entriesDir, key);

  function evict() {
    for (const [key, e] of entries) {
      if (total <= maxBytes) return;
      if (e.pins > 0) continue;
      entries.delete(key);
      total -= e.bytes;
      // Rename first so the key is free at once; the slow delete happens in the background.
      const dead = join(tmpDir, `dead-${randomUUID()}`);
      renameSync(entryDir(key), dead);
      rm(dead, { recursive: true, force: true }).catch((err) => console.error(`cache: could not delete ${dead}`, err));
    }
  }

  function lease(key: string, e: Entry): Lease {
    let released = false;
    return {
      dir: entryDir(key),
      release() {
        if (released) return;
        released = true;
        e.pins--;
        evict();
      },
    };
  }

  evict();

  return {
    tmpDir,
    key: (sha256: string, preset: string) => `${sha256}-${preset}`,
    bytes: () => total,
    /** Pins and returns the entry, marking it most recently used; undefined on a miss. */
    acquire(key: string): Lease | undefined {
      const e = entries.get(key);
      if (!e) return undefined;
      entries.delete(key);
      entries.set(key, e);
      e.pins++;
      return lease(key, e);
    },
    /**
     * Moves a finished result folder into the cache, pinned `holds` times. The
     * returned lease must be released exactly `holds` times, once per holder
     * (unlike `acquire`, whose lease guards against a second release).
     */
    add(key: string, stagingDir: string, bytes: number, holds: number): Lease {
      if (entries.has(key)) throw new Error(`cache entry ${key} already exists`);
      renameSync(stagingDir, entryDir(key));
      const e: Entry = { bytes, pins: holds };
      entries.set(key, e);
      total += bytes;
      evict();
      return {
        dir: entryDir(key),
        release() {
          e.pins--;
          evict();
        },
      };
    },
  };
}

export type ResultCache = Awaited<ReturnType<typeof createResultCache>>;

/** One request's place on a job. */
export interface Waiter<T> {
  /** True when the job was already running. */
  joined: boolean;
  /**
   * The job's result, or undefined when this waiter left before the job
   * finished (it was not counted in `holds`, so it must not release anything).
   */
  wait(): Promise<T | undefined>;
  /** The client went away. The job is cancelled only when the last waiter leaves. */
  leave(): void;
}

interface Job<T> {
  ac: AbortController;
  waiters: Set<InternalWaiter<T>>;
  settled: boolean;
  promise: Promise<T>;
}

interface InternalWaiter<T> extends Waiter<T> {
  counted: boolean;
}

/** Running jobs by key. Identical keys share one job; its result is committed once. */
export function createJobs<Staged, Result>() {
  const running = new Map<string, Job<Result>>();

  function begin(
    key: string,
    start: (signal: AbortSignal) => Promise<Staged>,
    commit: (staged: Staged, holds: number) => Result,
  ): Job<Result> {
    const job: Job<Result> = { ac: new AbortController(), waiters: new Set(), settled: false, promise: undefined as never };
    running.set(key, job);
    job.promise = (async () => {
      try {
        const staged = await start(job.ac.signal);
        // From here no request can join this key: it either hits the cache or starts a new job.
        job.settled = true;
        if (running.get(key) === job) running.delete(key);
        for (const w of job.waiters) w.counted = true;
        return commit(staged, job.waiters.size);
      } finally {
        job.settled = true;
        if (running.get(key) === job) running.delete(key);
      }
    })();
    return job;
  }

  return {
    /**
     * Joins the running job for `key`, or starts one with `start`. `commit`
     * runs synchronously when the job succeeds, with the number of waiters still attached.
     */
    attach(
      key: string,
      start: (signal: AbortSignal) => Promise<Staged>,
      commit: (staged: Staged, holds: number) => Result,
    ): Waiter<Result> {
      const existing = running.get(key);
      const job = existing ?? begin(key, start, commit);
      const waiter: InternalWaiter<Result> = {
        joined: existing !== undefined,
        counted: false,
        wait: () => job.promise.then((v) => (waiter.counted ? v : undefined)),
        leave() {
          if (job.settled || !job.waiters.delete(waiter)) return;
          if (job.waiters.size === 0) {
            // A cancelled job must not attract new requests while its worker winds down.
            if (running.get(key) === job) running.delete(key);
            job.ac.abort();
          }
        },
      };
      job.waiters.add(waiter);
      return waiter;
    },
  };
}
