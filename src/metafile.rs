//! Windows metafiles (EMF and WMF) -> IR shapes.
//!
//! Logos, icons and the previews of embedded Excel/Word objects are often
//! stored as metafiles: a list of GDI drawing calls. They are replayed here
//! into ordinary IR shapes (paths, text boxes and pictures), so they stay
//! vector in the PDF. Supported: the drawing state (pens, brushes, fonts,
//! colors, window/viewport mapping, world transforms, save/restore), lines,
//! polylines, polygons, poly-polygons, Béziers, rectangles, rounded
//! rectangles, ellipses, GDI paths, pattern fills, text and embedded device
//! independent bitmaps. Not supported: clipping regions and raster
//! operations other than plain copy; EMF+ records are skipped in favor of
//! the EMF fallback records that dual files carry.
//!
//! Every read is bounds-checked: these are binary formats from untrusted
//! decks, and a malformed record ends the replay, never the process.

use crate::ir::*;

/// Bitmaps extracted from a metafile, encoded as PNG.
pub type Bitmaps = Vec<Vec<u8>>;

const MAX_RECORDS: usize = 500_000;
const MAX_ELEMENTS: usize = 50_000;
const MAX_POINTS: usize = 200_000;
const MAX_BITMAP_PIXELS: u64 = 16_000_000;

struct Reader<'a> {
    b: &'a [u8],
}

impl Reader<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        self.b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn i16(&self, at: usize) -> Option<i16> {
        self.u16(at).map(|v| v as i16)
    }
    fn u32(&self, at: usize) -> Option<u32> {
        self.b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&self, at: usize) -> Option<i32> {
        self.u32(at).map(|v| v as i32)
    }
    fn f32(&self, at: usize) -> Option<f32> {
        self.u32(at).map(f32::from_bits).filter(|v| v.is_finite())
    }
    fn color(&self, at: usize) -> Option<Rgb> {
        let s = self.b.get(at..at + 3)?;
        Some(Rgb(s[0], s[1], s[2]))
    }
}

#[derive(Clone, Copy)]
enum Obj {
    Pen {
        color: Rgb,
        width: f32,
        none: bool,
    },
    Brush {
        color: Rgb,
        none: bool,
    },
    Font {
        height: f32,
        bold: bool,
        italic: bool,
        face: usize,
    },
    Other,
}

#[derive(Clone, Copy)]
struct State {
    pen: (Rgb, f32, bool),
    brush: (Rgb, bool),
    font: (f32, bool, bool, usize),
    text_color: Rgb,
    text_align: u32,
    win_org: (f32, f32),
    win_ext: (f32, f32),
    vp_org: (f32, f32),
    vp_ext: (f32, f32),
    map_mode: u32,
    world: [f32; 6],
    pos: (f32, f32),
    even_odd: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            pen: (Rgb::BLACK, 1.0, false),
            brush: (Rgb::WHITE, false),
            font: (12.0, false, false, usize::MAX),
            text_color: Rgb::BLACK,
            text_align: 0,
            win_org: (0.0, 0.0),
            win_ext: (1.0, 1.0),
            vp_org: (0.0, 0.0),
            vp_ext: (1.0, 1.0),
            map_mode: 1,
            world: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            pos: (0.0, 0.0),
            even_odd: true,
        }
    }
}

/// Replays drawing calls into IR elements placed in `target`.
struct Canvas {
    target: Rect,
    /// Device-space box that maps onto `target`.
    bounds: (f32, f32, f32, f32),
    out: Vec<Element>,
    bitmaps: Bitmaps,
    faces: Vec<String>,
    state: State,
    stack: Vec<State>,
    objects: Vec<Option<Obj>>,
    path: Vec<PathCmd>,
    in_path: bool,
    points: usize,
    wmf: bool,
}

impl Canvas {
    fn new(target: Rect, bounds: (f32, f32, f32, f32), wmf: bool) -> Self {
        Canvas {
            target,
            bounds,
            out: Vec::new(),
            bitmaps: Vec::new(),
            faces: Vec::new(),
            state: State::default(),
            stack: Vec::new(),
            objects: Vec::new(),
            path: Vec::new(),
            in_path: false,
            points: 0,
            wmf,
        }
    }

    /// Logical -> device coordinates.
    fn device(&self, x: f32, y: f32) -> (f32, f32) {
        let w = &self.state.world;
        let (x, y) = (w[0] * x + w[2] * y + w[4], w[1] * x + w[3] * y + w[5]);
        let s = &self.state;
        if self.wmf {
            // WMF: logical units are mapped onto the window (see `bounds`).
            (x, y)
        } else if s.map_mode == 8 || s.map_mode == 7 {
            // Anisotropic/isotropic (and WMF, mapped by window later).
            let sx = if s.win_ext.0 != 0.0 { s.vp_ext.0 / s.win_ext.0 } else { 1.0 };
            let sy = if s.win_ext.1 != 0.0 { s.vp_ext.1 / s.win_ext.1 } else { 1.0 };
            ((x - s.win_org.0) * sx + s.vp_org.0, (y - s.win_org.1) * sy + s.vp_org.1)
        } else {
            (x - s.win_org.0 + s.vp_org.0, y - s.win_org.1 + s.vp_org.1)
        }
    }

