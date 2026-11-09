import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createJobs, createResultCache } from "./dedupe.ts";
import { createMetrics, outcomeOfCode } from "./metrics.ts";

// npm/dist is built from the Rust crate and git-ignored, so a fresh checkout
// cannot load slidepdf; the end-to-end tests then skip and the unit tests still run.
const built = existsSync(join(dirname(fileURLToPath(import.meta.resolve("slidepdf"))), "dist", "full-node"));
const e2e = { skip: built ? false : "slidepdf WebAssembly build (npm/dist) is missing" };

let child: ChildProcess;
let base: string;
let cacheDir: string;

async function freePort(): Promise<number> {
  const srv = createServer();
  await new Promise<void>((r) => srv.listen(0, r));
  const { port } = srv.address() as { port: number };
  await new Promise((r) => srv.close(r));
  return port;
}

const deckPath = join(import.meta.dirname, "..", "..", "..", "tests", "fixtures", "deck.pptx");
const CACHE = "x-slidepdf-cache";
const upload = (body: Uint8Array | string, preset: string, signal?: AbortSignal) =>
  fetch(`${base}/api/convert?preset=${preset}`, { method: "POST", body, signal });
const cacheCount = (text: string, result: string) => sample(text, `slidepdf_cache_total\\{result="${result}"\\}`);

/** Polls until `check` holds; the server gives no event to wait on. */
// Polls for up to 20 s: on a loaded machine the uploads that fill the queue
// take seconds to arrive, and a shorter wait failed for no reason.
async function until(check: () => Promise<boolean>, what: string) {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((r) => setTimeout(r, 10));
  }
  assert.fail(`timed out waiting for ${what}`);
}

const metrics = async () => (await fetch(`${base}/metrics`)).text();
const sample = (text: string, line: string) => Number(text.match(new RegExp(`^${line} (\\S+)$`, "m"))?.[1]);

