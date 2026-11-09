// Demo backend: converts uploaded PPTX files with slidepdf and stores the
// fragments on disk, the way a CDN bucket would hold them.
//
//   POST /api/convert?preset=light|balanced|print   body: raw PPTX bytes
//        -> { id, manifest, report, sizes }, with X-Slidepdf-Cache: hit | miss | joined
//   GET  /api/decks/:id/assemble?range=2-4          -> application/pdf
//   GET  /store/:id/<file>                          -> stored fragment / manifest
//   GET  /metrics                                   -> Prometheus text format
//
// Run with: node server/server.ts   (Node 23+ strips the types natively)

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { copyFile, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { constants, createReadStream, createWriteStream } from "node:fs";
import { tmpdir } from "node:os";
import { pipeline } from "node:stream/promises";
import { Transform } from "node:stream";
import { join, normalize } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { createJobs, createResultCache, dirBytes, type Lease } from "./dedupe.ts";
import { createMetrics, outcomeOfCode, type Outcome } from "./metrics.ts";
import { createPool, verifyFragment, SlidepdfError, type Preset, type Manifest } from "slidepdf";

const PORT = Number(process.env.PORT ?? 3001);
const STORE = join(import.meta.dirname, "..", "store");
const MAX_UPLOAD_MB = Number(process.env.MAX_UPLOAD_MB ?? 500);
const MAX_UPLOAD = MAX_UPLOAD_MB * 1024 * 1024;
// Decks with more slides than this are refused up front, before taking a worker.
const MAX_SLIDES = Number(process.env.MAX_SLIDES ?? 500);
const RECYCLE_MB = 64;
const CACHE_HEADER = "x-slidepdf-cache";
const PRESETS: Preset[] = ["light", "balanced", "print"];

// Conversions run in worker threads with a hard budget each, so a bad deck
// fails on its own and the process memory stays at roughly
// size x (memoryMb + 30 MB) whatever comes in. See "Running in pods" in the README.
const pool = createPool({
  size: Number(process.env.WORKERS ?? 2),
  memoryMb: Number(process.env.MEMORY_MB ?? 256),
  timeoutMs: Number(process.env.TIMEOUT_MS ?? 120_000),
  recycleAboveMb: RECYCLE_MB,
});

const metrics = createMetrics(RECYCLE_MB);

// Finished conversions, keyed by the SHA-256 of the deck and the preset. One server per CACHE_DIR.
const cache = await createResultCache(
  process.env.CACHE_DIR ?? join(tmpdir(), "slidepdf-demo-cache"),
  Number(process.env.CACHE_MB ?? 256) * 1024 * 1024,
);

/** What a finished job leaves behind: a cache entry pinned once per attached request. */
interface Staged {
  staging: string;
  bytes: number;
  ms: number;
}
interface Done {
  lease: Lease;
  ms: number;
}
const jobs = createJobs<Staged, Done>();

/** Maps a conversion failure to its outcome label. */
function outcomeOf(e: unknown): Outcome {
  if (e instanceof HttpError) return "invalid";
  return e instanceof SlidepdfError ? outcomeOfCode(e.code) : "error";
}

/** Streams the upload to disk, hashing it on the way, so a large deck never sits in memory. */
async function saveUpload(req: IncomingMessage, dest: string): Promise<{ size: number; sha256: string }> {
  let size = 0;
  const hash = createHash("sha256");
  const limit = new Transform({
    transform(chunk: Buffer, _enc, done) {
      size += chunk.length;
      hash.update(chunk);
      done(size > MAX_UPLOAD ? new HttpError(413, `file is larger than ${MAX_UPLOAD_MB} MB`) : null, chunk);
    },
  });
  await pipeline(req, limit, createWriteStream(dest));
  return { size, sha256: hash.digest("hex") };
}

class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** The preset is part of a file name in the cache, so only the known ones get through. */
function parsePreset(value: string | null): Preset {
  const preset = (value ?? "balanced") as Preset;
  if (!PRESETS.includes(preset)) throw new HttpError(400, `preset must be one of ${PRESETS.join(", ")}`);
  return preset;
}

/** Each failure code maps to a status the client can act on. */
function toHttpError(e: unknown): HttpError {
  if (e instanceof HttpError) return e;
  const err = e as SlidepdfError;
  const status = { memory: 413, timeout: 408, aborted: 499, convert: 422 }[err.code] ?? 500;
  return new HttpError(status, `could not convert (${err.code}): ${err.message}`);
}

/**
 * Converts the uploaded deck into a staging folder. The job owns `upload` and
 * deletes it, whichever request started it, so it survives that request leaving.
 */
async function convertJob(upload: string, preset: Preset, name: string, started: number, signal: AbortSignal): Promise<Staged> {
  const staging = join(cache.tmpDir, `job-${randomUUID()}`);
  try {
    await mkdir(staging);
    // Preflight in milliseconds: refuse oversized decks before converting.
    const info = await pool.inspectFile(upload, { signal });
    if (info.slides > MAX_SLIDES) throw new HttpError(413, `deck has ${info.slides} slides, more than the ${MAX_SLIDES} allowed`);
    // Bounded memory: reads the deck from disk and writes each output as it is produced.
    const out = await pool.convertFile(upload, staging, { preset, sourceName: name, signal });
    const bytes = await dirBytes(staging);
    metrics.record("ok", (performance.now() - started) / 1000, out.stats.peakBytes);
    metrics.converted(out.stats.slides, out.stats.inputBytes);
    return { staging, bytes, ms: Math.round(out.stats.ms) };
  } catch (e) {
    await rm(staging, { recursive: true, force: true });
    metrics.record(outcomeOf(e), (performance.now() - started) / 1000, e instanceof SlidepdfError ? e.peakBytes : undefined);
    throw e;
  } finally {
    await rm(upload, { force: true });
  }
}

/**
 * Copies a cached result into a new deck folder of the store and answers with
 * it. Takes over `lease` and releases it. `ms` is the conversion time, 0 when nothing was converted.
 */
async function publish(res: ServerResponse, lease: Lease, name: string, pptxBytes: number, ms: number) {
  if (res.destroyed) return lease.release();
  const id = randomUUID();
  const dir = deckDir(id);
  let manifest: Manifest, report: unknown, pdfBytes: number;
  try {
    await mkdir(dir, { recursive: true });
    // Copies, so evicting the cache entry never breaks a deck a client already holds.
    for (const file of await readdir(lease.dir)) await copyFile(join(lease.dir, file), join(dir, file), constants.COPYFILE_FICLONE);
    manifest = JSON.parse(await readFile(join(dir, "manifest.json"), "utf8"));
    report = JSON.parse(await readFile(join(dir, "report.json"), "utf8"));
    // The same deck can arrive under another name; the manifest follows this upload.
    if (manifest.source !== name) {
      manifest.source = name;
      await writeFile(join(dir, "manifest.json"), JSON.stringify(manifest));
    }
    pdfBytes = (await stat(join(dir, "deck.pdf"))).size;
  } catch (e) {
    await rm(dir, { recursive: true, force: true });
    throw e;
  } finally {
    lease.release();
  }
  const fragmentBytes = manifest.slides.reduce((s, f) => s + f.bytes, 0);
  json(res, 200, { id, manifest, report, sizes: { pptx: pptxBytes, pdf: pdfBytes, fragments: fragmentBytes, ms } });
}

function json(res: ServerResponse, status: number, data: unknown) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(data));
}