    /// Logical -> unit coordinates of the target box.
    fn unit(&self, x: f32, y: f32) -> (f32, f32) {
        let (dx, dy) = self.device(x, y);
        let (l, t, r, b) = self.bounds;
        let u = if r != l { (dx - l) / (r - l) } else { 0.0 };
        let v = if b != t { (dy - t) / (b - t) } else { 0.0 };
        (if u.is_finite() { u } else { 0.0 }, if v.is_finite() { v } else { 0.0 })
    }

    /// Size of a logical length in EMU (average of both axes).
    fn emu_len(&self, len: f32) -> f32 {
        let (a, b) = (self.unit(0.0, 0.0), self.unit(len, len));
        let v = ((b.0 - a.0).abs() * self.target.w + (b.1 - a.1).abs() * self.target.h) / 2.0;
        if v.is_finite() {
            v
        } else {
            0.0
        }
    }

    fn full(&self) -> bool {
        self.out.len() >= MAX_ELEMENTS || self.points >= MAX_POINTS
    }

    fn emit(&mut self, cmds: Vec<PathCmd>, fill: bool, stroke: bool) {
        if cmds.is_empty() || self.full() {
            return;
        }
        self.points += cmds.len();
        let fill = (fill && !self.state.brush.1).then_some(self.state.brush.0);
        let stroke = (stroke && !self.state.pen.2).then(|| Stroke {
            color: self.state.pen.0,
            width: self.emu_len(self.state.pen.1.max(1.0)).max(3175.0),
        });
        if fill.is_none() && stroke.is_none() {
            return;
        }
        let geometry = if fill.is_none() {
            Geometry::Open(cmds)
        } else if self.state.even_odd {
            Geometry::PathEvenOdd(cmds)
        } else {
            Geometry::Path(cmds)
        };
        self.out.push(Element::Shape(Shape {
            rect: self.target,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry,
            fill: fill.map(Fill::Solid),
            stroke,
            text: None,
            is_title: false,
            text_rect: None,
        }));
    }

    fn poly(&mut self, pts: &[(f32, f32)], close: bool) -> Vec<PathCmd> {
        let mut v = Vec::with_capacity(pts.len() + 1);
        for (i, p) in pts.iter().enumerate() {
            let (u, w) = self.unit(p.0, p.1);
            v.push(if i == 0 { PathCmd::Move(u, w) } else { PathCmd::Line(u, w) });
        }
        if close && !v.is_empty() {
            v.push(PathCmd::Close);
        }
        v
    }

    fn shape_or_path(&mut self, cmds: Vec<PathCmd>, fill: bool, stroke: bool) {
        if self.in_path {
            self.path.extend(cmds);
        } else {
            self.emit(cmds, fill, stroke);
        }
    }

    fn rect(&mut self, l: f32, t: f32, r: f32, b: f32) {
        let c = self.poly(&[(l, t), (r, t), (r, b), (l, b)], true);
        self.shape_or_path(c, true, true);
    }

    fn ellipse(&mut self, l: f32, t: f32, r: f32, b: f32) {
        let (cx, cy, rx, ry) = ((l + r) / 2.0, (t + b) / 2.0, (r - l) / 2.0, (b - t) / 2.0);
        let k = 0.5523;
        let pts = [
            (cx + rx, cy),
            (cx + rx, cy + ky(ry, k)),
            (cx + kx(rx, k), cy + ry),
            (cx, cy + ry),
            (cx - kx(rx, k), cy + ry),
            (cx - rx, cy + ky(ry, k)),
            (cx - rx, cy),
            (cx - rx, cy - ky(ry, k)),
            (cx - kx(rx, k), cy - ry),
            (cx, cy - ry),
            (cx + kx(rx, k), cy - ry),
            (cx + rx, cy - ky(ry, k)),
            (cx + rx, cy),
        ];
        let u: Vec<(f32, f32)> = pts.iter().map(|p| self.unit(p.0, p.1)).collect();
        let mut cmds = vec![PathCmd::Move(u[0].0, u[0].1)];
        for seg in u[1..].as_chunks::<3>().0 {
            cmds.push(PathCmd::Cubic(seg[0].0, seg[0].1, seg[1].0, seg[1].1, seg[2].0, seg[2].1));
        }
        cmds.push(PathCmd::Close);
        self.shape_or_path(cmds, true, true);
    }