before(async () => {
  if (!built) return;
  const port = await freePort();
  base = `http://localhost:${port}`;
  // A private cache folder, so a result cached by an earlier run cannot turn a miss into a hit.
  cacheDir = await mkdtemp(join(tmpdir(), "slidepdf-test-cache-"));
  child = spawn(process.execPath, [join(import.meta.dirname, "server.ts")], {
    env: { ...process.env, PORT: String(port), WORKERS: "1", CACHE_DIR: cacheDir },
    stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise<void>((resolve, reject) => {
    child.once("exit", () => reject(new Error("server exited before listening")));
    child.stdout!.on("data", (d) => String(d).includes("on http") && resolve());
  });
});

after(async () => {
  child?.kill();
  if (cacheDir) await rm(cacheDir, { recursive: true, force: true });
});

test("GET /metrics exposes Prometheus text with every required family", e2e, async () => {
  const res = await fetch(`${base}/metrics`);
  assert.match(res.headers.get("content-type")!, /^text\/plain; version=0\.0\.4/);
  const text = await res.text();
  for (const name of [
    "slidepdf_conversions_total",
    "slidepdf_conversion_duration_seconds_bucket",
    "slidepdf_conversion_peak_memory_megabytes_bucket",
    "slidepdf_slides_converted_total",
    "slidepdf_input_bytes_total",
    "slidepdf_workers_busy",
    "slidepdf_workers_idle",
    "slidepdf_workers_recycled_total",
    "slidepdf_queue_depth",
    "slidepdf_conversions_in_flight",
    "slidepdf_cache_total",
    "slidepdf_cache_bytes",
  ]) assert.ok(text.includes(name), name);
  for (const o of ["ok", "memory", "timeout", "aborted", "invalid", "error"]) {
    assert.equal(sample(text, `slidepdf_conversions_total\\{outcome="${o}"\\}`), 0, o);
  }
  assert.ok(text.endsWith("\n"));
});

test("GET /metrics counts a good deck and a bad one", e2e, async () => {
  const deck = await readFile(deckPath);
  const ok = await fetch(`${base}/api/convert`, { method: "POST", body: deck });
  assert.equal(ok.status, 200);
  const { sizes, manifest } = (await ok.json()) as { sizes: { pptx: number }; manifest: { count: number } };
  const bad = await fetch(`${base}/api/convert`, { method: "POST", body: "not a pptx" });
  assert.equal(bad.status, 422);

  const text = await metrics();
  assert.equal(sample(text, 'slidepdf_conversions_total\\{outcome="ok"\\}'), 1);
  assert.equal(sample(text, 'slidepdf_conversions_total\\{outcome="invalid"\\}'), 1);
  assert.equal(sample(text, "slidepdf_slides_converted_total"), manifest.count);
  assert.equal(sample(text, "slidepdf_input_bytes_total"), sizes.pptx);
  assert.equal(sample(text, "slidepdf_conversion_duration_seconds_count"), 2);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="\\+Inf"\\}'), 2);
  assert.equal(sample(text, "slidepdf_conversions_in_flight"), 0);
  assert.equal(sample(text, "slidepdf_workers_busy"), 0);
  assert.equal(sample(text, "slidepdf_queue_depth"), 0);
});

test("a second upload of the same deck is a cache hit", e2e, async () => {
  const deck = await readFile(deckPath);
  const before = await metrics();
  const first = await upload(deck, "light");
  assert.equal(first.status, 200);
  assert.equal(first.headers.get(CACHE), "miss");
  const a = (await first.json()) as { id: string; manifest: { source: string; slides: { file: string; hash: string }[] } };

  const second = await fetch(`${base}/api/convert?preset=light&name=renamed.pptx`, { method: "POST", body: deck });
  assert.equal(second.status, 200);
  assert.equal(second.headers.get(CACHE), "hit");
  const b = (await second.json()) as typeof a & { sizes: { ms: number } };
  assert.notEqual(b.id, a.id, "each upload gets its own deck folder");
  assert.deepEqual(b.manifest.slides, a.manifest.slides);
  assert.equal(b.manifest.source, "renamed.pptx");
  assert.equal(b.sizes.ms, 0, "nothing was converted");

  // The copied fragments are served and verified like any other deck.
  const fragment = await fetch(`${base}/store/${b.id}/${b.manifest.slides[0].file}`);
  assert.equal(fragment.status, 200);
  assert.equal((await fetch(`${base}/api/decks/${b.id}/assemble`)).status, 200);

  const after = await metrics();
  assert.equal(cacheCount(after, "hit") - cacheCount(before, "hit"), 1);
  assert.equal(cacheCount(after, "miss") - cacheCount(before, "miss"), 1);
  assert.equal(sample(after, 'slidepdf_conversions_total\\{outcome="ok"\\}') - sample(before, 'slidepdf_conversions_total\\{outcome="ok"\\}'), 1);
  assert.ok(sample(after, "slidepdf_cache_bytes") > 0);
});

/**
 * Fills the single worker's queue with decks of their own (a few trailing bytes
 * change the hash), so a job started afterwards waits long enough to be joined.
 */
async function busyQueue(deck: Buffer, tag: string, count = 12) {
  const blockers = Array.from({ length: count }, (_, i) => upload(Buffer.concat([deck, Buffer.from(`${tag}-${i}`)]), "light"));
  await until(async () => sample(await metrics(), "slidepdf_queue_depth") >= 4, "the queue to fill");
  return blockers;
}

test("concurrent uploads of the same deck run one conversion", e2e, async () => {
  const deck = await readFile(deckPath);
  const before = await metrics();
  const blockers = await busyQueue(deck, "join");

  const results = await Promise.all([upload(deck, "print"), upload(deck, "print"), upload(deck, "print")]);
  assert.deepEqual(results.map((r) => r.status), [200, 200, 200]);
  assert.deepEqual(results.map((r) => r.headers.get(CACHE)).sort(), ["joined", "joined", "miss"]);
  const ids = await Promise.all(results.map(async (r) => ((await r.json()) as { id: string }).id));
  assert.equal(new Set(ids).size, 3, "every request gets its own deck folder");
  assert.deepEqual((await Promise.all(blockers)).map((r) => r.status), blockers.map(() => 200));

  const after = await metrics();
  assert.equal(cacheCount(after, "joined") - cacheCount(before, "joined"), 2);
  assert.equal(cacheCount(after, "miss") - cacheCount(before, "miss"), blockers.length + 1);
  // The three requests cost one conversion on top of the blockers'.
  assert.equal(sample(after, 'slidepdf_conversions_total\\{outcome="ok"\\}') - sample(before, 'slidepdf_conversions_total\\{outcome="ok"\\}'), blockers.length + 1);
  assert.equal(sample(after, "slidepdf_conversions_in_flight"), 0);
});

test("a request that leaves a shared job does not cancel it for the others", e2e, async () => {
  const deck = await readFile(deckPath);
  const blockers = await busyQueue(deck, "leave");
  const copy = Buffer.concat([deck, Buffer.from("shared")]);
  const gone = new AbortController();
  const joinedBefore = cacheCount(await metrics(), "joined");
  const missBefore = cacheCount(await metrics(), "miss");
  const leaving = upload(copy, "light", gone.signal).catch((e) => e);
  await until(async () => cacheCount(await metrics(), "miss") > missBefore, "the first request to start its job");
  const staying = upload(copy, "light");
  await until(async () => cacheCount(await metrics(), "joined") > joinedBefore, "the second request to join");
  gone.abort();
  assert.equal(((await leaving) as Error).name, "AbortError");
  const res = await staying;
  assert.equal(res.status, 200);
  assert.equal(res.headers.get(CACHE), "joined");
  await Promise.all(blockers);
  assert.equal(sample(await metrics(), 'slidepdf_conversions_total\\{outcome="aborted"\\}'), 0);
});

test("the job is cancelled when the last request leaves", e2e, async () => {
  const deck = await readFile(deckPath);
  const blockers = await busyQueue(deck, "cancel");
  const missBefore = cacheCount(await metrics(), "miss");
  const gone = new AbortController();
  const only = upload(Buffer.concat([deck, Buffer.from("alone")]), "light", gone.signal).catch((e) => e);
  await until(async () => cacheCount(await metrics(), "miss") > missBefore, "the request to start its job");
  gone.abort();
  assert.equal(((await only) as Error).name, "AbortError");
  await until(async () => sample(await metrics(), 'slidepdf_conversions_total\\{outcome="aborted"\\}') === 1, "the job to be cancelled");
  assert.deepEqual((await Promise.all(blockers)).map((r) => r.status), blockers.map(() => 200));
});

test("GET /metrics counts hits, misses and joins and the cache size", e2e, async () => {
  const text = await metrics();
  assert.equal(cacheCount(text, "hit"), 1);
  assert.equal(cacheCount(text, "joined"), 3);
  assert.ok(cacheCount(text, "miss") >= 4);
  assert.match(text, /^# TYPE slidepdf_cache_total counter$/m);
  assert.match(text, /^# TYPE slidepdf_cache_bytes gauge$/m);
  assert.ok(sample(text, "slidepdf_cache_bytes") > 0);
});

test("an unknown preset is refused before anything is stored", e2e, async () => {
  const res = await upload("x", "../../etc");
  assert.equal(res.status, 400);
});

test("render accumulates histogram buckets and counts outcomes", () => {
  const m = createMetrics(64);
  const end = m.track();
  m.record("ok", 0.05, 10 * 1048576);
  m.record("ok", 3, 100 * 1048576);
  m.record("timeout", 70);
  m.record(outcomeOfCode("convert"), 0.2);
  m.converted(4, 1000);
  const text = m.render({ workers: 2, busy: 1, queued: 3 });
  assert.equal(sample(text, 'slidepdf_conversions_total\\{outcome="ok"\\}'), 2);
  assert.equal(sample(text, 'slidepdf_conversions_total\\{outcome="timeout"\\}'), 1);
  assert.equal(sample(text, 'slidepdf_conversions_total\\{outcome="invalid"\\}'), 1);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="0.1"\\}'), 1);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="0.25"\\}'), 2);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="5"\\}'), 3);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="60"\\}'), 3);
  assert.equal(sample(text, 'slidepdf_conversion_duration_seconds_bucket\\{le="\\+Inf"\\}'), 4);
  assert.equal(sample(text, 'slidepdf_conversion_peak_memory_megabytes_bucket\\{le="16"\\}'), 1);
  assert.equal(sample(text, 'slidepdf_conversion_peak_memory_megabytes_bucket\\{le="128"\\}'), 2);
  assert.equal(sample(text, "slidepdf_workers_recycled_total"), 1);
  assert.equal(sample(text, "slidepdf_slides_converted_total"), 4);
  assert.equal(sample(text, "slidepdf_input_bytes_total"), 1000);
  assert.equal(sample(text, "slidepdf_workers_busy"), 1);
  assert.equal(sample(text, "slidepdf_workers_idle"), 1);
  assert.equal(sample(text, "slidepdf_queue_depth"), 3);
  assert.equal(sample(text, "slidepdf_conversions_in_flight"), 1);
  end();
  end();
  assert.equal(sample(m.render({ workers: 0, busy: 0, queued: 0 }), "slidepdf_conversions_in_flight"), 0);
});

