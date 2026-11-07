// Shared wrapper over the raw wasm-bindgen exports.

const fold = (s) => (s || "").normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

/** Error with a stable `code`; see SlidepdfErrorCode in the typings. */
export class SlidepdfError extends Error {
  constructor(code, message, extra = {}) {
    super(message);
    this.name = "SlidepdfError";
    this.code = code;
    Object.assign(this, extra);
  }
}

const CANCELLED = "conversion cancelled";

/**
 * Wraps the raw exports. `get` returns the current raw module (a function,
 * so Node can swap in a fresh instance after a trap); `onTrap` is called
 * when the instance is no longer usable.
 */
export function wrap(rawOrGet, onTrap) {
  const get = typeof rawOrGet === "function" ? rawOrGet : () => rawOrGet;

  // Turns a WebAssembly trap into a typed error. After a trap the instance
  // is dead (its allocator may be mid-operation), so the owner is told.
  function guard(fn) {
    try {
      return fn();
    } catch (e) {
      if (e instanceof WebAssembly.RuntimeError) {
        // Reading two statics from the dead instance is safe; nothing else is.
        let memory = false;
        let st = { peak: 0, limit: 0 };
        try { memory = get().memoryLimitHit(); st = get().memoryStats(); } catch { /* the instance may be unusable */ }
        onTrap?.();
        throw memory
          ? new SlidepdfError("memory", `memory limit exceeded (${(st.limit / 1048576) | 0} MB)`, { peakBytes: st.peak, limitBytes: st.limit })
          : new SlidepdfError("trap", `WebAssembly trap: ${e.message}`, { peakBytes: st.peak });
      }
      if (e instanceof SlidepdfError) throw e;
      const msg = e?.message ?? String(e);
      throw new SlidepdfError(msg === CANCELLED ? "cancelled" : "convert", msg, { cause: e });
    }
  }

  const stats = (t0, extra) => ({
    ms: Math.round((performance.now() - t0) * 10) / 10,
    peakBytes: get().memoryStats().peak,
    ...extra,
  });

  return {
    convert(pptx, options = {}) {
      const t0 = performance.now();
      const r = guard(() => get().convertRaw(toBytes(pptx), options));
      const manifest = JSON.parse(r.manifestJson);
      return {
        pdf: r.pdf,
        fragments: r.fragments,
        manifest,
        manifestJson: r.manifestJson,
        report: { ...JSON.parse(r.reportJson), text: r.reportText },
        stats: stats(t0, { slides: manifest.count, inputBytes: byteLength(pptx) }),
      };
    },
    convertStream(source, options = {}, { onFragment, onPdf, onProgress, zeroCopy = false, shouldCancel } = {}) {
      const src = source instanceof Uint8Array || source instanceof ArrayBuffer || ArrayBuffer.isView(source)
        ? bytesSource(toBytes(source))
        : source;
      if (typeof onFragment !== "function") throw new TypeError("onFragment callback is required");
      const t0 = performance.now();
      const { timeoutMs, signal, ...opts } = options;
      const deadline = timeoutMs > 0 ? t0 + timeoutMs : Infinity;
      let timedOut = false;
      // Checked after every slide: the only point where a synchronous
      // conversion can stop gracefully.
      const progress = (done, total) => {
        if (performance.now() > deadline) { timedOut = true; return false; }
        if (signal?.aborted || shouldCancel?.()) return false;
        if (onProgress) onProgress(done, total);
        return true;
      };
      let r;
      try {
        r = guard(() => get().convertStreamRaw(src.size, (off, len) => src.read(off, len), opts, onFragment, onPdf ?? undefined, zeroCopy, progress));
      } catch (e) {
        if (e.code === "cancelled") {
          if (timedOut) throw new SlidepdfError("timeout", `conversion exceeded ${timeoutMs} ms`);
          if (signal?.aborted) throw new SlidepdfError("aborted", "conversion aborted");
        }
        throw e;
      }
      const manifest = JSON.parse(r.manifestJson);
      return {
        manifest,
        manifestJson: r.manifestJson,
        report: { ...JSON.parse(r.reportJson), text: r.reportText },
        stats: stats(t0, { slides: manifest.count, inputBytes: src.size }),
      };
    },
    inspect(pptx) {
      const src = pptx instanceof Uint8Array || pptx instanceof ArrayBuffer || ArrayBuffer.isView(pptx)
        ? bytesSource(toBytes(pptx))
        : pptx;
      return guard(() => get().inspectRaw(src.size, (off, len) => src.read(off, len)));
    },
    assemble(parts) {
      if (!get().assemble) throw new Error("slidepdf/lite has no assemble(); use the full package");
      return guard(() => get().assemble(parts.map(toBytes)));
    },
    assembleStream(next, onPdf, { zeroCopy = false } = {}) {
      if (!get().assembleStreamRaw) throw new Error("slidepdf/lite has no assembleStream(); use the full package");
      return guard(() => get().assembleStreamRaw(() => {
        const part = next();
        return part == null ? null : toBytes(part);
      }, onPdf, zeroCopy));
    },
    split(pdf) {
      if (!get().split) throw new Error("slidepdf/lite has no split(); use the full package");
      return guard(() => get().split(toBytes(pdf)));
    },
    fragmentHash(pdf) {
      return get().fragmentHash(toBytes(pdf));
    },
    verifyFragment(pdf, hash) {
      return get().fragmentHash(toBytes(pdf)) === hash;
    },
    setMemoryLimit(mb) {
      get().setMemoryLimit(mb ?? 0);
    },
    memoryStats() {
      return get().memoryStats();
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

function byteLength(x) {
  return x instanceof ArrayBuffer ? x.byteLength : x.byteLength ?? x.length ?? 0;
}

function toBytes(x) {
  if (x instanceof Uint8Array) return x;
  if (x instanceof ArrayBuffer) return new Uint8Array(x);
  if (ArrayBuffer.isView(x)) return new Uint8Array(x.buffer, x.byteOffset, x.byteLength);
  throw new TypeError("expected Uint8Array, ArrayBuffer or Buffer");
}