    fn text(&mut self, x: f32, y: f32, s: &str) {
        let s = s.trim_end_matches('\0');
        if s.trim().is_empty() || self.full() {
            return;
        }
        let (h, bold, italic, face) = self.state.font;
        let size_emu = self.emu_len(h.abs().max(1.0));
        let size = (size_emu / EMU_PER_PT).clamp(1.0, 400.0);
        let face = self.faces.get(face).cloned().unwrap_or_default();
        let (u, v) = self.unit(x, y);
        let (px, py) = (self.target.x + u * self.target.w, self.target.y + v * self.target.h);
        let width = s.chars().count() as f32 * size * 0.6 * EMU_PER_PT;
        let height = size * 1.3 * EMU_PER_PT;
        let align = self.state.text_align;
        // TA_CENTER = 6, TA_RIGHT = 2 (horizontal bits); TA_BASELINE = 24, TA_BOTTOM = 8.
        let (x0, al) = match align & 6 {
            6 => (px - width / 2.0, Align::Center),
            2 => (px - width, Align::Right),
            _ => (px, Align::Left),
        };
        let y0 = match align & 24 {
            24 => py - size * 0.8 * EMU_PER_PT,
            8 => py - height,
            _ => py,
        };
        let family = if face.to_ascii_lowercase().contains("times") || face.to_ascii_lowercase().contains("serif") {
            Family::Serif
        } else if face.to_ascii_lowercase().contains("courier") {
            Family::Mono
        } else {
            Family::Sans
        };
        let run = Run {
            text: s.to_string(),
            size,
            bold,
            italic,
            underline: false,
            color: self.state.text_color,
            family,
            face,
            line_break: false,
        };
        self.out.push(Element::Shape(Shape {
            rect: Rect {
                x: x0,
                y: y0,
                w: width,
                h: height,
            },
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry: Geometry::Rect,
            fill: None,
            stroke: None,
            text: Some(TextBody {
                insets: [0.0; 4],
                rotation: 0.0,
                anchor: Anchor::Top,
                wrap: false,
                font_scale: 1.0,
                line_reduction: 0.0,
                paragraphs: vec![Paragraph {
                    align: al,
                    margin_left: 0.0,
                    indent: 0.0,
                    bullet: None,
                    line_spacing: 1.0,
                    line_spacing_pt: None,
                    space_before: 0.0,
                    space_after: 0.0,
                    runs: vec![run],
                    end_size: size,
                }],
            }),
            is_title: false,
            text_rect: None,
        }));
    }

    /// Places a decoded bitmap (PNG bytes) in the logical rectangle.
    fn bitmap(&mut self, png: Vec<u8>, l: f32, t: f32, r: f32, b: f32) {
        if self.full() {
            return;
        }
        let (a, c) = (self.unit(l, t), self.unit(r, b));
        let (u0, u1) = (a.0.min(c.0), a.0.max(c.0));
        let (v0, v1) = (a.1.min(c.1), a.1.max(c.1));
        let rect = Rect {
            x: self.target.x + u0 * self.target.w,
            y: self.target.y + v0 * self.target.h,
            w: (u1 - u0) * self.target.w,
            h: (v1 - v0) * self.target.h,
        };
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let idx = self.bitmaps.len();
        self.bitmaps.push(png);
        self.out.push(Element::Picture(Picture {
            rect,
            rotation: 0.0,
            media: format!("#bitmap{idx}"),
            crop: [0.0; 4],
            flip_h: false,
            flip_v: false,
        }));
    }

    fn create(&mut self, slot: Option<usize>, o: Obj) {
        match slot {
            Some(i) if i < 65_536 => {
                if self.objects.len() <= i {
                    self.objects.resize(i + 1, None);
                }
                self.objects[i] = Some(o);
            }
            Some(_) => {}
            None => {
                // WMF: first free slot.
                match self.objects.iter().position(|o| o.is_none()) {
                    Some(i) => self.objects[i] = Some(o),
                    None if self.objects.len() < 65_536 => self.objects.push(Some(o)),
                    None => {}
                }
            }
        }
    }

    fn select(&mut self, i: usize) {
        // EMF stock objects have the high bit set.
        let stock = i & 0x8000_0000 != 0;
        if stock {
            match i & 0x7fff_ffff {
                0 => self.state.brush = (Rgb::WHITE, false),
                1 => self.state.brush = (Rgb(192, 192, 192), false),
                2 => self.state.brush = (Rgb(128, 128, 128), false),
                3 => self.state.brush = (Rgb(64, 64, 64), false),
                4 => self.state.brush = (Rgb::BLACK, false),
                5 => self.state.brush = (Rgb::WHITE, true),
                6 => self.state.pen = (Rgb::WHITE, 1.0, false),
                7 => self.state.pen = (Rgb::BLACK, 1.0, false),
                8 => self.state.pen = (Rgb::BLACK, 1.0, true),
                _ => {}
            }
            return;
        }
        match self.objects.get(i).copied().flatten() {
            Some(Obj::Pen { color, width, none }) => self.state.pen = (color, width, none),
            Some(Obj::Brush { color, none }) => self.state.brush = (color, none),
            Some(Obj::Font {
                height,
                bold,
                italic,
                face,
            }) => self.state.font = (height, bold, italic, face),
            _ => {}
        }
    }

    fn face(&mut self, name: String) -> usize {
        self.faces.push(name);
        self.faces.len() - 1
    }
}

fn kx(r: f32, k: f32) -> f32 {
    r * k
}
fn ky(r: f32, k: f32) -> f32 {
    r * k
}

