//! Turns JPEG and PNG bytes into the smallest PDF image streams we can make
//! without a full imaging library.
//!
//! - JPEG is embedded untouched (DCTDecode) unless it is much larger than the
//!   size it is shown at, in which case it is decoded, box-downsampled and
//!   re-encoded (feature `downsample`).
//! - PNG is decoded, alpha goes to a separate soft mask, and pixels are
//!   re-compressed with PNG row predictors + Flate, which is what PNG itself
//!   does, so the stream ends up about as small as the original file.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
}

impl ColorSpace {
    pub fn comps(self) -> usize {
        match self {
            ColorSpace::Gray => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub width: u32,
    pub height: u32,
    pub space: ColorSpace,
    pub data: Vec<u8>,
    /// true: `data` is a JPEG (DCTDecode); false: predictor-15 Flate data.
    pub dct: bool,
    /// Adobe CMYK JPEGs store inverted values.
    pub invert: bool,
    /// Flate-compressed 8-bit alpha, predictor 15, same size as the image.
    pub smask: Option<Vec<u8>>,
}

/// Downsampling policy. `max_px` is the largest pixel size worth keeping,
/// computed from where the image is displayed and the target DPI.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub max_px: Option<(u32, u32)>,
    pub jpeg_quality: u8,
}

/// Images above this many pixels are refused instead of decoded, so a
/// hostile file cannot make us allocate gigabytes.
pub const MAX_PIXELS: u64 = 80_000_000;

pub fn prepare(bytes: &[u8], target: Target) -> Result<Prepared, String> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg(bytes, target)
    } else if bytes.starts_with(b"\x89PNG") {
        png_image(bytes, target)
    } else if bytes.starts_with(b"GIF8") {
        gif_image(bytes, target)
    } else {
        Err("unknown image format".into())
    }
}

/// Reads size and component count from the JPEG SOF marker, plus whether
/// an Adobe APP14 marker is present (inverted CMYK).
/// Progressive JPEGs cannot be decoded row by row: every coefficient of
/// the image is held in memory (about 3.5 MB per megapixel). Cap them
/// well below `MAX_PIXELS` so one picture cannot take a small container
/// down.
pub const MAX_PROGRESSIVE_PIXELS: u64 = 20_000_000;

/// GIFs are decoded whole into an RGBA canvas (4 bytes per pixel).
pub const MAX_GIF_PIXELS: u64 = 16_000_000;

struct JpegInfo {
    width: u32,
    height: u32,
    comps: u8,
    adobe: bool,
    progressive: bool,
}

fn jpeg_info(b: &[u8]) -> Option<JpegInfo> {
    let mut i = 2;
    let mut adobe = false;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xEE && b.get(i + 4..i + 9) == Some(b"Adobe") {
            adobe = true;
        }
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof && i + 9 < b.len() {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some(JpegInfo {
                width: w,
                height: h,
                comps: b[i + 9],
                adobe,
                progressive: matches!(marker, 0xC2 | 0xC6 | 0xCA | 0xCE),
            });
        }
        i += 2 + len;
    }
    None
}

fn needs_downsample(w: u32, h: u32, t: Target) -> Option<(u32, u32)> {
    let (mw, mh) = t.max_px?;
    // Only bother when it saves a real amount (more than ~1.25x per axis).
    if w as f32 > mw as f32 * 1.25 && h as f32 > mh as f32 * 1.25 {
        let s = (mw as f32 / w as f32).max(mh as f32 / h as f32);
        Some((((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1)))
    } else {
        None
    }
}

fn jpeg(bytes: &[u8], t: Target) -> Result<Prepared, String> {
    let JpegInfo {
        width: w,
        height: h,
        comps,
        adobe,
        progressive,
    } = jpeg_info(bytes).ok_or("unreadable JPEG header")?;
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("JPEG is {w}x{h} pixels, outside the supported size"));
    }
    if progressive && w as u64 * h as u64 > MAX_PROGRESSIVE_PIXELS {
        return Err(format!(
            "progressive JPEG is {w}x{h} pixels; progressive images above {} megapixels are skipped to bound memory (re-save it as baseline JPEG)",
            MAX_PROGRESSIVE_PIXELS / 1_000_000
        ));
    }
    let space = match comps {
        1 => ColorSpace::Gray,
        4 => ColorSpace::Cmyk,
        _ => ColorSpace::Rgb,
    };
    #[cfg(feature = "downsample")]
    if space != ColorSpace::Cmyk {
        if let Some((nw, nh)) = needs_downsample(w, h, t) {
            if let Ok(p) = reencode_jpeg(bytes, nw, nh, t.jpeg_quality) {
                if p.data.len() < bytes.len() {
                    return Ok(p);
                }
            }
        }
    }
    #[cfg(not(feature = "downsample"))]
    let _ = t;
    Ok(Prepared {
        width: w,
        height: h,
        space,
        data: bytes.to_vec(),
        dct: true,
        invert: space == ColorSpace::Cmyk && adobe,
        smask: None,
    })
}

