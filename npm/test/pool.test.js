import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createPool, convertFileInWorker, inspectFile, convertFile, convertStream, setMemoryLimit, memoryStats, SlidepdfError } from "../node.js";

// The repo fixture, or any deck given through the environment.
const deck = process.env.SLIDEPDF_TEST_PPTX ?? new URL("../../tests/fixtures/deck.pptx", import.meta.url).pathname;
const skip = !existsSync(deck) && "set SLIDEPDF_TEST_PPTX";
const out = () => mkdtempSync(join(tmpdir(), "slidepdf-pool-"));

test("inspect is cheap and consistent with the conversion", { skip }, () => {
  const i = inspectFile(deck);
  assert.ok(i.slides >= 1);
  assert.ok(i.width > 0 && i.height > 0);
  assert.ok(i.mediaBytes <= i.bytes);
  const dir = out();
  const r = convertFile(deck, dir, { preset: "light" });
  assert.equal(r.manifest.count, i.slides);
  assert.equal(Math.round(r.manifest.width), Math.round(i.width));
  assert.ok(r.stats.ms >= 0 && r.stats.peakBytes > 0 && r.stats.inputBytes > 0);
  rmSync(dir, { recursive: true });
});

test("memory limit fails with code memory and the module recovers", { skip }, () => {
  const dir = out();
  setMemoryLimit(1);
  try {
    assert.throws(() => convertFile(deck, dir, { preset: "light" }), (e) => {
      assert.ok(e instanceof SlidepdfError);
      assert.equal(e.code, "memory");
      assert.equal(e.limitBytes, 1 << 20);
      assert.ok(e.peakBytes > 0 && e.peakBytes <= (1 << 20));
      return true;
    });
  } finally {
    setMemoryLimit(0);
  }
  // A fresh instance was loaded behind the same API.
  assert.equal(memoryStats().limit, 0);
  const r = convertFile(deck, dir, { preset: "light" });
  assert.ok(r.manifest.count >= 1);
  rmSync(dir, { recursive: true });
});

test("timeout and cancellation are cooperative", { skip }, () => {
  const bytes = readFileSync(deck);
  let seen = 0;
  assert.throws(
    () => convertStream(bytes, { preset: "light", timeoutMs: 0.0001 }, { onFragment: () => {}, onProgress: () => seen++ }),
    (e) => e instanceof SlidepdfError && e.code === "timeout",
  );
  assert.ok(seen <= 1, "stopped after the first slide at most");
  let n = 0;
  assert.throws(
    () => convertStream(bytes, { preset: "light" }, { onFragment: () => {}, shouldCancel: () => ++n >= 2 }),
    (e) => e.code === "cancelled",
  );
});

test("pool: concurrency, stats, memory cap, timeout, abort, recycling", { skip }, async () => {
  const pool = createPool({ size: 2, memoryMb: 64, recycleAboveMb: 1 });
  try {
    const dirs = [1, 2, 3].map(out);
    const rs = await Promise.all(dirs.map((d) => pool.convertFile(deck, d, { preset: "light" })));
    for (const r of rs) {
      assert.ok(r.manifest.count >= 1);
      assert.ok(r.stats.peakBytes > 0);
    }
    assert.ok(existsSync(join(dirs[0], "manifest.json")));
    const st = pool.stats();
    assert.equal(st.size, 2);
    assert.equal(st.busy, 0);
    // recycleAboveMb: 1 retires every worker that did real work.
    assert.equal(st.workers, 0, "workers past the cap were recycled");

    await assert.rejects(pool.convertFile(deck, out(), { preset: "light", memoryMb: 1 }), (e) => {
      assert.equal(e.code, "memory");
      assert.ok(e.peakBytes > 0);
      return true;
    });
    // The pool survives a memory failure.
    const again = await pool.convertFile(deck, out(), { preset: "light" });
    assert.ok(again.manifest.count >= 1);

    await assert.rejects(pool.convertFile(deck, out(), { preset: "light", timeoutMs: 0.0001 }), (e) => e.code === "timeout");

    const ac = new AbortController();
    ac.abort();
    await assert.rejects(pool.convertFile(deck, out(), { signal: ac.signal }), (e) => e.code === "aborted");

    const i = await pool.inspectFile(deck);
    assert.equal(i.slides, again.manifest.count);
    const progress = [];
    await pool.convertFile(deck, out(), { preset: "light", onProgress: (d, t) => progress.push([d, t]) });
    assert.equal(progress.length, i.slides);
    assert.deepEqual(progress.at(-1), [i.slides, i.slides]);
  } finally {
    await pool.close();
  }
  await assert.rejects(pool.convertFile(deck, out()), (e) => e.code === "closed");
});

test("convertFileInWorker runs one job and releases the worker", { skip }, async () => {
  const dir = out();
  const r = await convertFileInWorker(deck, dir, { preset: "light", timeoutMs: 60000, memoryMb: 128 });
  assert.ok(r.manifest.count >= 1);
  assert.ok(existsSync(join(dir, "deck.pdf")));
  rmSync(dir, { recursive: true });
});