test("error codes map to outcome labels", () => {
  assert.deepEqual(["memory", "timeout", "aborted", "convert", "worker", undefined].map(outcomeOfCode), [
    "memory", "timeout", "aborted", "invalid", "error", "error",
  ]);
});

test("jobs: identical keys share one start, and every waiter is counted", async () => {
  const jobs = createJobs<string, { value: string; holds: number }>();
  let starts = 0;
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const start = async () => {
    starts++;
    await gate;
    return "done";
  };
  const commit = (value: string, holds: number) => ({ value, holds });
  const a = jobs.attach("k", start, commit);
  const b = jobs.attach("k", start, commit);
  assert.deepEqual([a.joined, b.joined], [false, true]);
  release();
  assert.deepEqual(await Promise.all([a.wait(), b.wait()]), [{ value: "done", holds: 2 }, { value: "done", holds: 2 }]);
  assert.equal(starts, 1);
  // Settled: the key is free, so the next request starts a new job.
  assert.equal(jobs.attach("k", async () => "again", commit).joined, false);
});

test("jobs: one waiter leaving keeps the job running for the rest", async () => {
  const jobs = createJobs<string, { holds: number }>();
  let signal!: AbortSignal;
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const a = jobs.attach("k", async (s) => ((signal = s), await gate, "x"), (_v, holds) => ({ holds }));
  const b = jobs.attach("k", async () => "unused", (_v, holds) => ({ holds }));
  a.leave();
  assert.equal(signal.aborted, false);
  release();
  assert.equal(await a.wait(), undefined, "the waiter that left holds nothing");
  assert.deepEqual(await b.wait(), { holds: 1 });
});