#[cfg(feature = "downsample")]
fn reencode_jpeg(bytes: &[u8], nw: u32, nh: u32, quality: u8) -> Result<Prepared, String> {
    let mut dec = jpeg_decoder::Decoder::new(bytes);
    // Decode straight at 1/2, 1/4 or 1/8 scale when possible: the IDCT does
    // the first reduction for free, so a 12 MP photo never exists in memory.
    dec.scale(nw.min(u16::MAX as u32) as u16, nh.min(u16::MAX as u32) as u16)
        .map_err(|e| e.to_string())?;
    let pixels = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or("no JPEG info")?;
    if (info.width as u32, info.height as u32) == (nw, nh) {
        let (space, _) = match info.pixel_format {
            jpeg_decoder::PixelFormat::L8 => (ColorSpace::Gray, 1),
            jpeg_decoder::PixelFormat::RGB24 => (ColorSpace::Rgb, 3),
            _ => return Err("unsupported JPEG pixel format".into()),
        };
        let data = encode_jpeg(&pixels, nw, nh, space, quality)?;
        return Ok(Prepared {
            width: nw,
            height: nh,
            space,
            data,
            dct: true,
            invert: false,
            smask: None,
        });
    }
    let (space, comps) = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => (ColorSpace::Gray, 1),
        jpeg_decoder::PixelFormat::RGB24 => (ColorSpace::Rgb, 3),
        _ => return Err("unsupported JPEG pixel format".into()),
    };
    let small = resize(&pixels, info.width as u32, info.height as u32, comps, nw, nh);
    let data = encode_jpeg(&small, nw, nh, space, quality)?;
    Ok(Prepared {
        width: nw,
        height: nh,
        space,
        data,
        dct: true,
        invert: false,
        smask: None,
    })
}

#[cfg(feature = "downsample")]
fn encode_jpeg(px: &[u8], w: u32, h: u32, space: ColorSpace, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, quality);
    let ct = if space == ColorSpace::Gray {
        jpeg_encoder::ColorType::Luma
    } else {
        jpeg_encoder::ColorType::Rgb
    };
    enc.encode(px, w as u16, h as u16, ct).map_err(|e| e.to_string())?;
    Ok(out)
}

