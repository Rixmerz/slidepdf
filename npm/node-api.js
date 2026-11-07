// Node.js API over a loaded wasm-bindgen module (shared by full and lite).
import { createRequire } from "node:module";
import { wrap, search, SlidepdfError } from "./common.js";

const require = createRequire(import.meta.url);

/**
 * Builds the Node API for the glue at `glue` (a path relative to this
 * file). The module is loaded lazily and reloaded after a trap (a memory
 * limit hit), so one failed conversion does not poison the process.
 */
export function makeNodeApi(glue) {
  let get, reload;
  if (typeof glue === "string") {
    const resolved = require.resolve(glue);
    let raw = null;
    get = () => raw ?? (raw = require(resolved));
    reload = () => {
      delete require.cache[resolved];
      raw = null;
    };
  } else {
    // An already initialised module (a worker given a shared compiled
    // module); `reload` is whatever the owner can do after a trap.
    ({ get, reload } = glue);
  }
  const api = wrap(get, reload);

  function openSource(input) {
    const fs = require("node:fs");
    const fd = fs.openSync(input, "r");
    const size = fs.fstatSync(fd).size;
    // One reusable buffer: the WebAssembly side copies each chunk out
    // immediately, so allocating per read would only feed the GC.
    let scratch = Buffer.allocUnsafe(1 << 16);
    return {
      fd,
      size,
      read(offset, length) {
        if (length > scratch.length) scratch = Buffer.allocUnsafe(length);
        const n = fs.readSync(fd, scratch, 0, length, offset);
        return scratch.subarray(0, n);
      },
      close: () => fs.closeSync(fd),
    };
  }

  /**
   * Converts a PPTX file into `outDir` with bounded memory: the package is
   * read from disk in pieces and every output is written as soon as it is
   * ready (slide-NNN.pdf, deck.pdf, manifest.json, report.json).
   */
  function convertFile(input, outDir, options = {}) {
    const fs = require("node:fs");
    const path = require("node:path");
    fs.mkdirSync(outDir, { recursive: true });
    const { onProgress, shouldCancel, ...opts } = options;
    const source = openSource(input);
    const pdfFd = opts.fullPdf === false ? null : fs.openSync(path.join(outDir, "deck.pdf"), "w");
    try {
      const out = api.convertStream(source, { sourceName: path.basename(input), ...opts }, {
        onFragment: (f) => fs.writeFileSync(path.join(outDir, f.file), f.pdf),
        onPdf: pdfFd === null ? undefined : (chunk) => fs.writeSync(pdfFd, chunk),
        onProgress,
        shouldCancel,
        // Both callbacks write synchronously, so views into wasm memory are safe.
        zeroCopy: true,
      });
      fs.writeFileSync(path.join(outDir, "manifest.json"), out.manifestJson);
      fs.writeFileSync(path.join(outDir, "report.json"), JSON.stringify(out.report));
      return out;
    } finally {
      source.close();
      if (pdfFd !== null) fs.closeSync(pdfFd);
    }
  }

  /** Sizes up a .pptx file in milliseconds without converting it. */
  function inspectFile(input) {
    const source = openSource(input);
    try {
      return api.inspect(source);
    } finally {
      source.close();
    }
  }

  /**
   * Merges PDF files into `outPath` with bounded memory: inputs are read one
   * at a time and the output is written as it is produced. Returns the page count.
   */
  function assembleFiles(inputs, outPath) {
    const fs = require("node:fs");
    const fd = fs.openSync(outPath, "w");
    try {
      let i = 0;
      return api.assembleStream(
        () => (i < inputs.length ? fs.readFileSync(inputs[i++]) : null),
        (chunk) => fs.writeSync(fd, chunk),
        { zeroCopy: true },
      );
    } finally {
      fs.closeSync(fd);
    }
  }

  return { ...api, convertFile, inspectFile, assembleFiles, search, SlidepdfError, ready: async () => {} };
}
