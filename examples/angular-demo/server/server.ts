// Demo backend: converts uploaded PPTX files with slidepdf and stores the
// fragments on disk, the way a CDN bucket would hold them.
//
//   POST /api/convert?preset=light|balanced|print   body: raw PPTX bytes
//        -> { id, manifest, report, sizes }
//   GET  /api/decks/:id/assemble?range=2-4          -> application/pdf
//   GET  /store/:id/<file>                          -> stored fragment / manifest
//
// Run with: node server/server.ts   (Node 23+ strips the types natively)

import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { mkdir, readFile, rm, stat } from "node:fs/promises";
import { createReadStream, createWriteStream } from "node:fs";
import { tmpdir } from "node:os";
import { pipeline } from "node:stream/promises";
import { Transform } from "node:stream";
import { join, normalize } from "node:path";
import { randomUUID } from "node:crypto";
import { createPool, verifyFragment, SlidepdfError, type Preset, type Manifest } from "slidepdf";

const PORT = Number(process.env.PORT ?? 3001);
const STORE = join(import.meta.dirname, "..", "store");
const MAX_UPLOAD_MB = Number(process.env.MAX_UPLOAD_MB ?? 500);
const MAX_UPLOAD = MAX_UPLOAD_MB * 1024 * 1024;
// Decks with more slides than this are refused up front, before taking a worker.
const MAX_SLIDES = Number(process.env.MAX_SLIDES ?? 500);

// Conversions run in worker threads with a hard budget each, so a bad deck
// fails on its own and the process memory stays at roughly
// size x (memoryMb + 30 MB) whatever comes in. See "Running in pods" in the README.
const pool = createPool({
  size: Number(process.env.WORKERS ?? 2),
  memoryMb: Number(process.env.MEMORY_MB ?? 256),
  timeoutMs: Number(process.env.TIMEOUT_MS ?? 120_000),
  recycleAboveMb: 64,
});

/** Streams the upload to disk so a large deck never sits in memory. */
async function saveUpload(req: IncomingMessage, dest: string): Promise<number> {
  let size = 0;
  const limit = new Transform({
    transform(chunk: Buffer, _enc, done) {
      size += chunk.length;
      done(size > MAX_UPLOAD ? new HttpError(413, `file is larger than ${MAX_UPLOAD_MB} MB`) : null, chunk);
    },
  });
  await pipeline(req, limit, createWriteStream(dest));
  return size;
}

class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** A client that goes away cancels its conversion after the current slide. */
function abortOnClose(req: IncomingMessage): AbortSignal {
  const ac = new AbortController();
  req.once("close", () => ac.abort());
  return ac.signal;
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

  if (req.method === "POST" && url.pathname === "/api/convert") {
    const preset = (url.searchParams.get("preset") ?? "balanced") as Preset;
    const name = url.searchParams.get("name") ?? "deck.pptx";
    const id = randomUUID();
    const dir = deckDir(id);
    await mkdir(dir, { recursive: true });
    const upload = join(dir, "source.pptx");
    const pptxBytes = await saveUpload(req, upload);
    let out;
    try {
      // Preflight in milliseconds: refuse oversized decks before converting.
      const info = await pool.inspectFile(upload);
      if (info.slides > MAX_SLIDES) throw new HttpError(413, `deck has ${info.slides} slides, more than the ${MAX_SLIDES} allowed`);
      // Bounded memory: reads the deck from disk and writes each output as it is produced.
      out = await pool.convertFile(upload, dir, { preset, sourceName: name, signal: AbortSignal.any([abortOnClose(req)]) });
    } catch (e) {
      await rm(dir, { recursive: true, force: true });
      if (e instanceof HttpError) throw e;
      const err = e as SlidepdfError;
      // Each failure code maps to a status the client can act on.
      const status = { memory: 413, timeout: 408, aborted: 499, convert: 422 }[err.code] ?? 500;
      throw new HttpError(status, `could not convert (${err.code}): ${err.message}`);
    } finally {
      await rm(upload, { force: true });
    }
    const ms = Math.round(out.stats.ms);
    const fragmentBytes = out.manifest.slides.reduce((s, f) => s + f.bytes, 0);
    const pdfBytes = (await stat(join(dir, "deck.pdf"))).size;
    return json(res, 200, {
      id,
      manifest: out.manifest,
      report: out.report,
      sizes: { pptx: pptxBytes, pdf: pdfBytes, fragments: fragmentBytes, ms },
    });
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