fn png_image(bytes: &[u8], t: Target) -> Result<Prepared, String> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    dec.set_limits(png::Limits {
        bytes: (MAX_PIXELS * 4) as usize,
    });
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let (w, h, interlaced) = {
        let i = reader.info();
        (i.width, i.height, i.interlaced)
    };
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("PNG is {w}x{h} pixels, outside the supported size"));
    }
    let (ct, _) = reader.output_color_type();
    let (space, has_alpha) = match ct {
        png::ColorType::Grayscale => (ColorSpace::Gray, false),
        png::ColorType::GrayscaleAlpha => (ColorSpace::Gray, true),
        png::ColorType::Rgb => (ColorSpace::Rgb, false),
        png::ColorType::Rgba => (ColorSpace::Rgb, true),
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    let comps = space.comps();
    let stride = comps + has_alpha as usize;
    let (nw, nh) = needs_downsample(w, h, t).unwrap_or((w, h));

    // Rows are box-averaged into the output size as they are decoded, so
    // only the (small) result and one row ever exist, never the full image.
    // Interlaced PNGs deliver rows out of order and are decoded whole.
    let (out, ow, oh) = if !interlaced {
        let (w_, h_, nw_, nh_) = (w as usize, h as usize, nw as usize, nh as usize);
        let mut out = vec![0u8; nw_ * nh_ * stride];
        let mut acc = vec![0u32; nw_ * stride];
        // Input column runs per output column, so a row is summed in
        // contiguous slices instead of indexing per pixel.
        let runs: Vec<(usize, usize)> = (0..nw_)
            .map(|ox| (ox * w_ / nw_, ((ox + 1) * w_ / nw_).max(ox * w_ / nw_ + 1).min(w_)))
            .collect();
        let mut rows_in_box = 0u32;
        let mut cur_oy = 0usize;
        let mut flush = |oy: usize, acc: &mut Vec<u32>, rows_in_box: u32| {
            let dst = &mut out[oy * nw_ * stride..(oy + 1) * nw_ * stride];
            for (ox, (a, d)) in acc.chunks_exact(stride).zip(dst.chunks_exact_mut(stride)).enumerate() {
                let n = ((runs[ox].1 - runs[ox].0) as u32 * rows_in_box).max(1);
                for c in 0..stride {
                    d[c] = ((a[c] + n / 2) / n) as u8;
                }
            }
            acc.iter_mut().for_each(|v| *v = 0);
        };
        for y in 0..h_ {
            let row = reader.next_row().map_err(|e| e.to_string())?.ok_or("PNG ended early")?;
            let data = row.data();
            let oy = y * nh_ / h_;
            if oy != cur_oy {
                flush(cur_oy, &mut acc, rows_in_box);
                cur_oy = oy;
                rows_in_box = 0;
            }
            rows_in_box += 1;
            accumulate_row(data, stride, &runs, &mut acc);
        }
        flush(cur_oy, &mut acc, rows_in_box);
        (out, nw, nh)
    } else {
        let mut buf = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(frame.buffer_size());
        if (nw, nh) != (w, h) {
            (resize(&buf, w, h, stride, nw, nh), nw, nh)
        } else {
            (buf, w, h)
        }
    };

    let (color, alpha) = if has_alpha {
        let mut c = Vec::with_capacity(out.len() / stride * comps);
        let mut a = Vec::with_capacity(out.len() / stride);
        for px in out.chunks_exact(stride) {
            c.extend_from_slice(&px[..comps]);
            a.push(px[comps]);
        }
        drop(out);
        let opaque = a.iter().all(|&v| v == 255);
        (c, if opaque { None } else { Some(a) })
    } else {
        (out, None)
    };
    Ok(Prepared {
        width: ow,
        height: oh,
        space,
        data: predict_and_deflate(&color, ow as usize, comps),
        dct: false,
        invert: false,
        smask: alpha.map(|a| predict_and_deflate(&a, ow as usize, 1)),
    })
}