test("jobs: the last waiter leaving cancels the job and frees the key", async () => {
  const jobs = createJobs<string, string>();
  let signal!: AbortSignal;
  const first = jobs.attach(
    "k",
    (s) => new Promise<string>((_, reject) => ((signal = s), s.addEventListener("abort", () => reject(new Error("cancelled"))))),
    (v) => v,
  );
  const second = jobs.attach("k", async () => "unused", (v) => v);
  first.leave();
  assert.equal(signal.aborted, false);
  second.leave();
  assert.equal(signal.aborted, true);
  await assert.rejects(first.wait(), /cancelled/);
  // A request arriving while the old worker winds down starts its own job.
  assert.equal(jobs.attach("k", async () => "fresh", (v) => v).joined, false);
});

test("jobs: a failure reaches every waiter and frees the key", async () => {
  const jobs = createJobs<string, string>();
  const a = jobs.attach("k", async () => { throw new Error("boom"); }, (v) => v);
  const b = jobs.attach("k", async () => "unused", (v) => v);
  await assert.rejects(a.wait(), /boom/);
  await assert.rejects(b.wait(), /boom/);
  assert.equal(jobs.attach("k", async () => "ok", (v) => v).joined, false);
});

test("cache: evicts the least recently used entry, never a pinned one", async () => {
  const root = await mkdtemp(join(tmpdir(), "slidepdf-cache-unit-"));
  try {
    const cache = await createResultCache(root, 10);
    const stage = async (name: string) => {
      const dir = join(cache.tmpDir, name);
      await mkdir(dir);
      await writeFile(join(dir, "f"), "1234");
      return dir;
    };
    cache.add("a", await stage("a"), 4, 0);
    cache.add("b", await stage("b"), 4, 0);
    assert.equal(cache.bytes(), 8);
    cache.acquire("a")!.release(); // a is now newer than b
    const pinnedC = cache.add("c", await stage("c"), 4, 1);
    assert.equal(cache.bytes(), 8, "b went, being the oldest");
    assert.equal(cache.acquire("b"), undefined);
    assert.ok(cache.acquire("a"));

    // A pinned entry outlives the limit until its holder is done.
    const small = await createResultCache(root, 0);
    assert.equal(small.bytes(), 0, "a smaller limit applies at start");
    const held = small.add("d", await (async () => { const d = join(small.tmpDir, "d"); await mkdir(d); await writeFile(join(d, "f"), "1234"); return d; })(), 4, 1);
    assert.equal(small.bytes(), 4);
    held.release();
    assert.equal(small.bytes(), 0);
    pinnedC.release();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("cache: entries survive a restart", async () => {
  const root = await mkdtemp(join(tmpdir(), "slidepdf-cache-unit-"));
  try {
    const first = await createResultCache(root, 100);
    const dir = join(first.tmpDir, "x");
    await mkdir(dir);
    await writeFile(join(dir, "f"), "12345");
    first.add("x", dir, 5, 0);
    const second = await createResultCache(root, 100);
    assert.equal(second.bytes(), 5);
    assert.ok(second.acquire("x"));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("render exposes the cache counters and size", () => {
  const m = createMetrics(64);
  m.cache("hit");
  m.cache("joined");
  m.cache("joined");
  const text = m.render({ workers: 0, busy: 0, queued: 0 }, 1234);
  assert.equal(cacheCount(text, "hit"), 1);
  assert.equal(cacheCount(text, "miss"), 0);
  assert.equal(cacheCount(text, "joined"), 2);
  assert.equal(sample(text, "slidepdf_cache_bytes"), 1234);
});
