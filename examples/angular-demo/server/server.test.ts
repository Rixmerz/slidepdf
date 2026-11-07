import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { readFile } from "node:fs/promises";
import { createServer } from "node:net";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createMetrics, outcomeOfCode } from "./metrics.ts";

// npm/dist is built from the Rust crate and git-ignored, so a fresh checkout
// cannot load slidepdf; the end-to-end tests then skip and the unit tests still run.
const built = existsSync(join(dirname(fileURLToPath(import.meta.resolve("slidepdf"))), "dist", "full-node"));
const e2e = { skip: built ? false : "slidepdf WebAssembly build (npm/dist) is missing" };

let child: ChildProcess;
let base: string;

async function freePort(): Promise<number> {
  const srv = createServer();
  await new Promise<void>((r) => srv.listen(0, r));
  const { port } = srv.address() as { port: number };
  await new Promise((r) => srv.close(r));
  return port;
}

const metrics = async () => (await fetch(`${base}/metrics`)).text();
const sample = (text: string, line: string) => Number(text.match(new RegExp(`^${line} (\\S+)$`, "m"))?.[1]);

before(async () => {
  if (!built) return;
  const port = await freePort();
  base = `http://localhost:${port}`;
  child = spawn(process.execPath, [join(import.meta.dirname, "server.ts")], {
    env: { ...process.env, PORT: String(port), WORKERS: "1" },
    stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise<void>((resolve, reject) => {
    child.once("exit", () => reject(new Error("server exited before listening")));
    child.stdout!.on("data", (d) => String(d).includes("on http") && resolve());
  });
});

after(() => child?.kill());

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
  ]) assert.ok(text.includes(name), name);
  for (const o of ["ok", "memory", "timeout", "aborted", "invalid", "error"]) {
    assert.equal(sample(text, `slidepdf_conversions_total\\{outcome="${o}"\\}`), 0, o);
  }
  assert.ok(text.endsWith("\n"));
});

test("GET /metrics counts a good deck and a bad one", e2e, async () => {
  const deck = await readFile(join(import.meta.dirname, "..", "..", "..", "tests", "fixtures", "deck.pptx"));
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