/// First frame of a GIF (animation is not representable in PDF).
fn gif_image(bytes: &[u8], t: Target) -> Result<Prepared, String> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    // GIFs decode whole (an RGBA canvas plus the frame), so bound them tighter.
    opts.set_memory_limit(gif::MemoryLimit::Bytes(std::num::NonZeroU64::new(MAX_GIF_PIXELS * 4).unwrap()));
    opts.check_frame_consistency(true);
    let mut dec = opts.read_info(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (dec.width() as u32, dec.height() as u32);
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_GIF_PIXELS {
        return Err(format!("GIF is {w}x{h} pixels, outside the supported size"));
    }
    let (fl, ft, fw, fh, interlaced) = {
        let f = dec.next_frame_info().map_err(|e| e.to_string())?.ok_or("GIF has no frames")?;
        (f.left as u32, f.top as u32, f.width as u32, f.height as u32, f.interlaced)
    };
    let (nw, nh) = needs_downsample(w, h, t).unwrap_or((w, h));
    // Common case (a frame covering the screen, not interlaced): decode row
    // by row and box-average into the output, so the full image never
    // exists in memory, as for PNG.
    if (fl, ft, fw, fh) == (0, 0, w, h) && !interlaced {
        let (w_, h_, nw_, nh_) = (w as usize, h as usize, nw as usize, nh as usize);
        let runs: Vec<(usize, usize)> = (0..nw_)
            .map(|ox| (ox * w_ / nw_, ((ox + 1) * w_ / nw_).max(ox * w_ / nw_ + 1).min(w_)))
            .collect();
        let mut out = vec![0u8; nw_ * nh_ * 4];
        let mut acc = vec![0u32; nw_ * 4];
        let mut row = vec![0u8; w_ * 4];
        let (mut cur_oy, mut rows_in_box) = (0usize, 0u32);
        let mut flush = |oy: usize, acc: &mut Vec<u32>, rows: u32| {
            for (ox, a) in acc.as_chunks::<4>().0.iter().enumerate() {
                let n = ((runs[ox].1 - runs[ox].0) as u32 * rows).max(1);
                for c in 0..4 {
                    out[(oy * nw_ + ox) * 4 + c] = ((a[c] + n / 2) / n) as u8;
                }
            }
            acc.iter_mut().for_each(|v| *v = 0);
        };
        for y in 0..h_ {
            if !dec.fill_buffer(&mut row).map_err(|e| e.to_string())? {
                break;
            }
            let oy = y * nh_ / h_;
            if oy != cur_oy {
                flush(cur_oy, &mut acc, rows_in_box);
                cur_oy = oy;
                rows_in_box = 0;
            }
            rows_in_box += 1;
            accumulate_row(&row, 4, &runs, &mut acc);
        }
        flush(cur_oy, &mut acc, rows_in_box);
        return Ok(rgba_prepared(&out, nw, nh));
    }
    // Partial or interlaced frame: read it whole (the decoder deinterlaces)
    // and place it on a transparent canvas of the logical screen size.
    let mut frame_px = vec![0u8; dec.buffer_size()];
    dec.read_into_buffer(&mut frame_px).map_err(|e| e.to_string())?;
    if (fl, ft, fw, fh) == (0, 0, w, h) && frame_px.len() == (w * h * 4) as usize {
        // Interlaced full-screen frame: the decoded frame is the canvas.
        let px = if (nw, nh) != (w, h) {
            resize(&frame_px, w, h, 4, nw, nh)
        } else {
            frame_px
        };
        return Ok(rgba_prepared(&px, nw, nh));
    }
    let mut canvas = vec![0u8; (w * h * 4) as usize];
    for y in 0..fh.min(h.saturating_sub(ft)) {
        for x in 0..fw.min(w.saturating_sub(fl)) {
            let s = ((y * fw + x) * 4) as usize;
            let d = (((y + ft) * w + x + fl) * 4) as usize;
            if let (Some(src), true) = (frame_px.get(s..s + 4), d + 4 <= canvas.len()) {
                canvas[d..d + 4].copy_from_slice(src);
            }
        }
    }
    let px = if (nw, nh) != (w, h) {
        resize(&canvas, w, h, 4, nw, nh)
    } else {
        canvas
    };
    Ok(rgba_prepared(&px, nw, nh))
}

/// RGBA pixels -> RGB image plus an alpha soft mask when not opaque.
fn rgba_prepared(px: &[u8], nw: u32, nh: u32) -> Prepared {
    let mut color = Vec::with_capacity(px.len() / 4 * 3);
    let mut alpha = Vec::with_capacity(px.len() / 4);
    for p in px.as_chunks::<4>().0 {
        color.extend_from_slice(&p[..3]);
        alpha.push(p[3]);
    }
    let opaque = alpha.iter().all(|&a| a == 255);
    Prepared {
        width: nw,
        height: nh,
        space: ColorSpace::Rgb,
        data: predict_and_deflate(&color, nw as usize, 3),
        dct: false,
        invert: false,
        smask: (!opaque).then(|| predict_and_deflate(&alpha, nw as usize, 1)),
    }
}

/// Adds one input row into the per-output-column accumulators.
fn accumulate_row(data: &[u8], stride: usize, runs: &[(usize, usize)], acc: &mut [u32]) {
    match stride {
        1 => accumulate_row_n::<1>(data, runs, acc),
        2 => accumulate_row_n::<2>(data, runs, acc),
        3 => accumulate_row_n::<3>(data, runs, acc),
        4 => accumulate_row_n::<4>(data, runs, acc),
        _ => unreachable!("PNG stride is 1..4 bytes"),
    }
}

fn accumulate_row_n<const N: usize>(data: &[u8], runs: &[(usize, usize)], acc: &mut [u32]) {
    let (acc, _) = acc.as_chunks_mut::<N>();
    for (a, &(x0, x1)) in acc.iter_mut().zip(runs) {
        let (pixels, _) = data[x0 * N..x1 * N].as_chunks::<N>();
        let mut sum = [0u32; N];
        for px in pixels {
            for c in 0..N {
                sum[c] += px[c] as u32;
            }
        }
        for c in 0..N {
            a[c] += sum[c];
        }
    }
}