/// Device-independent bitmap -> PNG. Supports 1/4/8/24/32 bpp uncompressed.
fn dib_to_png(b: &[u8]) -> Option<Vec<u8>> {
    let r = Reader { b };
    let header = r.u32(0)? as usize;
    if header < 40 {
        return None;
    }
    let w = r.i32(4)?;
    let h = r.i32(8)?;
    let bpp = r.u16(14)? as usize;
    let compression = r.u32(16)?;
    if !(compression == 0 || compression == 3) || w <= 0 || h == 0 {
        return None;
    }
    let (w, top_down, h) = (w as usize, h < 0, h.unsigned_abs() as usize);
    if (w as u64) * (h as u64) > MAX_BITMAP_PIXELS {
        return None;
    }
    let colors_used = r.u32(32)? as usize;
    let palette_len = if bpp <= 8 {
        if colors_used > 0 {
            colors_used.min(256)
        } else {
            1 << bpp
        }
    } else {
        0
    };
    let masks = if compression == 3 && header == 40 { 12 } else { 0 };
    let pal_at = header + masks;
    let mut palette = Vec::with_capacity(palette_len);
    for i in 0..palette_len {
        let p = b.get(pal_at + i * 4..pal_at + i * 4 + 3)?;
        palette.push([p[2], p[1], p[0]]);
    }
    let data_at = pal_at + palette_len * 4;
    let stride = (w * bpp).div_ceil(32) * 4;
    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let line = b.get(data_at + src_row * stride..data_at + src_row * stride + stride)?;
        for x in 0..w {
            let px = &mut rgba[(row * w + x) * 4..(row * w + x) * 4 + 4];
            let c = match bpp {
                1 | 4 | 8 => {
                    let bit = x * bpp;
                    let byte = *line.get(bit / 8)?;
                    let idx = ((byte >> (8 - bpp - bit % 8)) & ((1u16 << bpp) - 1) as u8) as usize;
                    let p = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    [p[0], p[1], p[2], 255]
                }
                24 => {
                    let s = line.get(x * 3..x * 3 + 3)?;
                    [s[2], s[1], s[0], 255]
                }
                32 => {
                    let s = line.get(x * 4..x * 4 + 4)?;
                    if s[3] != 0 {
                        any_alpha = true;
                    }
                    [s[2], s[1], s[0], s[3]]
                }
                16 => {
                    let v = u16::from_le_bytes([*line.get(x * 2)?, *line.get(x * 2 + 1)?]);
                    let (r5, g5, b5) = ((v >> 10) & 31, (v >> 5) & 31, v & 31);
                    [(r5 * 255 / 31) as u8, (g5 * 255 / 31) as u8, (b5 * 255 / 31) as u8, 255]
                }
                _ => return None,
            };
            px.copy_from_slice(&c);
        }
    }
    if bpp == 32 && !any_alpha {
        // 32-bit DIBs without alpha data are opaque.
        rgba.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&rgba).ok()?;
    }
    Some(out)
}

fn utf16(b: &[u8], at: usize, chars: usize) -> Option<String> {
    let s = b.get(at..at + chars.min(10_000) * 2)?;
    let units: Vec<u16> = s.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    Some(String::from_utf16_lossy(&units))
}

fn latin1(b: &[u8], at: usize, len: usize) -> Option<String> {
    let s = b.get(at..at + len.min(10_000))?;
    Some(s.iter().map(|&c| c as char).collect())
}

/// Replays a metafile into elements inside `target`. Returns the elements,
/// the extracted bitmaps (referenced as `#bitmapN`) and a warning when the
/// file could not be read.
pub fn render(bytes: &[u8], target: Rect) -> Result<(Vec<Element>, Bitmaps), String> {
    if bytes.len() >= 44 && u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == 1 && bytes.get(40..44) == Some(b" EMF") {
        emf(bytes, target)
    } else {
        wmf(bytes, target)
    }
}

