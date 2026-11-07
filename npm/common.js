// Shared wrapper over the raw wasm-bindgen exports.

const fold = (s) => (s || "").normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

export function wrap(raw) {
  return {
    convert(pptx, options = {}) {
      const r = raw.convertRaw(toBytes(pptx), options);
      return {
        pdf: r.pdf,
        fragments: r.fragments,
        manifest: JSON.parse(r.manifestJson),
        manifestJson: r.manifestJson,
        report: { ...JSON.parse(r.reportJson), text: r.reportText },
      };
    },
    convertStream(source, options = {}, { onFragment, onPdf, zeroCopy = false } = {}) {
      const src = source instanceof Uint8Array || source instanceof ArrayBuffer || ArrayBuffer.isView(source)
        ? bytesSource(toBytes(source))
        : source;
      if (typeof onFragment !== "function") throw new TypeError("onFragment callback is required");
      const r = raw.convertStreamRaw(src.size, (off, len) => src.read(off, len), options, onFragment, onPdf ?? undefined, zeroCopy);
      return {
        manifest: JSON.parse(r.manifestJson),
        manifestJson: r.manifestJson,
        report: { ...JSON.parse(r.reportJson), text: r.reportText },
      };
    },
    assemble(parts) {
      if (!raw.assemble) throw new Error("slidepdf/lite has no assemble(); use the full package");
      return raw.assemble(parts.map(toBytes));
    },
    assembleStream(next, onPdf, { zeroCopy = false } = {}) {
      if (!raw.assembleStreamRaw) throw new Error("slidepdf/lite has no assembleStream(); use the full package");
      return raw.assembleStreamRaw(() => {
        const part = next();
        return part == null ? null : toBytes(part);
      }, onPdf, zeroCopy);
    },
    split(pdf) {
      if (!raw.split) throw new Error("slidepdf/lite has no split(); use the full package");
      return raw.split(toBytes(pdf));
    },
    fragmentHash(pdf) {
      return raw.fragmentHash(toBytes(pdf));
    },
    verifyFragment(pdf, hash) {
      return raw.fragmentHash(toBytes(pdf)) === hash;
    },
  };
}

/** Accent- and case-insensitive search over a manifest's text index. */
export function search(manifest, query) {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (!words.length) return [];
  return manifest.slides
    .filter((s) => {
      const hay = fold(`${s.title || ""} ${s.text || ""}`);
      return words.every((w) => hay.includes(w));
    })
    .map((s) => ({ number: s.number, title: s.title, file: s.file }));
}

function bytesSource(b) {
  return { size: b.length, read: (off, len) => b.subarray(off, off + len) };
}

function toBytes(x) {
  if (x instanceof Uint8Array) return x;
  if (x instanceof ArrayBuffer) return new Uint8Array(x);
  if (ArrayBuffer.isView(x)) return new Uint8Array(x.buffer, x.byteOffset, x.byteLength);
  throw new TypeError("expected Uint8Array, ArrayBuffer or Buffer");
}
