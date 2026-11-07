//! WebAssembly bindings. The npm package wraps these in a typed API
//! (see `npm/index.d.ts`); call that instead of these raw exports.

use std::io::{Read, Seek, SeekFrom, Write};

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

/// Counting allocator: `setMemoryLimit` caps a conversion and `memoryStats`
/// reports its peak. WebAssembly memory never shrinks, so the peak is also
/// what the instance keeps.
#[global_allocator]
static ALLOC: slidepdf::budget::Budget = slidepdf::budget::Budget;

/// Caps heap use at `mb` megabytes (0 removes the cap). When a conversion
/// exceeds it, the instance traps (a `WebAssembly.RuntimeError` in JS) and
/// must not be used again; `memoryLimitHit()` then returns true.
#[wasm_bindgen(js_name = setMemoryLimit)]
pub fn set_memory_limit(mb: f64) {
    slidepdf::budget::set_limit((mb.max(0.0) as usize) << 20);
}

/// `{ inUse, peak, limit, linear }` in bytes. `peak` restarts at every
/// conversion; `linear` is the size of WebAssembly memory, which only grows.
#[wasm_bindgen(js_name = memoryStats)]
pub fn memory_stats() -> JsValue {
    let o = Object::new();
    set(&o, "inUse", slidepdf::budget::in_use() as f64);
    set(&o, "peak", slidepdf::budget::peak() as f64);
    set(&o, "limit", slidepdf::budget::limit() as f64);
    set(&o, "linear", (core::arch::wasm32::memory_size(0) * 65536) as f64);
    o.into()
}

/// Whether the last trap was the memory limit (as opposed to a bug).
#[wasm_bindgen(js_name = memoryLimitHit)]
pub fn memory_limit_hit() -> bool {
    slidepdf::budget::limit_hit()
}

/// Sizes up a package without converting it (see `Inspection` in the
/// typings). `read(offset, length)` supplies the bytes, as in `convertStreamRaw`.
#[wasm_bindgen(js_name = inspectRaw)]
pub fn inspect_raw(size: f64, read: Function) -> Result<JsValue, JsError> {
    let input = std::io::BufReader::with_capacity(
        64 << 10,
        JsReader {
            read,
            size: size as u64,
            pos: 0,
        },
    );
    let i = slidepdf::inspect(input).map_err(|e| JsError::new(&e.0))?;
    let o = Object::new();
    set(&o, "slides", i.slides as u32);
    set(&o, "width", i.width);
    set(&o, "height", i.height);
    set(&o, "parts", i.parts as u32);
    set(&o, "bytes", i.bytes as f64);
    set(&o, "media", i.media as u32);
    set(&o, "mediaBytes", i.media_bytes as f64);
    set(&o, "largestMediaBytes", i.largest_media_bytes as f64);
    set(&o, "largestMedia", i.largest_media.as_str());
    set(&o, "embeddedFonts", i.embedded_fonts as u32);
    Ok(o.into())
}

fn set(o: &Object, k: &str, v: impl Into<JsValue>) {
    let _ = Reflect::set(o, &JsValue::from_str(k), &v.into());
}