fn emf(b: &[u8], target: Rect) -> Result<(Vec<Element>, Bitmaps), String> {
    let r = Reader { b };
    let bad = || "malformed EMF".to_string();
    let bounds = (
        r.i32(8).ok_or_else(bad)? as f32,
        r.i32(12).ok_or_else(bad)? as f32,
        r.i32(16).ok_or_else(bad)? as f32,
        r.i32(20).ok_or_else(bad)? as f32,
    );
    // Bounds are inclusive device pixels.
    let bounds = (bounds.0, bounds.1, bounds.2 + 1.0, bounds.3 + 1.0);
    let mut c = Canvas::new(target, bounds, false);
    let mut at = 0usize;
    for _ in 0..MAX_RECORDS {
        let (Some(kind), Some(size)) = (r.u32(at), r.u32(at + 4)) else {
            break;
        };
        let size = size as usize;
        if size < 8 || at + size > b.len() {
            break;
        }
        let p = at + 8; // record payload
        let pt16 = |i: usize| -> Option<(f32, f32)> { Some((r.i16(i)? as f32, r.i16(i + 2)? as f32)) };
        let pt32 = |i: usize| -> Option<(f32, f32)> { Some((r.i32(i)? as f32, r.i32(i + 4)? as f32)) };
        match kind {
            14 => break, // EOF
            9 => c.state.win_ext = pt32(p).unwrap_or(c.state.win_ext),
            10 => c.state.win_org = pt32(p).unwrap_or(c.state.win_org),
            11 => c.state.vp_ext = pt32(p).unwrap_or(c.state.vp_ext),
            12 => c.state.vp_org = pt32(p).unwrap_or(c.state.vp_org),
            17 => c.state.map_mode = r.u32(p).unwrap_or(1),
            19 => c.state.even_odd = r.u32(p) == Some(1),
            22 => c.state.text_align = r.u32(p).unwrap_or(0),
            24 => c.state.text_color = r.color(p).unwrap_or(Rgb::BLACK),
            27 => {
                c.state.pos = pt32(p).unwrap_or(c.state.pos);
                if c.in_path {
                    let (u, v) = c.unit(c.state.pos.0, c.state.pos.1);
                    c.path.push(PathCmd::Move(u, v));
                }
            }
            33 => {
                if c.stack.len() < 1000 {
                    c.stack.push(c.state);
                }
            }
            34 => {
                // RestoreDC(n): n is negative, relative to the current level.
                let n = r.i32(p).unwrap_or(-1);
                let pops = if n < 0 {
                    n.unsigned_abs() as usize
                } else {
                    c.stack.len().saturating_sub(n as usize - 1).max(1)
                };
                for _ in 0..pops.min(c.stack.len()) {
                    if let Some(s) = c.stack.pop() {
                        c.state = s;
                    }
                }
            }
            35 => {
                if let Some(m) = (0..6).map(|i| r.f32(p + i * 4)).collect::<Option<Vec<f32>>>() {
                    c.state.world = [m[0], m[1], m[2], m[3], m[4], m[5]];
                }
            }
            36 => {
                let m: Option<Vec<f32>> = (0..6).map(|i| r.f32(p + i * 4)).collect();
                let mode = r.u32(p + 24).unwrap_or(1);
                if let Some(m) = m {
                    let w = c.state.world;
                    c.state.world = match mode {
                        1 => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                        2 => mul(&m, &w),
                        3 => mul(&w, &m),
                        4 => [m[0], m[1], m[2], m[3], m[4], m[5]],
                        _ => w,
                    };
                }
            }
            37 => c.select(r.u32(p).unwrap_or(0) as usize),
            38 => {
                // CreatePen: ihPen, LOGPEN { style, width(POINTL), color }
                let slot = r.u32(p).map(|v| v as usize);
                let style = r.u32(p + 4).unwrap_or(0);
                let width = r.i32(p + 8).unwrap_or(1) as f32;
                let color = r.color(p + 16).unwrap_or(Rgb::BLACK);
                c.create(
                    slot,
                    Obj::Pen {
                        color,
                        width,
                        none: style & 0xF == 5,
                    },
                );
            }
            95 => {
                // ExtCreatePen: ihPen, offBmi, cbBmi, offBits, cbBits, LOGPEN32 { style, width, brushStyle, color }
                let slot = r.u32(p).map(|v| v as usize);
                let style = r.u32(p + 20).unwrap_or(0);
                let width = r.u32(p + 24).unwrap_or(1) as f32;
                let color = r.color(p + 32).unwrap_or(Rgb::BLACK);
                c.create(
                    slot,
                    Obj::Pen {
                        color,
                        width,
                        none: style & 0xF == 5,
                    },
                );
            }
            39 => {
                // CreateBrushIndirect: ihBrush, LOGBRUSH32 { style, color, hatch }
                let slot = r.u32(p).map(|v| v as usize);
                let style = r.u32(p + 4).unwrap_or(0);
                let color = r.color(p + 8).unwrap_or(Rgb::WHITE);
                c.create(slot, Obj::Brush { color, none: style == 1 });
            }
            82 => {
                // ExtCreateFontIndirectW: ihFont, LOGFONTW { height, width, esc, orient, weight, italic, ..., faceName[32] at 28 }
                let slot = r.u32(p).map(|v| v as usize);
                let height = r.i32(p + 4).unwrap_or(12) as f32;
                let weight = r.i32(p + 20).unwrap_or(400);
                let italic = b.get(p + 24).copied().unwrap_or(0) != 0;
                let face = utf16(b, p + 32, 32).unwrap_or_default();
                let face = face.split('\0').next().unwrap_or("").to_string();
                let fi = c.face(face);
                c.create(
                    slot,
                    Obj::Font {
                        height,
                        bold: weight >= 600,
                        italic,
                        face: fi,
                    },
                );
            }
            40 => {
                let i = r.u32(p).unwrap_or(u32::MAX) as usize;
                if let Some(o) = c.objects.get_mut(i) {
                    *o = Some(Obj::Other);
                    *o = None;
                }
            }
            54 => {
                if let Some(to) = pt32(p) {
                    let from = c.state.pos;
                    let cmds = c.poly(&[from, to], false);
                    if c.in_path {
                        c.path.push(cmds[1]);
                    } else {
                        c.emit(cmds, false, true);
                    }
                    c.state.pos = to;
                }
            }
            42..=44 => {
                if let (Some(a), Some(z)) = (pt32(p), pt32(p + 8)) {
                    if kind == 42 {
                        c.ellipse(a.0, a.1, z.0, z.1);
                    } else {
                        c.rect(a.0, a.1, z.0, z.1);
                    }
                }
            }
            // 16-bit point polylines: bounds(16), count(4), points
            86 | 87 | 85 | 88 | 89 | 3 | 4 | 2 | 5 | 6 => {
                let wide = matches!(kind, 2..=6);
                let n = r.u32(p + 16).unwrap_or(0) as usize;
                let step = if wide { 8 } else { 4 };
                if n > MAX_POINTS || p + 20 + n * step > at + size {
                    at += size;
                    continue;
                }
                let pts: Vec<(f32, f32)> = (0..n)
                    .filter_map(|i| if wide { pt32(p + 20 + i * 8) } else { pt16(p + 20 + i * 4) })
                    .collect();
                match kind {
                    86 | 3 => {
                        let cmds = c.poly(&pts, true);
                        c.shape_or_path(cmds, true, true);
                    }
                    87 | 4 => {
                        let cmds = c.poly(&pts, false);
                        c.shape_or_path(cmds, false, true);
                    }
                    89 | 6 => {
                        // PolylineTo: continues from the current position.
                        let mut all = vec![c.state.pos];
                        all.extend(&pts);
                        let cmds = c.poly(&all, false);
                        if c.in_path {
                            c.path.extend(cmds.into_iter().skip(1));
                        } else {
                            c.emit(cmds, false, true);
                        }
                        if let Some(l) = pts.last() {
                            c.state.pos = *l;
                        }
                    }
                    _ => {
                        // Béziers: PolyBezier (first point is a start) or PolyBezierTo.
                        let to = matches!(kind, 88 | 5);
                        let mut all = Vec::new();
                        if to {
                            all.push(c.state.pos);
                        }
                        all.extend(&pts);
                        if all.len() >= 4 {
                            let u: Vec<(f32, f32)> = all.iter().map(|q| c.unit(q.0, q.1)).collect();
                            let mut cmds = Vec::new();
                            if !(to && c.in_path) {
                                cmds.push(PathCmd::Move(u[0].0, u[0].1));
                            }
                            for s in u[1..].as_chunks::<3>().0 {
                                cmds.push(PathCmd::Cubic(s[0].0, s[0].1, s[1].0, s[1].1, s[2].0, s[2].1));
                            }
                            c.shape_or_path(cmds, false, true);
                            if let Some(l) = all.last() {
                                c.state.pos = *l;
                            }
                        }
                    }
                }
            }
            // PolyPolygon16 / PolyPolyline16 / PolyPolygon / PolyPolyline: bounds, nPolys, nPoints, counts[], points
            91 | 90 | 8 | 7 => {
                let wide = matches!(kind, 7 | 8);
                let polys = r.u32(p + 16).unwrap_or(0) as usize;
                let total = r.u32(p + 20).unwrap_or(0) as usize;
                let step = if wide { 8 } else { 4 };
                if polys > 10_000 || total > MAX_POINTS || p + 24 + polys * 4 + total * step > at + size {
                    at += size;
                    continue;
                }
                let mut q = p + 24 + polys * 4;
                let mut cmds = Vec::new();
                for k in 0..polys {
                    let n = r.u32(p + 24 + k * 4).unwrap_or(0) as usize;
                    let pts: Vec<(f32, f32)> = (0..n)
                        .filter_map(|i| if wide { pt32(q + i * 8) } else { pt16(q + i * 4) })
                        .collect();
                    q += n * step;
                    cmds.extend(c.poly(&pts, matches!(kind, 91 | 8)));
                }
                let fill = matches!(kind, 91 | 8);
                c.shape_or_path(cmds, fill, true);
            }
            59 => {
                c.in_path = true;
                c.path.clear();
            }
            60 => c.in_path = false,
            61 => {
                if c.in_path {
                    c.path.push(PathCmd::Close);
                }
            }
            62..=64 => {
                let cmds = std::mem::take(&mut c.path);
                c.in_path = false;
                c.emit(cmds, kind != 64, kind != 62);
            }
            // BitBlt / StretchBlt: pattern fill when there is no source bitmap.
            76 | 77 => {
                let dest = (r.i32(p + 16), r.i32(p + 20), r.i32(p + 24), r.i32(p + 28));
                let rop = r.u32(p + 32).unwrap_or(0);
                let off_bmi = r.u32(p + 84).unwrap_or(0) as usize;
                let cb_bmi = r.u32(p + 88).unwrap_or(0) as usize;
                let off_bits = r.u32(p + 92).unwrap_or(0) as usize;
                if let (Some(x), Some(y), Some(w), Some(h)) = dest {
                    let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
                    if cb_bmi == 0 {
                        pattern_fill(&mut c, rop, x, y, w, h);
                    } else if let Some(png) = dib_slice(b, at, size, off_bmi, off_bits).and_then(|d| dib_to_png(&d)) {
                        c.bitmap(png, x, y, x + w, y + h);
                    }
                }
            }
            81 => {
                // StretchDIBits: bounds(16), xDest, yDest, xSrc, ySrc, cxSrc, cySrc, offBmi, cbBmi, offBits, cbBits, usage, rop, cxDest, cyDest
                let (x, y) = (r.i32(p + 16).unwrap_or(0) as f32, r.i32(p + 20).unwrap_or(0) as f32);
                let off_bmi = r.u32(p + 40).unwrap_or(0) as usize;
                let off_bits = r.u32(p + 48).unwrap_or(0) as usize;
                let (w, h) = (r.i32(p + 64).unwrap_or(0) as f32, r.i32(p + 68).unwrap_or(0) as f32);
                if let Some(png) = dib_slice(b, at, size, off_bmi, off_bits).and_then(|d| dib_to_png(&d)) {
                    c.bitmap(png, x, y, x + w, y + h);
                }
            }
            84 | 83 => {
                // ExtTextOutW/A: bounds(16), graphicsMode, exScale, eyScale, EMRTEXT { ref(8), nChars, offString, options, rect(16), offDx }
                let refp = pt32(p + 28);
                let n = r.u32(p + 36).unwrap_or(0) as usize;
                let off = r.u32(p + 40).unwrap_or(0) as usize;
                if let Some(rp) = refp {
                    let s = if kind == 84 {
                        utf16(b, at + off, n)
                    } else {
                        latin1(b, at + off, n)
                    };
                    if let Some(s) = s {
                        c.text(rp.0, rp.1, &s);
                    }
                }
            }
            _ => {}
        }
        at += size;
    }
    Ok((c.out, c.bitmaps))
}

