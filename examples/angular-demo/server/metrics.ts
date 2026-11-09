// Prometheus text exposition for the demo API, with no client library.
// Kept free of any slidepdf import so it can be tested without the WebAssembly build.

import type { CacheResult } from "./dedupe.ts";

export type Outcome = "ok" | "memory" | "timeout" | "aborted" | "invalid" | "error";
export const OUTCOMES: Outcome[] = ["ok", "memory", "timeout", "aborted", "invalid", "error"];
const DURATION_BUCKETS_S = [0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60];
const PEAK_BUCKETS_MB = [16, 32, 64, 128, 256, 512, 1024];
const MB = 1048576;
const CACHE_RESULTS: CacheResult[] = ["hit", "miss", "joined"];

/** The parts of `pool.stats()` the metrics need. */
export interface PoolGauges {
  workers: number;
  busy: number;
  queued: number;
}

/** Maps a slidepdf error code to an outcome label; anything unknown is "error". */
export function outcomeOfCode(code: string | undefined): Outcome {
  if (code === "memory" || code === "timeout" || code === "aborted") return code;
  return code === "convert" ? "invalid" : "error";
}

class Histogram {
  bounds: number[];
  counts: number[];
  sum = 0;
  count = 0;
  constructor(bounds: number[]) {
    this.bounds = bounds;
    this.counts = bounds.map(() => 0);
  }
  observe(v: number) {
    this.sum += v;
    this.count++;
    // Stored per bucket; render() accumulates, as the format requires.
    const i = this.bounds.findIndex((b) => v <= b);
    if (i >= 0) this.counts[i]++;
  }
  render(name: string, help: string): string[] {
    const lines = [`# HELP ${name} ${help}`, `# TYPE ${name} histogram`];
    let acc = 0;
    this.bounds.forEach((b, i) => lines.push(`${name}_bucket{le="${b}"} ${(acc += this.counts[i])}`));
    lines.push(`${name}_bucket{le="+Inf"} ${this.count}`, `${name}_sum ${this.sum}`, `${name}_count ${this.count}`);
    return lines;
  }
}

export function createMetrics(recycleAboveMb: number) {
  const conversions = Object.fromEntries(OUTCOMES.map((o) => [o, 0])) as Record<Outcome, number>;
  const duration = new Histogram(DURATION_BUCKETS_S);
  const peakMb = new Histogram(PEAK_BUCKETS_MB);
  let slides = 0;
  let inputBytes = 0;
  // The pool does not report recycling, so this counts conversions whose
  // peak passed recycleAboveMb, the condition under which it retires the worker.
  let recycled = 0;
  let inFlight = 0;
  const cacheResults = Object.fromEntries(CACHE_RESULTS.map((r) => [r, 0])) as Record<CacheResult, number>;

  const gauge = (name: string, help: string, v: number, type = "gauge") => [
    `# HELP ${name} ${help}`,
    `# TYPE ${name} ${type}`,
    `${name} ${v}`,
  ];

  return {
    /** Call when a request starts handling a conversion; the returned function ends it. */
    track(): () => void {
      inFlight++;
      let done = false;
      return () => {
        if (!done) inFlight--;
        done = true;
      };
    },
    record(outcome: Outcome, seconds: number, peakBytes?: number) {
      conversions[outcome]++;
      duration.observe(seconds);
      if (peakBytes !== undefined) {
        peakMb.observe(peakBytes / MB);
        if (peakBytes / MB > recycleAboveMb) recycled++;
      }
    },
    /** A deck that converted: counts its slides and input size. */
    converted(slideCount: number, bytes: number) {
      slides += slideCount;
      inputBytes += bytes;
    },
    /** How an upload was served: from the cache, by a new conversion, or by joining a running one. */
    cache(result: CacheResult) {
      cacheResults[result]++;
    },
    render(pool: PoolGauges, cacheBytes = 0): string {
      return (
        [
          "# HELP slidepdf_conversions_total Conversions finished, by outcome.",
          "# TYPE slidepdf_conversions_total counter",
          ...OUTCOMES.map((o) => `slidepdf_conversions_total{outcome="${o}"} ${conversions[o]}`),
          ...duration.render("slidepdf_conversion_duration_seconds", "Wall time of a conversion, any outcome."),
          ...peakMb.render("slidepdf_conversion_peak_memory_megabytes", "Peak WebAssembly memory of a conversion."),
          ...gauge("slidepdf_slides_converted_total", "Slides in successfully converted decks.", slides, "counter"),
          ...gauge("slidepdf_input_bytes_total", "PPTX bytes of successfully converted decks.", inputBytes, "counter"),
          ...gauge("slidepdf_workers_busy", "Workers running a job.", pool.busy),
          ...gauge("slidepdf_workers_idle", "Spawned workers waiting for a job.", pool.workers - pool.busy),
          ...gauge("slidepdf_workers_recycled_total", "Workers retired for growing past recycleAboveMb (estimated).", recycled, "counter"),
          ...gauge("slidepdf_queue_depth", "Jobs waiting for a free worker.", pool.queued),
          "# HELP slidepdf_cache_total Uploads by how they were served: hit, miss or joined.",
          "# TYPE slidepdf_cache_total counter",
          ...CACHE_RESULTS.map((r) => `slidepdf_cache_total{result="${r}"} ${cacheResults[r]}`),
          ...gauge("slidepdf_cache_bytes", "Bytes held by the on-disk result cache.", cacheBytes),
          ...gauge("slidepdf_conversions_in_flight", "Convert requests being handled, queued ones included.", inFlight),
        ].join("\n") + "\n"
      );
    },
  };
}