fn get(o: &JsValue, k: &str) -> Option<JsValue> {
    Reflect::get(o, &JsValue::from_str(k))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

fn bytes(v: &[u8]) -> Uint8Array {
    Uint8Array::from(v)
}

fn options(js: &JsValue) -> Result<slidepdf::Options, JsError> {
    let preset = match get(js, "preset").and_then(|v| v.as_string()) {
        Some(p) => p.parse::<slidepdf::Preset>().map_err(|e| JsError::new(&e.0))?,
        None => slidepdf::Preset::Balanced,
    };
    let mut o = slidepdf::Options::preset(preset);
    if let Some(v) = get(js, "imageDpi").and_then(|v| v.as_f64()) {
        o.image_dpi = if v > 0.0 { Some(v as f32) } else { None };
    }
    if let Some(v) = get(js, "jpegQuality").and_then(|v| v.as_f64()) {
        o.jpeg_quality = v.clamp(1.0, 100.0) as u8;
    }
    if let Some(v) = get(js, "includeHidden").and_then(|v| v.as_bool()) {
        o.include_hidden = v;
    }
    if let Some(v) = get(js, "fullPdf").and_then(|v| v.as_bool()) {
        o.full_pdf = v;
    }
    if let Some(v) = get(js, "fragments").and_then(|v| v.as_bool()) {
        o.fragments = v;
    }
    if let Some(v) = get(js, "cacheMb").and_then(|v| v.as_f64()) {
        o.cache_bytes = (v.max(0.0) as usize) << 20;
    }
    if let Some(v) = get(js, "sourceName").and_then(|v| v.as_string()) {
        o.source_name = v;
    }
    if let Some(fonts) = get(js, "fonts") {
        for f in Array::from(&fonts).iter() {
            o.add_font(Uint8Array::new(&f).to_vec()).map_err(|e| JsError::new(&e.0))?;
        }
    }
    Ok(o)
}

/// Returns `{ pdf, fragments, manifestJson, reportJson, reportText }`.
#[wasm_bindgen(js_name = convertRaw)]
pub fn convert_raw(pptx: &[u8], opts: JsValue) -> Result<JsValue, JsError> {
    slidepdf::budget::reset_peak();
    let o = options(&opts)?;
    let out = slidepdf::convert(pptx, &o).map_err(|e| JsError::new(&e.0))?;
    let res = Object::new();
    set(&res, "pdf", bytes(&out.pdf));
    let frags = Array::new();
    for f in &out.fragments {
        frags.push(&fragment_js(f, false));
    }
    set(&res, "fragments", frags);
    set(&res, "manifestJson", out.manifest.to_json());
    set(&res, "reportJson", out.report.to_json());
    set(&res, "reportText", out.report.to_text());
    Ok(res.into())
}

/// Reads the PPTX through a JS callback `read(offset, length) -> Uint8Array`,
/// so the package never has to be copied into WebAssembly memory (which
/// can grow but never shrink).
struct JsReader {
    read: Function,
    size: u64,
    pos: u64,
}

impl Read for JsReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.size - self.pos) as usize;
        let chunk = self
            .read
            .call2(&JsValue::NULL, &JsValue::from_f64(self.pos as f64), &JsValue::from_f64(want as f64))
            .map_err(|e| std::io::Error::other(format!("read callback failed: {e:?}")))?;
        let arr = Uint8Array::new(&chunk);
        let n = (arr.length() as usize).min(want);
        arr.subarray(0, n as u32).copy_to(&mut buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for JsReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.size as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if p < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

/// Hands written bytes to a JS callback, as a copy or, with `zero_copy`,
/// as a view into WebAssembly memory valid only during the call.
struct JsWriter(Function, bool);

/// SAFETY: the view must not outlive `b`, and no Rust allocation may
/// happen while JS holds it. Callers only use it for a synchronous JS call
/// that consumes the data before returning.
fn view_or_copy(b: &[u8], zero_copy: bool) -> Uint8Array {
    if zero_copy {
        unsafe { Uint8Array::view(b) }
    } else {
        bytes(b)
    }
}

impl Write for JsWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .call1(&JsValue::NULL, &view_or_copy(b, self.1))
            .map_err(|e| std::io::Error::other(format!("write callback failed: {e:?}")))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fragment_js(f: &slidepdf::Fragment, zero_copy: bool) -> Object {
    let x = Object::new();
    set(&x, "number", f.number as u32);
    set(&x, "file", f.file.as_str());
    set(&x, "hash", f.hash());
    set(&x, "title", f.title.as_deref().map(JsValue::from_str).unwrap_or(JsValue::NULL));
    set(&x, "text", f.text.as_str());
    set(&x, "warnings", f.warnings.iter().map(|w| JsValue::from_str(w)).collect::<Array>());
    // Last: a zero-copy view is invalidated by any Rust allocation that grows
    // WebAssembly memory, so nothing may allocate after it is created.
    set(&x, "pdf", view_or_copy(&f.pdf, zero_copy));
    x
}

/// Streaming conversion with bounded memory. `read(offset, length)`
/// supplies the PPTX, `onFragment(fragment)` receives each slide PDF as
/// soon as it is ready, `onPdf(chunk)` (optional) receives the full PDF
/// in pieces. Returns `{ manifestJson, reportJson, reportText }`.
///
/// With `zero_copy`, the `pdf` bytes handed to the callbacks are views into
/// WebAssembly memory, valid only until the callback returns (write them
/// out synchronously, or copy them). `onProgress(done, total)` (optional)
/// runs after every slide; returning `false` cancels the conversion.
#[wasm_bindgen(js_name = convertStreamRaw)]
pub fn convert_stream_raw(
    size: f64,
    read: Function,
    opts: JsValue,
    on_fragment: Function,
    on_pdf: Option<Function>,
    zero_copy: bool,
    on_progress: Option<Function>,
) -> Result<JsValue, JsError> {
    slidepdf::budget::reset_peak();
    let mut o = options(&opts)?;
    o.full_pdf = on_pdf.is_some() && o.full_pdf;
    let input = std::io::BufReader::with_capacity(
        64 << 10,
        JsReader {
            read,
            size: size as u64,
            pos: 0,
        },
    );
    let mut sink = on_pdf.map(|f| std::io::BufWriter::with_capacity(256 << 10, JsWriter(f, zero_copy)));
    let summary = {
        let full: Option<&mut dyn Write> = sink.as_mut().map(|w| w as &mut dyn Write);
        slidepdf::convert_stream_with(
            input,
            &o,
            full,
            &mut |f| {
                on_fragment
                    .call1(&JsValue::NULL, &fragment_js(&f, zero_copy))
                    .map(|_| ())
                    .map_err(|e| format!("onFragment failed: {e:?}"))
            },
            &mut |p| match &on_progress {
                Some(cb) => cb
                    .call2(
                        &JsValue::NULL,
                        &JsValue::from_f64(p.done as f64),
                        &JsValue::from_f64(p.total as f64),
                    )
                    .map(|v| v.as_bool().unwrap_or(true))
                    .unwrap_or(false),
                None => true,
            },
        )
        .map_err(|e| JsError::new(&e.0))?
    };
    if let Some(mut w) = sink {
        w.flush().map_err(|e| JsError::new(&e.to_string()))?;
    }
    let res = Object::new();
    set(&res, "manifestJson", summary.manifest.to_json());
    set(&res, "reportJson", summary.report.to_json());
    set(&res, "reportText", summary.report.to_text());
    Ok(res.into())
}

/// Merges PDFs (fragments or any PDF) into one, deduplicating resources.
#[cfg(feature = "assemble")]
#[wasm_bindgen]
pub fn assemble(parts: Array) -> Result<Uint8Array, JsError> {
    let owned: Vec<Vec<u8>> = parts.iter().map(|p| Uint8Array::new(&p).to_vec()).collect();
    let refs: Vec<&[u8]> = owned.iter().map(|v| v.as_slice()).collect();
    slidepdf::assemble(&refs).map(|v| bytes(&v)).map_err(|e| JsError::new(&e))
}

/// Streaming merge with bounded memory: `next()` returns the next input PDF
/// (Uint8Array) or null when done; the output goes to `onPdf(chunk)` as it
/// is written. Only one input is held in WebAssembly memory at a time.
/// Returns the page count.
#[cfg(feature = "assemble")]
#[wasm_bindgen(js_name = assembleStreamRaw)]
pub fn assemble_stream_raw(next: Function, on_pdf: Function, zero_copy: bool) -> Result<u32, JsError> {
    slidepdf::budget::reset_peak();
    let parts = std::iter::from_fn(|| match next.call0(&JsValue::NULL) {
        Ok(v) if v.is_null() || v.is_undefined() => None,
        Ok(v) => Some(Ok(Uint8Array::new(&v).to_vec())),
        Err(e) => Some(Err(format!("next() failed: {e:?}"))),
    });
    let mut out = std::io::BufWriter::with_capacity(256 << 10, JsWriter(on_pdf, zero_copy));
    let n = slidepdf::fragments::assemble_to(parts, &mut out).map_err(|e| JsError::new(&e))?;
    out.flush().map_err(|e| JsError::new(&e.to_string()))?;
    Ok(n as u32)
}

/// Splits any PDF into one standalone PDF per page.
#[cfg(feature = "assemble")]
#[wasm_bindgen]
pub fn split(pdf: &[u8]) -> Result<Array, JsError> {
    let pages = slidepdf::split(pdf).map_err(|e| JsError::new(&e))?;
    Ok(pages.iter().map(|p| JsValue::from(bytes(p))).collect())
}

/// FNV-1a hash used in manifests, to verify stored fragments.
#[wasm_bindgen(js_name = fragmentHash)]
pub fn fragment_hash(pdf: &[u8]) -> String {
    format!("{:016x}", slidepdf::pdf::fnv1a(pdf))
}