/// Blits without a source bitmap only paint for the fill raster
/// operations; others (no-op, invert, destination tricks) are skipped
/// rather than painting over everything.
fn pattern_fill(c: &mut Canvas, rop: u32, x: f32, y: f32, w: f32, h: f32) {
    let brush = match rop {
        0x00F0_0021 => c.state.brush, // PATCOPY
        0x00FF_0062 => (Rgb::WHITE, false),
        0x0000_0042 => (Rgb::BLACK, false),
        _ => return,
    };
    let (saved_pen, saved_brush) = (c.state.pen, c.state.brush);
    c.state.pen.2 = true;
    c.state.brush = brush;
    c.rect(x, y, x + w, y + h);
    c.state.pen = saved_pen;
    c.state.brush = saved_brush;
}

/// The packed DIB (header + bits) of a bitmap record, as one buffer.
fn dib_slice(b: &[u8], at: usize, size: usize, off_bmi: usize, off_bits: usize) -> Option<Vec<u8>> {
    if off_bmi == 0 || off_bits == 0 || off_bmi >= size || off_bits >= size || off_bits < off_bmi {
        return None;
    }
    let header = b.get(at + off_bmi..at + off_bits)?;
    let bits = b.get(at + off_bits..at + size)?;
    let mut v = Vec::with_capacity(header.len() + bits.len());
    v.extend_from_slice(header);
    v.extend_from_slice(bits);
    Some(v)
}