/** "2-4,7" -> [2,3,4,7], validated against the slide count. */
function parseRange(range: string, max: number): number[] {
  const out: number[] = [];
  for (const part of range.split(",").map((p) => p.trim()).filter(Boolean)) {
    const [a, b = a] = part.split("-").map((x) => (x.trim() === "" ? String(max) : x.trim()));
    const from = Number(a), to = Number(b);
    if (!Number.isInteger(from) || !Number.isInteger(to) || from < 1 || to > max || from > to) {
      throw new HttpError(400, `range "${part}" is outside 1-${max}`);
    }
    for (let n = from; n <= to; n++) out.push(n);
  }
  return out;
}

const deckDir = (id: string) => {
  if (!/^[0-9a-f-]{36}$/.test(id)) throw new HttpError(404, "unknown deck");
  return join(STORE, id);
};

async function handle(req: IncomingMessage, res: ServerResponse) {
  const url = new URL(req.url ?? "/", "http://localhost");

  if (req.method === "GET" && url.pathname === "/metrics") {
    res.writeHead(200, { "content-type": "text/plain; version=0.0.4; charset=utf-8" });
    return res.end(metrics.render(pool.stats(), cache.bytes()));
  }

  if (req.method === "POST" && url.pathname === "/api/convert") {
    const preset = parsePreset(url.searchParams.get("preset"));
    const name = url.searchParams.get("name") ?? "deck.pptx";
    const upload = join(cache.tmpDir, `upload-${randomUUID()}.pptx`);
    const started = performance.now();
    const endUpload = metrics.track();
    let received;
    try {
      received = await saveUpload(req, upload);
    } catch (e) {
      await rm(upload, { force: true });
      metrics.record(req.destroyed ? "aborted" : outcomeOf(e), (performance.now() - started) / 1000);
      throw e;
    } finally {
      endUpload();
    }

    const key = cache.key(received.sha256, preset);
    const hit = cache.acquire(key);
    if (hit) {
      metrics.cache("hit");
      res.setHeader(CACHE_HEADER, "hit");
      await rm(upload, { force: true });
      return publish(res, hit, name, received.size, 0);
    }

    // Identical decks converting at the same time share one job; it is cancelled when the last request leaves.
    const waiter = jobs.attach(
      key,
      (signal) => convertJob(upload, preset, name, started, signal),
      (staged, holds): Done => ({ lease: cache.add(key, staged.staging, staged.bytes, holds), ms: staged.ms }),
    );
    const result = waiter.joined ? "joined" : "miss";
    metrics.cache(result);
    res.setHeader(CACHE_HEADER, result);
    res.once("close", waiter.leave);
    // A joined request's own copy is not needed: the running job reads the first upload.
    if (waiter.joined) await rm(upload, { force: true });

    const endConvert = metrics.track();
    let done;
    try {
      done = await waiter.wait();
    } catch (e) {
      throw toHttpError(e);
    } finally {
      endConvert();
    }
    // undefined: this request left before the job finished, so it holds no lease.
    if (!done) return;
    return publish(res, done.lease, name, received.size, done.ms);
  }

  const asm = url.pathname.match(/^\/api\/decks\/([^/]+)\/assemble$/);
  if (req.method === "GET" && asm) {
    const dir = deckDir(asm[1]);
    const manifest: Manifest = JSON.parse(await readFile(join(dir, "manifest.json"), "utf8"));
    const pick = parseRange(url.searchParams.get("range") || `1-${manifest.count}`, manifest.count);
    // Check fragments one at a time (never all in memory), then merge
    // file to file with bounded memory and stream the result.
    const files: string[] = [];
    for (const n of pick) {
      const s = manifest.slides[n - 1];
      const path = join(dir, s.file);
      // Refuse to rebuild from a fragment that changed since conversion.
      if (!verifyFragment(await readFile(path), s.hash)) throw new HttpError(409, `${s.file} does not match its manifest hash`);
      files.push(path);
    }
    const out = join(tmpdir(), `slidepdf-${randomUUID()}.pdf`);
    try {
      await pool.assembleFiles(files, out);
      const { size } = await stat(out);
      res.writeHead(200, {
        "content-type": "application/pdf",
        "content-length": size,
        "content-disposition": `attachment; filename="slides-${url.searchParams.get("range") || "all"}.pdf"`,
      });
      await pipeline(createReadStream(out), res);
    } finally {
      await rm(out, { force: true });
    }
    return;
  }

  const file = url.pathname.match(/^\/store\/([^/]+)\/([\w.-]+)$/);
  if (req.method === "GET" && file) {
    const path = normalize(join(deckDir(file[1]), file[2]));
    try {
      const data = await readFile(path);
      const type = path.endsWith(".json") ? "application/json" : "application/pdf";
      res.writeHead(200, { "content-type": type, "cache-control": "public, max-age=31536000, immutable" });
      return res.end(data);
    } catch {
      throw new HttpError(404, "not found");
    }
  }

  throw new HttpError(404, "not found");
}

createServer((req, res) => {
  handle(req, res).catch((e) => {
    const status = e instanceof HttpError ? e.status : 500;
    if (status === 500) console.error(e);
    json(res, status, { error: e.message });
  });
}).listen(PORT, () => console.log(`slidepdf demo API on http://localhost:${PORT}`));