/// Area-averaging (box) downsampler. Good quality for reduction, tiny code.
pub fn resize(src: &[u8], w: u32, h: u32, comps: usize, nw: u32, nh: u32) -> Vec<u8> {
    let (w, h, nw, nh) = (w as usize, h as usize, nw as usize, nh as usize);
    let mut out = vec![0u8; nw * nh * comps];
    let mut acc = vec![0u32; comps];
    for oy in 0..nh {
        let y0 = oy * h / nh;
        let y1 = ((oy + 1) * h / nh).max(y0 + 1).min(h);
        for ox in 0..nw {
            let x0 = ox * w / nw;
            let x1 = ((ox + 1) * w / nw).max(x0 + 1).min(w);
            acc.iter_mut().for_each(|a| *a = 0);
            for y in y0..y1 {
                let row = &src[(y * w + x0) * comps..(y * w + x1) * comps];
                for px in row.chunks_exact(comps) {
                    for (a, &v) in acc.iter_mut().zip(px) {
                        *a += v as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (oy * nw + ox) * comps;
            for c in 0..comps {
                out[o + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// Applies PNG row filters (choosing the best per row, like libpng's
/// heuristic) and deflates. Pair with `/DecodeParms << /Predictor 15 >>`.
pub fn predict_and_deflate(px: &[u8], width: usize, comps: usize) -> Vec<u8> {
    let stride = width * comps;
    if stride == 0 {
        return miniz_oxide::deflate::compress_to_vec_zlib(&[], 9);
    }
    let rows = px.len() / stride;
    let mut out = Vec::with_capacity(px.len() + rows);
    let zero = vec![0u8; stride];
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for r in 0..rows {
        let cur = &px[r * stride..(r + 1) * stride];
        let up = if r == 0 { &zero[..] } else { &px[(r - 1) * stride..r * stride] };
        let mut best_score = u64::MAX;
        let mut best_kind = 0u8;
        for kind in 0..5u8 {
            apply_filter(kind, comps, cur, up, &mut cand);
            let score: u64 = cand.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if score < best_score {
                best_score = score;
                best_kind = kind;
                std::mem::swap(&mut best, &mut cand);
            }
        }
        out.push(best_kind);
        out.extend_from_slice(&best);
    }
    // The per-row heuristic picks filters that help photos but often hurt
    // flat graphics (icons, screenshots). Deflating both ways and keeping
    // the smaller one is cheap at these sizes and always wins.
    let filtered = miniz_oxide::deflate::compress_to_vec_zlib(&out, 9);
    drop(out);
    let mut plain = Vec::with_capacity(px.len() + rows);
    for r in 0..rows {
        plain.push(0);
        plain.extend_from_slice(&px[r * stride..(r + 1) * stride]);
    }
    let unfiltered = miniz_oxide::deflate::compress_to_vec_zlib(&plain, 9);
    if unfiltered.len() < filtered.len() {
        unfiltered
    } else {
        filtered
    }
}

/// One PNG row filter, written as separate tight loops (no per-byte
/// branching on the filter type) so the compiler can vectorize them.
fn apply_filter(kind: u8, bpp: usize, cur: &[u8], up: &[u8], out: &mut [u8]) {
    let n = cur.len();
    match kind {
        0 => out.copy_from_slice(cur),
        1 => {
            out[..bpp].copy_from_slice(&cur[..bpp]);
            for i in bpp..n {
                out[i] = cur[i].wrapping_sub(cur[i - bpp]);
            }
        }
        2 => {
            for ((o, &c), &u) in out.iter_mut().zip(cur).zip(up) {
                *o = c.wrapping_sub(u);
            }
        }
        3 => {
            for i in 0..bpp {
                out[i] = cur[i].wrapping_sub(up[i] / 2);
            }
            for i in bpp..n {
                out[i] = cur[i].wrapping_sub(((cur[i - bpp] as u16 + up[i] as u16) / 2) as u8);
            }
        }
        _ => {
            for i in 0..bpp {
                out[i] = cur[i].wrapping_sub(up[i]);
            }
            for i in bpp..n {
                out[i] = cur[i].wrapping_sub(paeth(cur[i - bpp], up[i], up[i - bpp]));
            }
        }
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_resize_averages() {
        let src = [0, 100, 200, 255];
        assert_eq!(resize(&src, 2, 2, 1, 1, 1), vec![139]);
    }

    #[test]
    fn predictor_output_inflates_to_filtered_rows() {
        let px: Vec<u8> = (0..48).collect();
        let z = predict_and_deflate(&px, 4, 3);
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&z).unwrap();
        assert_eq!(raw.len(), 4 * (1 + 12));
    }
}