fn mul(a: &[f32], b: &[f32]) -> [f32; 6] {
    // Row-vector affine matrices [m11 m12 m21 m22 dx dy]: a then b.
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn wmf(b: &[u8], target: Rect) -> Result<(Vec<Element>, Bitmaps), String> {
    let r = Reader { b };
    let placeable = r.u32(0) == Some(0x9AC6_CDD7);
    let mut at = if placeable { 22 } else { 0 };
    // META_HEADER: type(2), headerSize(2, in words)
    let header_words = r.u16(at + 2).ok_or("malformed WMF")? as usize;
    if header_words != 9 {
        return Err("not a WMF file".into());
    }
    let bbox = if placeable {
        let (l, t, rr, bb) = (
            r.i16(6).unwrap_or(0) as f32,
            r.i16(8).unwrap_or(0) as f32,
            r.i16(10).unwrap_or(0) as f32,
            r.i16(12).unwrap_or(0) as f32,
        );
        Some((l, t, rr, bb))
    } else {
        None
    };
    at += 18;
    let mut c = Canvas::new(target, bbox.unwrap_or((0.0, 0.0, 1.0, 1.0)), true);
    if let Some((l, t, r, b)) = bbox {
        c.state.win_org = (l, t);
        c.state.win_ext = (r - l, b - t);
    }
    let mut window_set = false;
    for _ in 0..MAX_RECORDS {
        let (Some(words), Some(func)) = (r.u32(at), r.u16(at + 4)) else {
            break;
        };
        let size = words as usize * 2;
        if size < 6 || at + size > b.len() {
            break;
        }
        let p = at + 6;
        let s16 = |i: usize| r.i16(p + i * 2).unwrap_or(0) as f32;
        match func {
            0x0000 => break,
            // WMF coordinates: params are stored in reverse order.
            // The window (origin + extent, possibly negative = flipped) is
            // the logical box that maps onto the picture frame.
            0x020B => {
                c.state.win_org = (s16(1), s16(0));
                window_set = true;
                let (ox, oy) = c.state.win_org;
                let (ex, ey) = c.state.win_ext;
                c.bounds = (ox, oy, ox + ex, oy + ey);
            }
            0x020C => {
                c.state.win_ext = (s16(1), s16(0));
                window_set = true;
                let (ox, oy) = c.state.win_org;
                let (ex, ey) = c.state.win_ext;
                c.bounds = (ox, oy, ox + ex, oy + ey);
            }
            0x0106 => c.state.even_odd = s16(0) as i32 == 1,
            0x012E => c.state.text_align = s16(0) as u32,
            0x0209 => c.state.text_color = r.color(p).unwrap_or(Rgb::BLACK),
            0x001E => {
                if c.stack.len() < 1000 {
                    c.stack.push(c.state);
                }
            }
            0x0127 => {
                if let Some(s) = c.stack.pop() {
                    c.state = s;
                }
            }
            0x0214 => c.state.pos = (s16(1), s16(0)),
            0x0213 => {
                let to = (s16(1), s16(0));
                let cmds = c.poly(&[c.state.pos, to], false);
                c.emit(cmds, false, true);
                c.state.pos = to;
            }
            0x02FA => {
                // CreatePenIndirect: style, width(x, y), color
                let style = s16(0) as i32 as u32;
                c.create(
                    None,
                    Obj::Pen {
                        color: r.color(p + 6).unwrap_or(Rgb::BLACK),
                        width: s16(1).abs(),
                        none: style & 0xF == 5,
                    },
                );
            }
            0x02FC => {
                let style = r.u16(p).unwrap_or(0);
                c.create(
                    None,
                    Obj::Brush {
                        color: r.color(p + 2).unwrap_or(Rgb::WHITE),
                        none: style == 1,
                    },
                );
            }
            0x0142 | 0x01F9 | 0x06FF | 0x00F7 => c.create(
                None,
                Obj::Brush {
                    color: Rgb(128, 128, 128),
                    none: false,
                },
            ),
            0x02FB => {
                // CreateFontIndirect: height, width, esc, orient, weight, italic(1), ..., face at 18
                let height = s16(0);
                let weight = s16(4);
                let italic = b.get(p + 10).copied().unwrap_or(0) != 0;
                let face = latin1(b, p + 18, 32).unwrap_or_default();
                let face = face.split('\0').next().unwrap_or("").to_string();
                let fi = c.face(face);
                c.create(
                    None,
                    Obj::Font {
                        height,
                        bold: weight >= 600.0,
                        italic,
                        face: fi,
                    },
                );
            }
            0x012D => c.select(r.u16(p).unwrap_or(0) as usize),
            0x01F0 => {
                let i = r.u16(p).unwrap_or(u16::MAX) as usize;
                if let Some(o) = c.objects.get_mut(i) {
                    *o = None;
                }
            }
            0x041B | 0x0418 | 0x061C => {
                let (bottom, right, top, left) = (s16(0), s16(1), s16(2), s16(3));
                let (bottom, right, top, left) = if func == 0x061C {
                    (s16(2), s16(3), s16(4), s16(5))
                } else {
                    (bottom, right, top, left)
                };
                if func == 0x0418 {
                    c.ellipse(left, top, right, bottom);
                } else {
                    c.rect(left, top, right, bottom);
                }
            }
            0x061D => {
                // PatBlt: rop(4), h, w, y, x -> fill with the brush
                let rop = r.u32(p).unwrap_or(0);
                let (h, w, y, x) = (s16(2), s16(3), s16(4), s16(5));
                pattern_fill(&mut c, rop, x, y, w, h);
            }
            0x0324 | 0x0325 => {
                let n = r.u16(p).unwrap_or(0) as usize;
                if p + 2 + n * 4 <= at + size {
                    let pts: Vec<(f32, f32)> = (0..n)
                        .map(|i| (r.i16(p + 2 + i * 4).unwrap_or(0) as f32, r.i16(p + 4 + i * 4).unwrap_or(0) as f32))
                        .collect();
                    let close = func == 0x0324;
                    let cmds = c.poly(&pts, close);
                    c.emit(cmds, close, true);
                }
            }
            0x0538 => {
                let polys = r.u16(p).unwrap_or(0) as usize;
                let mut q = p + 2 + polys * 2;
                let mut cmds = Vec::new();
                for k in 0..polys.min(10_000) {
                    let n = r.u16(p + 2 + k * 2).unwrap_or(0) as usize;
                    if q + n * 4 > at + size {
                        break;
                    }
                    let pts: Vec<(f32, f32)> = (0..n)
                        .map(|i| (r.i16(q + i * 4).unwrap_or(0) as f32, r.i16(q + 2 + i * 4).unwrap_or(0) as f32))
                        .collect();
                    q += n * 4;
                    cmds.extend(c.poly(&pts, true));
                }
                c.emit(cmds, true, true);
            }
            0x0521 => {
                // TextOut: count, string (padded), y, x
                let n = r.u16(p).unwrap_or(0) as usize;
                let padded = n.div_ceil(2) * 2;
                let (y, x) = (r.i16(p + 2 + padded).unwrap_or(0) as f32, r.i16(p + 4 + padded).unwrap_or(0) as f32);
                if let Some(s) = latin1(b, p + 2, n) {
                    c.text(x, y, &s);
                }
            }
            0x0A32 => {
                // ExtTextOut: y, x, count, options, [rect], string
                let (y, x, n, opts) = (s16(0), s16(1), r.u16(p + 4).unwrap_or(0) as usize, r.u16(p + 6).unwrap_or(0));
                let off = if opts & 0x0006 != 0 { 16 } else { 8 };
                if let Some(s) = latin1(b, p + off, n) {
                    c.text(x, y, &s);
                }
            }
            0x0B41 | 0x0F43 | 0x0940 => {
                // DIBStretchBlt / StretchDIB / DIBBitBlt (reverse order params, DIB follows)
                let (dib_at, dest) = match func {
                    0x0F43 => (p + 22, (s16(10), s16(9), s16(8), s16(7))), // x, y, w, h
                    0x0B41 => (p + 20, (s16(9), s16(8), s16(7), s16(6))),
                    _ => (p + 16, (s16(7), s16(6), s16(5), s16(4))),
                };
                let (x, y, w, h) = dest;
                if let Some(png) = b.get(dib_at..at + size).and_then(dib_to_png) {
                    c.bitmap(png, x, y, x + w, y + h);
                }
            }
            _ => {}
        }
        at += size;
    }
    let _ = window_set;
    Ok((c.out, c.bitmaps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_rejected_or_empty() {
        let target = Rect {
            x: 0.0,
            y: 0.0,
            w: 914400.0,
            h: 914400.0,
        };
        for len in [0usize, 10, 100, 1000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
            let _ = render(&data, target);
        }
    }

    #[test]
    fn tiny_dib_converts() {
        // 2x1, 24 bpp, bottom-up
        let mut dib = vec![0u8; 40];
        dib[0] = 40;
        dib[4] = 2;
        dib[8] = 1;
        dib[12] = 1;
        dib[14] = 24;
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // red, green, padding
        let png = dib_to_png(&dib).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }
}
