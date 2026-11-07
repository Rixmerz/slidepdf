// Node.js API over a loaded wasm-bindgen module (shared by full and lite).
import { createRequire } from "node:module";
import { wrap, search } from "./common.js";

const require = createRequire(import.meta.url);

export function makeNodeApi(raw) {
  const api = wrap(raw);

  /**
   * Converts a PPTX file into `outDir` with bounded memory: the package is
   * read from disk in pieces and every output is written as soon as it is
   * ready (slide-NNN.pdf, deck.pdf, manifest.json, report.json).
   */
  function convertFile(input, outDir, options = {}) {
    const fs = require("node:fs");
    const path = require("node:path");
    fs.mkdirSync(outDir, { recursive: true });
    const fd = fs.openSync(input, "r");
    const size = fs.fstatSync(fd).size;
    const pdfFd = options.fullPdf === false ? null : fs.openSync(path.join(outDir, "deck.pdf"), "w");
    try {
      // One reusable buffer: the WebAssembly side copies each chunk out
      // immediately, so allocating per read would only feed the GC.
      let scratch = Buffer.allocUnsafe(1 << 16);
      const source = {
        size,
        read(offset, length) {
          if (length > scratch.length) scratch = Buffer.allocUnsafe(length);
          const n = fs.readSync(fd, scratch, 0, length, offset);
          return scratch.subarray(0, n);
        },
      };
      const out = api.convertStream(source, { sourceName: path.basename(input), ...options }, {
        onFragment: (f) => fs.writeFileSync(path.join(outDir, f.file), f.pdf),
        onPdf: pdfFd === null ? undefined : (chunk) => fs.writeSync(pdfFd, chunk),
        // Both callbacks write synchronously, so views into wasm memory are safe.
        zeroCopy: true,
      });
      fs.writeFileSync(path.join(outDir, "manifest.json"), out.manifestJson);
      fs.writeFileSync(path.join(outDir, "report.json"), JSON.stringify(out.report));
      return out;
    } finally {
      fs.closeSync(fd);
      if (pdfFd !== null) fs.closeSync(pdfFd);
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

  return { ...api, convertFile, assembleFiles, search, ready: async () => {} };
}

export const loadRaw = (path) => require(path);
