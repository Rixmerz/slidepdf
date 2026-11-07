//! IR -> PDF backend.
//!
//! Size choices, in order of impact:
//! - Text uses the base-14 fonts (Helvetica, Times, Courier) with WinAnsi
//!   encoding: zero font bytes in the file. Line breaking uses their real
//!   advance widths so wrapping matches what the viewer draws.
//! - JPEGs are passed through; oversized images are downsampled to the
//!   resolution they are displayed at (see `images`).
//! - Images are deduplicated by content within a document.
//! - Content streams are Flate level 9; the xref is a stream.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::rc::Rc;

use pdf_writer::types::{FunctionShadingType, Predictor};
use pdf_writer::{Chunk, Content, Filter, Name, Rect as PRect, Ref, Settings, Str};

use crate::fonts::{face_style, run_weight, subset_tag, FontFile, Usage};
use crate::images::{self, ColorSpace, Prepared, Target};
use crate::ir::*;
use crate::metrics;

#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// Images are downsampled to this resolution at their displayed size.
    /// `None` keeps original pixels.
    pub image_dpi: Option<f32>,
    /// Quality for re-encoded JPEGs (1-100).
    pub jpeg_quality: u8,
    /// Fonts to embed (subset). A font is used for runs whose typeface
    /// matches its family, and as a fallback for characters the base-14
    /// fonts cannot encode.
    pub fonts: Vec<FontFile>,
    /// Upper bound for prepared images kept for reuse across slides.
    pub cache_bytes: usize,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            image_dpi: Some(150.0),
            jpeg_quality: 75,
            fonts: Vec::new(),
            cache_bytes: 16 << 20,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FontKey(Family, bool, bool);

/// A font as used on a page: base-14 or the n-th embedded font.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum FontId {
    Base(FontKey),
    Emb(usize),
}

impl FontId {
    fn res_name(self) -> String {
        match self {
            FontId::Base(k) => font_res_name(k),
            FontId::Emb(i) => format!("E{i}"),
        }
    }
}

/// One positioned glyph: the bytes to show and its advance in 1/1000 em.
#[derive(Clone, Copy)]
struct Glyph {
    font: FontId,
    code: [u8; 2],
    len: u8,
    /// Advance used for layout (the deck's font metrics when known).
    adv: f32,
    /// Advance of the glyph as drawn; differs from `adv` when a base-14
    /// font stands in for a font we have metrics for (e.g. Calibri).
    nat: f32,
    space: bool,
    run: usize,
}

/// Chooses the font and encoding for each character.
struct Fonts<'f, 'u> {
    files: &'f [FontFile],
    usage: &'u mut [Usage],
    /// Characters no available font could draw on the current slide.
    missing: Vec<char>,
    /// Typefaces drawn with a base-14 font instead, with that font's family.
    substituted: Vec<(String, &'static str)>,
}

/// Typefaces the base-14 fonts stand in for without visible loss: the
/// same design (Arial/Helvetica, Times, Courier) or one whose metrics we
/// ship so lines still break in the same place (Calibri).
const NATIVE_FAMILIES: [&str; 9] = [
    "arial",
    "helvetica",
    "helvetica neue",
    "times",
    "times new roman",
    "courier",
    "courier new",
    "calibri",
    "",
];

impl Fonts<'_, '_> {
    /// The registered face for a run: same family (by the name the deck
    /// uses), same slant first, then the nearest weight; on a tie a bold
    /// request takes the heavier face and a light one the lighter, as CSS
    /// does.
    fn matching(&self, r: &Run) -> Option<usize> {
        if r.face.is_empty() || self.files.is_empty() {
            return None;
        }
        let (family, _, named_italic) = face_style(&r.face);
        let full = r.face.to_lowercase();
        let weight = run_weight(&r.face, r.bold) as i32;
        let italic = r.italic || named_italic;
        (0..self.files.len())
            .filter(|&i| self.files[i].family == family || self.files[i].family == full)
            .min_by_key(|&i| {
                let f = &self.files[i];
                let lighter = (f.weight as i32) < weight;
                (f.italic != italic, (f.weight as i32 - weight).abs(), lighter == (weight >= 400))
            })
    }

    fn note_substitution(&mut self, r: &Run, key: FontKey) {
        if self.substituted.iter().any(|(f, _)| *f == r.face) {
            return;
        }
        let (family, ..) = face_style(&r.face);
        if !NATIVE_FAMILIES.contains(&family.as_str()) {
            // Named by family only: the same typeface is drawn bold and regular.
            self.substituted.push((r.face.clone(), FontKey(key.0, false, false).base_name()));
        }
    }

    fn glyph(&mut self, r: &Run, run: usize, c: char) -> Glyph {
        // NBSP is drawn as a space but is not a break opportunity.
        let space = c == ' ' || c == '\t';
        let emb = |this: &mut Self, i: usize| {
            this.usage[i].glyph(&this.files[i], c).map(|(gid, adv)| Glyph {
                font: FontId::Emb(i),
                code: gid.to_be_bytes(),
                len: 2,
                adv,
                nat: adv,
                space,
                run,
            })
        };
        let matched = self.matching(r);
        if let Some(i) = matched {
            if let Some(g) = emb(self, i) {
                return g;
            }
        }
        let key = FontKey::of(r);
        // A registered font lacking one character is not a missing font.
        if matched.is_none() && !space {
            self.note_substitution(r, key);
        }
        if let Some(b) = winansi(c) {
            let nat = char_width(key, b) * 1000.0;
            let adv = metric_width(&r.face, key, b).map(|w| w * 1000.0).unwrap_or(nat);
            return Glyph {
                font: FontId::Base(key),
                code: [b, 0],
                len: 1,
                adv,
                nat,
                space,
                run,
            };
        }
        for i in 0..self.files.len() {
            if let Some(g) = emb(self, i) {
                return g;
            }
        }
        if !self.missing.contains(&c) {
            self.missing.push(c);
        }
        Glyph {
            font: FontId::Base(key),
            code: [b'?', 0],
            len: 1,
            adv: char_width(key, b'?') * 1000.0,
            nat: char_width(key, b'?') * 1000.0,
            space,
            run,
        }
    }
}

impl FontKey {
    fn base_name(self) -> &'static str {
        match self {
            FontKey(Family::Sans, false, false) => "Helvetica",
            FontKey(Family::Sans, true, false) => "Helvetica-Bold",
            FontKey(Family::Sans, false, true) => "Helvetica-Oblique",
            FontKey(Family::Sans, true, true) => "Helvetica-BoldOblique",
            FontKey(Family::Serif, false, false) => "Times-Roman",
            FontKey(Family::Serif, true, false) => "Times-Bold",
            FontKey(Family::Serif, false, true) => "Times-Italic",
            FontKey(Family::Serif, true, true) => "Times-BoldItalic",
            FontKey(Family::Mono, false, false) => "Courier",
            FontKey(Family::Mono, true, false) => "Courier-Bold",
            FontKey(Family::Mono, false, true) => "Courier-Oblique",
            FontKey(Family::Mono, true, true) => "Courier-BoldOblique",
        }
    }

    fn widths(self) -> Option<&'static [u16; 224]> {
        Some(match self {
            FontKey(Family::Sans, false, false) => &metrics::HELVETICA,
            FontKey(Family::Sans, true, false) => &metrics::HELVETICA_BOLD,
            FontKey(Family::Sans, false, true) => &metrics::HELVETICA_OBLIQUE,
            FontKey(Family::Sans, true, true) => &metrics::HELVETICA_BOLD_OBLIQUE,
            FontKey(Family::Serif, false, false) => &metrics::TIMES_ROMAN,
            FontKey(Family::Serif, true, false) => &metrics::TIMES_BOLD,
            FontKey(Family::Serif, false, true) => &metrics::TIMES_ITALIC,
            FontKey(Family::Serif, true, true) => &metrics::TIMES_BOLD_ITALIC,
            FontKey(Family::Mono, ..) => return None,
        })
    }

    /// "Montserrat Black" is a bold face even when the run is not marked
    /// bold: the weight named in the typeface counts too.
    fn of(r: &Run) -> Self {
        let (_, weight, named_italic) = face_style(&r.face);
        let bold = r.bold || weight.is_some_and(|w| w >= 600);
        FontKey(r.family, bold, r.italic || named_italic)
    }
}

/// Maps a char to its WinAnsi (cp1252) byte.
pub fn winansi(c: char) -> Option<u8> {
    let u = c as u32;
    if (0x20..0x7F).contains(&u) || (0xA0..=0xFF).contains(&u) {
        return Some(u as u8);
    }
    Some(match c {
        '€' => 0x80,
        '‚' => 0x82,
        'ƒ' => 0x83,
        '„' => 0x84,
        '…' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '‹' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' | '▪' | '■' | '●' | '◦' | '○' | '\u{F0A7}' | '\u{F0B7}' | '\u{F0D8}' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '›' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        '\t' => b' ',
        '\u{2010}' | '\u{2011}' | '\u{2212}' => b'-',
        '\u{2002}'..='\u{200A}' | '\u{202F}' => b' ',
        _ => return None,
    })
}

/// Width from the real typeface's metrics when we ship them, so lines
/// break where PowerPoint breaks them even though Helvetica is drawn.
fn metric_width(face: &str, key: FontKey, code: u8) -> Option<f32> {
    if code < 32 || key.0 != Family::Sans || !face.to_ascii_lowercase().starts_with("calibri") {
        return None;
    }
    let t = match (key.1, key.2) {
        (false, false) => &metrics::CALIBRI,
        (true, false) => &metrics::CALIBRI_BOLD,
        (false, true) => &metrics::CALIBRI_ITALIC,
        (true, true) => &metrics::CALIBRI_BOLD_ITALIC,
    };
    Some(t[(code - 32) as usize] as f32 / 1000.0)
}

fn char_width(font: FontKey, code: u8) -> f32 {
    match font.widths() {
        Some(t) if code >= 32 => t[(code - 32) as usize] as f32 / 1000.0,
        Some(_) => 0.0,
        None => 0.6,
    }
}

/// Where the renderer gets image bytes from: called with a package path,
/// returns the bytes. Images are fetched only when drawn and dropped after
/// being prepared, so the deck's media never sits in memory all at once.
pub type MediaSource<'d> = Box<dyn FnMut(&str) -> Option<Vec<u8>> + 'd>;

/// Renders slides of one deck, streaming the PDF as it goes.
///
/// Memory stays bounded: one slide's content, the image being prepared and
/// a cache of prepared images capped at `PdfOptions::cache_bytes`. Identical
/// images give byte-identical streams in every output, which lets
/// [`crate::fragments::assemble`] deduplicate them again.
pub struct Renderer<'d> {
    deck: &'d Deck,
    opts: PdfOptions,
    media: MediaSource<'d>,
    max_px: HashMap<String, (u32, u32)>,
    cache: HashMap<String, Option<Rc<Prepared>>>,
    cache_order: VecDeque<String>,
    cache_used: usize,
    /// Slide being drawn, so image problems are reported on it.
    current_slide: Option<usize>,
    pub warnings: Vec<Warning>,
}

/// One PDF being written. Objects go to the writer as soon as they are
/// complete; only their offsets are kept for the final cross-reference.
pub struct DocWriter<'w> {
    w: &'w mut dyn Write,
    pos: u64,
    offsets: Vec<(i32, u64)>,
    next: i32,
    catalog: Ref,
    tree: Ref,
    fonts: BTreeMap<FontId, Ref>,
    images: HashMap<u64, (Ref, String)>,
    shadings: HashMap<u64, Ref>,
    gstates: HashMap<u32, Ref>,
    pages: Vec<Ref>,
    usage: Vec<Usage>,
    error: Option<std::io::Error>,
}

const COMPACT: Settings = Settings { pretty: false };

impl<'w> DocWriter<'w> {
    fn new(w: &'w mut dyn Write, fonts: usize) -> Self {
        let mut d = DocWriter {
            w,
            pos: 0,
            offsets: Vec::new(),
            next: 1,
            catalog: Ref::new(1),
            tree: Ref::new(2),
            fonts: BTreeMap::new(),
            images: HashMap::new(),
            shadings: HashMap::new(),
            gstates: HashMap::new(),
            pages: Vec::new(),
            usage: (0..fonts).map(|_| Usage::default()).collect(),
            error: None,
        };
        d.catalog = d.alloc();
        d.tree = d.alloc();
        d.raw(b"%PDF-1.7\n%\x80\x80\x80\x80\n");
        d
    }

    fn alloc(&mut self) -> Ref {
        let r = Ref::new(self.next);
        self.next += 1;
        r
    }

    fn raw(&mut self, b: &[u8]) {
        if self.error.is_none() {
            if let Err(e) = self.w.write_all(b) {
                self.error = Some(e);
            }
        }
        self.pos += b.len() as u64;
    }

    /// Writes exactly one indirect object built by `f`.
    fn obj(&mut self, id: Ref, f: impl FnOnce(&mut Chunk)) {
        let mut c = Chunk::with_settings(COMPACT);
        f(&mut c);
        debug_assert!(c.refs().len() == 1, "one object per chunk");
        self.offsets.push((id.get(), self.pos));
        self.raw(c.as_bytes());
    }

    /// Writes page tree, catalog, fonts and the cross-reference stream.
    /// Fonts that fail to embed are left out and returned as warnings, so a
    /// bad font file degrades the output instead of failing it.
    fn finish(mut self, files: &[FontFile]) -> Result<Vec<String>, String> {
        let fonts: Vec<(FontId, Ref)> = self.fonts.iter().map(|(k, v)| (*k, *v)).collect();
        let mut warnings = Vec::new();
        for (key, id) in fonts {
            match key {
                FontId::Base(k) => self.obj(id, |c| {
                    c.type1_font(id)
                        .base_font(Name(k.base_name().as_bytes()))
                        .encoding_predefined(Name(b"WinAnsiEncoding"));
                }),
                FontId::Emb(i) => {
                    let usage = std::mem::take(&mut self.usage[i]);
                    if let Err(e) = write_embedded(&mut self, id, &files[i], &usage) {
                        warnings.push(e);
                    }
                }
            }
        }
        let (tree, catalog) = (self.tree, self.catalog);
        let pages = std::mem::take(&mut self.pages);
        let n = pages.len() as i32;
        self.obj(tree, |c| {
            c.pages(tree).kids(pages).count(n);
        });
        self.obj(catalog, |c| {
            c.indirect(catalog)
                .dict()
                .pair(Name(b"Type"), Name(b"Catalog"))
                .pair(Name(b"Pages"), tree);
        });

        // Cross-reference stream: type 1 entries with 4-byte offsets.
        let xref = self.alloc();
        let start = self.pos;
        self.offsets.push((xref.get(), start));
        let size = self.next as usize;
        let mut table = vec![0u8; size * 6];
        table[5] = 0xFF; // object 0: free, generation 65535 (truncated to 1 byte)
        for &(id, off) in &self.offsets {
            let e = &mut table[id as usize * 6..id as usize * 6 + 6];
            e[0] = 1;
            e[1..5].copy_from_slice(&(off as u32).to_be_bytes());
        }
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&table, 9);
        let head = format!(
            "{} 0 obj\n<</Type/XRef/Size {size}/W[1 4 1]/Root {} 0 R/Filter/FlateDecode/Length {}>>\nstream\n",
            xref.get(),
            catalog.get(),
            packed.len()
        );
        self.raw(head.as_bytes());
        self.raw(&packed);
        self.raw(format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes());
        if start > u32::MAX as u64 {
            return Err("PDF larger than 4 GB is not supported".into());
        }
        if let Some(e) = self.error {
            return Err(e.to_string());
        }
        Ok(warnings)
    }
}

/// An open document: add slides one by one, then call [`Renderer::finish`].
pub struct Document<'w> {
    doc: DocWriter<'w>,
}

impl<'d> Renderer<'d> {
    pub fn new(deck: &'d Deck, media: MediaSource<'d>, opts: PdfOptions) -> Self {
        // Largest displayed size of each image across the whole deck, so the
        // downsampled pixels are the same in every output.
        let mut max_px: HashMap<String, (u32, u32)> = HashMap::new();
        if let Some(dpi) = opts.image_dpi {
            let px = |emu: f32| ((emu / 914400.0) * dpi).ceil().max(1.0) as u32;
            let mut note = |m: &str, w: f32, h: f32| {
                let e = max_px.entry(m.to_string()).or_insert((0, 0));
                *e = (e.0.max(px(w)), e.1.max(px(h)));
            };
            for s in &deck.slides {
                if let Some(Fill::Image(m)) = &s.background {
                    note(m, deck.width, deck.height);
                }
                for e in &s.elements {
                    match e {
                        Element::Picture(p) => {
                            let r = uncropped(&p.rect, p.crop);
                            note(&p.media, r.w, r.h)
                        }
                        Element::Shape(Shape {
                            fill: Some(Fill::Image(m)),
                            rect,
                            ..
                        }) => note(m, rect.w, rect.h),
                        _ => {}
                    }
                }
            }
        }
        Self {
            deck,
            opts,
            media,
            max_px,
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            cache_used: 0,
            current_slide: None,
            warnings: Vec::new(),
        }
    }

    fn image(&mut self, media: &str) -> Option<Rc<Prepared>> {
        if let Some(p) = self.cache.get(media) {
            return p.clone();
        }
        let target = Target {
            max_px: self.max_px.get(media).copied(),
            jpeg_quality: self.opts.jpeg_quality,
        };
        let prepared = match (self.media)(media).map(|b| images::prepare(&b, target)) {
            Some(Ok(p)) => Some(Rc::new(p)),
            Some(Err(e)) => {
                self.warnings.push(Warning {
                    slide: self.current_slide,
                    message: format!("{media}: {e}, skipped"),
                });
                None
            }
            None => {
                self.warnings.push(Warning {
                    slide: self.current_slide,
                    message: format!("{media}: missing or too large, skipped"),
                });
                None
            }
        };
        // Bounded cache: evict oldest entries until the new one fits. An
        // image bigger than the whole budget is used once and not kept.
        let size = prepared
            .as_ref()
            .map(|p| p.data.len() + p.smask.as_ref().map_or(0, |m| m.len()))
            .unwrap_or(0);
        if size <= self.opts.cache_bytes {
            while self.cache_used + size > self.opts.cache_bytes {
                let Some(old) = self.cache_order.pop_front() else { break };
                if let Some(Some(p)) = self.cache.remove(&old) {
                    self.cache_used -= p.data.len() + p.smask.as_ref().map_or(0, |m| m.len());
                }
            }
            self.cache.insert(media.to_string(), prepared.clone());
            self.cache_order.push_back(media.to_string());
            self.cache_used += size;
        }
        prepared
    }

    /// Starts a PDF written to `w`.
    pub fn begin<'w>(&self, w: &'w mut dyn Write) -> Document<'w> {
        Document {
            doc: DocWriter::new(w, self.opts.fonts.len()),
        }
    }

    /// Finishes a document started with [`Renderer::begin`].
    pub fn finish(&mut self, d: Document) -> Result<(), String> {
        for message in d.doc.finish(&self.opts.fonts)? {
            let w = Warning { slide: None, message };
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        }
        Ok(())
    }

    /// Renders the given slides (0-based indices) into one in-memory PDF.
    pub fn render(&mut self, indices: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut d = self.begin(&mut out);
            for &i in indices {
                self.add_slide(&mut d, i);
            }
            if let Err(e) = self.finish(d) {
                self.warnings.push(Warning { slide: None, message: e });
            }
        }
        out
    }

    /// Appends slide `i` (0-based) as a page.
    pub fn add_slide(&mut self, d: &mut Document, i: usize) {
        let deck = self.deck;
        let Some(slide) = deck.slides.get(i) else { return };
        self.current_slide = Some(slide.number);
        let doc = &mut d.doc;
        let (pw, ph) = (deck.width / EMU_PER_PT, deck.height / EMU_PER_PT);
        let page_id = doc.alloc();
        let content_id = doc.alloc();
        doc.pages.push(page_id);
        let mut page = PageCtx {
            c: Content::new(),
            ph,
            fonts: Vec::new(),
            images: Vec::new(),
            shadings: Vec::new(),
            gstates: Vec::new(),
        };

        if let Some(bg) = &slide.background {
            let full = Rect {
                x: 0.0,
                y: 0.0,
                w: deck.width,
                h: deck.height,
            };
            self.draw_fill(&mut page, bg, &full, doc);
        }
        // Font files are moved out for the slide to keep borrows simple.
        let font_files = std::mem::take(&mut self.opts.fonts);
        let mut missing = Vec::new();
        let mut substituted = Vec::new();
        for e in &slide.elements {
            match e {
                Element::Picture(p) => {
                    page.c.save_state();
                    rotate(&mut page, &p.rect, p.rotation);
                    if p.crop != [0.0; 4] {
                        // Clip to the frame, then draw the whole image larger.
                        let (x, y, w, h) = page.bbox(&p.rect);
                        page.c.rect(x, y, w, h);
                        page.c.clip_nonzero();
                        page.c.end_path();
                    }
                    let full = uncropped(&p.rect, p.crop);
                    self.draw_image(&mut page, &p.media, &full, doc, (p.flip_h, p.flip_v));
                    page.c.restore_state();
                }
                Element::Shape(s) => {
                    page.c.save_state();
                    rotate(&mut page, &s.rect, s.rotation);
                    if let Some(Fill::Image(m)) = &s.fill {
                        self.draw_image(&mut page, m, &s.rect, doc, (false, false));
                    }
                    let sh = match &s.fill {
                        Some(Fill::Gradient(g)) => Some(self.shading(doc, &mut page, g)),
                        _ => None,
                    };
                    if let Some(Fill::Translucent(_, a)) = &s.fill {
                        // Opacity applies to the fill only, so text and outline stay opaque.
                        let gs = self.alpha_state(doc, &mut page, *a);
                        page.c.save_state();
                        page.c.set_parameters(Name(gs.as_bytes()));
                        draw_geometry(&mut page, &Shape { stroke: None, ..s.clone() }, None);
                        page.c.restore_state();
                        draw_geometry(&mut page, &Shape { fill: None, ..s.clone() }, None);
                    } else {
                        draw_geometry(&mut page, s, sh.as_deref());
                    }
                    if let Some(t) = &s.text {
                        let mut fset = Fonts {
                            files: &font_files,
                            usage: &mut doc.usage,
                            missing: std::mem::take(&mut missing),
                            substituted: std::mem::take(&mut substituted),
                        };
                        let r = *s.text_rect.as_ref().unwrap_or(&s.rect);
                        if t.rotation.abs() > 0.01 {
                            // Lay the text out in the box turned sideways, then rotate it into place.
                            page.c.save_state();
                            rotate(&mut page, &r, t.rotation);
                            // Quarter turns swap the box sides; other angles keep them.
                            let quarter = ((t.rotation / 90.0).round() as i32).rem_euclid(2) == 1;
                            let turned = if quarter {
                                Rect {
                                    x: r.x + (r.w - r.h) / 2.0,
                                    y: r.y + (r.h - r.w) / 2.0,
                                    w: r.h,
                                    h: r.w,
                                }
                            } else {
                                r
                            };
                            draw_text(&mut page, &turned, t, &mut fset);
                            page.c.restore_state();
                        } else {
                            draw_text(&mut page, &r, t, &mut fset);
                        }
                        missing = fset.missing;
                        substituted = fset.substituted;
                    }
                    page.c.restore_state();
                }
            }
        }
        self.opts.fonts = font_files;

        for (face, base) in substituted {
            // Deck-wide, once per typeface: the same font is usually on every slide.
            let w = Warning {
                slide: None,
                message: format!(
                    "font '{face}' is not available and was drawn with {base}; line breaks may differ (register the font file to embed it)"
                ),
            };
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        }
        if !missing.is_empty() {
            let chars: String = missing.into_iter().collect();
            let w = Warning {
                slide: Some(slide.number),
                message: format!("characters drawn as '?' (not in the built-in fonts; register a font that has them): {chars}"),
            };
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        }
        let PageCtx {
            c,
            fonts: used_fonts,
            images: used_images,
            shadings: used_shadings,
            gstates: used_gstates,
            ..
        } = page;
        let raw = c.finish();
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 9);
        drop(raw);
        doc.obj(content_id, |ch| {
            ch.stream(content_id, &packed).filter(Filter::FlateDecode);
        });
        let font_refs: Vec<(FontId, Ref)> = used_fonts
            .iter()
            .map(|k| {
                let next = Ref::new(doc.next);
                let id = *doc.fonts.entry(*k).or_insert(next);
                if id == next {
                    doc.next += 1;
                }
                (*k, id)
            })
            .collect();
        let tree = doc.tree;
        doc.obj(page_id, |ch| {
            let mut pg = ch.page(page_id);
            pg.media_box(PRect::new(0.0, 0.0, pw, ph));
            pg.parent(tree);
            pg.contents(content_id);
            let mut res = pg.resources();
            {
                let mut f = res.fonts();
                for (key, id) in &font_refs {
                    f.pair(Name(key.res_name().as_bytes()), *id);
                }
            }
            {
                let mut x = res.x_objects();
                for (name, id) in &used_images {
                    x.pair(Name(name.as_bytes()), *id);
                }
            }
            if !used_gstates.is_empty() {
                let mut g = res.ext_g_states();
                for (name, id) in &used_gstates {
                    g.pair(Name(name.as_bytes()), *id);
                }
            }
            if !used_shadings.is_empty() {
                let mut sh = res.shadings();
                for (name, id) in &used_shadings {
                    sh.pair(Name(name.as_bytes()), *id);
                }
            }
        });
    }

    fn draw_fill(&mut self, page: &mut PageCtx, fill: &Fill, r: &Rect, doc: &mut DocWriter) {
        match fill {
            Fill::Solid(c) => {
                let (x, y, w, h) = page.bbox(r);
                let (cr, cg, cb) = c.floats();
                page.c.set_fill_rgb(cr, cg, cb);
                page.c.rect(x, y, w, h);
                page.c.fill_nonzero();
            }
            Fill::Translucent(c, a) => {
                let gs = self.alpha_state(doc, page, *a);
                let (x, y, w, h) = page.bbox(r);
                let (cr, cg, cb) = c.floats();
                page.c.save_state();
                page.c.set_parameters(Name(gs.as_bytes()));
                page.c.set_fill_rgb(cr, cg, cb);
                page.c.rect(x, y, w, h);
                page.c.fill_nonzero();
                page.c.restore_state();
            }
            Fill::Image(m) => self.draw_image(page, m, r, doc, (false, false)),
            Fill::Gradient(g) => {
                let name = self.shading(doc, page, g);
                let shape = Shape {
                    rect: *r,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                    geometry: Geometry::Rect,
                    fill: Some(fill.clone()),
                    stroke: None,
                    text: None,
                    is_title: false,
                    text_rect: None,
                };
                draw_geometry(page, &shape, Some(&name));
            }
        }
    }

    /// Graphics state with a fill opacity, written once per document.
    fn alpha_state(&mut self, doc: &mut DocWriter, page: &mut PageCtx, alpha: f32) -> String {
        let key = (alpha * 1000.0).round() as u32;
        let id = match doc.gstates.get(&key) {
            Some(id) => *id,
            None => {
                let id = doc.alloc();
                doc.obj(id, |c| {
                    c.ext_graphics(id).non_stroking_alpha(key as f32 / 1000.0);
                });
                doc.gstates.insert(key, id);
                id
            }
        };
        let name = format!("G{key}");
        if !page.gstates.iter().any(|(n, _)| *n == name) {
            page.gstates.push((name.clone(), id));
        }
        name
    }

    /// Writes (once per document) the shading for a gradient and returns
    /// its page resource name. Coordinates are in the unit box, y up.
    fn shading(&mut self, doc: &mut DocWriter, page: &mut PageCtx, g: &Gradient) -> String {
        let key = fnv1a(format!("{g:?}").as_bytes());
        let id = match doc.shadings.get(&key) {
            Some(id) => *id,
            None => {
                let mut stops = g.stops.clone();
                if stops[0].0 > 0.0 {
                    stops.insert(0, (0.0, stops[0].1));
                }
                if stops[stops.len() - 1].0 < 1.0 {
                    stops.push((1.0, stops[stops.len() - 1].1));
                }
                let mut parts = Vec::new();
                for pair in stops.windows(2) {
                    let id = doc.alloc();
                    let (a, b) = (pair[0].1.floats(), pair[1].1.floats());
                    doc.obj(id, |c| {
                        c.exponential_function(id)
                            .domain([0.0, 1.0])
                            .c0([a.0, a.1, a.2])
                            .c1([b.0, b.1, b.2])
                            .n(1.0);
                    });
                    parts.push(id);
                }
                let func = if parts.len() == 1 {
                    parts[0]
                } else {
                    let id = doc.alloc();
                    // Bounds must increase strictly; nudge coincident stops apart.
                    let mut bounds: Vec<f32> = stops[1..stops.len() - 1].iter().map(|s| s.0).collect();
                    for i in 0..bounds.len() {
                        let lo = if i == 0 { 0.0 } else { bounds[i - 1] };
                        bounds[i] = bounds[i].max(lo + 1e-4).min(1.0 - 1e-4 * (bounds.len() - i) as f32);
                    }
                    let encode: Vec<f32> = parts.iter().flat_map(|_| [0.0, 1.0]).collect();
                    let refs = parts.clone();
                    doc.obj(id, |c| {
                        c.stitching_function(id)
                            .domain([0.0, 1.0])
                            .functions(refs)
                            .bounds(bounds)
                            .encode(encode);
                    });
                    id
                };
                let id = doc.alloc();
                let kind = g.kind;
                doc.obj(id, |c| {
                    let mut sh = c.function_shading(id);
                    match kind {
                        GradientKind::Linear(deg) => {
                            // PowerPoint angles run clockwise with y down; the unit box is y up.
                            let (s, co) = deg.to_radians().sin_cos();
                            let (dx, dy) = (co, -s);
                            let half = 0.5 * (co.abs() + s.abs());
                            sh.shading_type(FunctionShadingType::Axial);
                            sh.coords([0.5 - dx * half, 0.5 - dy * half, 0.5 + dx * half, 0.5 + dy * half]);
                        }
                        GradientKind::Radial(cx, cy) => {
                            let cy = 1.0 - cy;
                            let r = [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                                .iter()
                                .map(|(px, py)| ((px - cx).powi(2) + (py - cy).powi(2)).sqrt())
                                .fold(0.0, f32::max);
                            sh.shading_type(FunctionShadingType::Radial);
                            sh.coords([cx, cy, 0.0, cx, cy, r]);
                        }
                    }
                    sh.color_space().device_rgb();
                    sh.function(func);
                    sh.extend([true, true]);
                });
                doc.shadings.insert(key, id);
                id
            }
        };
        let name = format!("S{}", id.get());
        if !page.shadings.iter().any(|(n, _)| *n == name) {
            page.shadings.push((name.clone(), id));
        }
        name
    }

    fn draw_image(&mut self, page: &mut PageCtx, media: &str, r: &Rect, doc: &mut DocWriter, flip: (bool, bool)) {
        let Some(img) = self.image(media) else { return };
        let key = fnv1a(&img.data);
        let (id, name) = match doc.images.get(&key) {
            Some(v) => v.clone(),
            None => {
                let id = doc.alloc();
                let name = format!("I{}", doc.images.len() + 1);
                write_image(doc, id, &img);
                doc.images.insert(key, (id, name.clone()));
                (id, name)
            }
        };
        if !page.images.iter().any(|(n, _)| *n == name) {
            page.images.push((name.clone(), id));
        }
        let (x, y, w, h) = page.bbox(r);
        page.c.save_state();
        let (a, ex) = if flip.0 { (-w, x + w) } else { (w, x) };
        let (d, fy) = if flip.1 { (-h, y + h) } else { (h, y) };
        page.c.transform([a, 0.0, 0.0, d, ex, fy]);
        page.c.x_object(Name(name.as_bytes()));
        page.c.restore_state();
    }
}

fn write_image(doc: &mut DocWriter, id: Ref, img: &Prepared) {
    let mask_id = img.smask.as_ref().map(|_| doc.alloc());
    doc.obj(id, |c| {
        let mut x = c.image_xobject(id, &img.data);
        x.width(img.width as i32);
        x.height(img.height as i32);
        x.bits_per_component(8);
        match img.space {
            ColorSpace::Gray => x.color_space().device_gray(),
            ColorSpace::Rgb => x.color_space().device_rgb(),
            ColorSpace::Cmyk => x.color_space().device_cmyk(),
        };
        if img.dct {
            x.filter(Filter::DctDecode);
        } else {
            x.filter(Filter::FlateDecode);
            x.decode_parms()
                .predictor(Predictor::PngOptimum)
                .colors(img.space.comps() as i32)
                .bits_per_component(8)
                .columns(img.width as i32);
        }
        if img.invert {
            x.decode([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
        }
        if let Some(m) = mask_id {
            x.s_mask(m);
        }
    });
    if let (Some(m), Some(data)) = (mask_id, &img.smask) {
        doc.obj(m, |c| {
            let mut x = c.image_xobject(m, data);
            x.width(img.width as i32);
            x.height(img.height as i32);
            x.bits_per_component(8);
            x.color_space().device_gray();
            x.filter(Filter::FlateDecode);
            x.decode_parms()
                .predictor(Predictor::PngOptimum)
                .colors(1)
                .bits_per_component(8)
                .columns(img.width as i32);
        });
    }
}

/// NaN or infinite numbers would be written literally and make viewers drop
/// the whole page's content; clamp them to something drawable.
fn fin(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-1.0e6, 1.0e6)
    } else {
        0.0
    }
}

/// The rect the whole image occupies when `rect` shows only the cropped part.
fn uncropped(rect: &Rect, crop: [f32; 4]) -> Rect {
    let [l, t, r, b] = crop;
    let fw = (1.0 - l - r).max(0.01);
    let fh = (1.0 - t - b).max(0.01);
    let (w, h) = (rect.w / fw, rect.h / fh);
    Rect {
        x: rect.x - l * w,
        y: rect.y - t * h,
        w,
        h,
    }
}

pub fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn font_res_name(k: FontKey) -> String {
    let fam = match k.0 {
        Family::Sans => 'H',
        Family::Serif => 'T',
        Family::Mono => 'C',
    };
    format!("{fam}{}", k.1 as u8 * 2 + k.2 as u8)
}

struct PageCtx {
    c: Content,
    ph: f32,
    fonts: Vec<FontId>,
    images: Vec<(String, Ref)>,
    shadings: Vec<(String, Ref)>,
    gstates: Vec<(String, Ref)>,
}

impl PageCtx {
    /// EMU rect (top-left origin) -> PDF (x, y, w, h) in points, bottom-left origin.
    fn bbox(&self, r: &Rect) -> (f32, f32, f32, f32) {
        let s = EMU_PER_PT;
        (fin(r.x / s), fin(self.ph - (r.y + r.h) / s), fin(r.w / s), fin(r.h / s))
    }

    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        (fin(x / EMU_PER_PT), fin(self.ph - y / EMU_PER_PT))
    }

    fn use_font(&mut self, k: FontId) {
        if !self.fonts.contains(&k) {
            self.fonts.push(k);
        }
    }
}

/// PowerPoint rotates clockwise around the shape center; PDF's y axis
/// points up, so the angle is negated.
fn rotate(page: &mut PageCtx, r: &Rect, deg: f32) {
    if deg.abs() < 0.01 {
        return;
    }
    let (cx, cy) = page.pt(r.x + r.w / 2.0, r.y + r.h / 2.0);
    let (s, c) = (-deg).to_radians().sin_cos();
    page.c.transform([c, s, -s, c, cx - c * cx + s * cy, cy - s * cx - c * cy]);
}

/// Draws the shape outline and fill. `shading` is the page resource name
/// of a gradient to paint inside the shape instead of a solid fill.
fn draw_geometry(page: &mut PageCtx, s: &Shape, shading: Option<&str>) {
    let fill = match &s.fill {
        Some(Fill::Solid(c)) | Some(Fill::Translucent(c, _)) => Some(*c),
        _ => None,
    };
    // Width 0 is a hairline in OOXML (no line is `noFill`, i.e. no stroke).
    let stroke = s.stroke;
    if fill.is_none() && stroke.is_none() && shading.is_none() {
        return;
    }
    let is_line = s.geometry == Geometry::Line;
    if fill.is_none() && is_line && stroke.is_none() {
        return;
    }
    if let Some(c) = fill {
        let (r, g, b) = c.floats();
        page.c.set_fill_rgb(r, g, b);
    }
    if let Some(st) = stroke {
        let (r, g, b) = st.color.floats();
        page.c.set_stroke_rgb(r, g, b);
        page.c.set_line_width((st.width / EMU_PER_PT).max(0.25));
    }
    let (x, y, w, h) = page.bbox(&s.rect);
    // Unit coordinates (origin top-left of the box) -> PDF points.
    let map = |u: f32, v: f32| {
        let u = if s.flip_h { 1.0 - u } else { u };
        let v = if s.flip_v { 1.0 - v } else { v };
        (fin(x + u * w), fin(y + h - v * h))
    };
    let c = &mut page.c;
    if let Geometry::Line = s.geometry {
        let (a, b) = (map(0.0, 0.0), map(1.0, 1.0));
        c.move_to(a.0, a.1);
        c.line_to(b.0, b.1);
        if stroke.is_some() {
            c.stroke();
        } else {
            c.end_path();
        }
        return;
    }
    let open = matches!(s.geometry, Geometry::Open(_));
    if let (Some(sh), false) = (shading, open) {
        // Clip to the outline and paint the gradient in the unit box.
        c.save_state();
        trace(c, &s.geometry, (x, y, w, h), &map);
        c.clip_nonzero();
        c.end_path();
        c.transform([w, 0.0, 0.0, h, x, y]);
        c.shading(Name(sh.as_bytes()));
        c.restore_state();
        if stroke.is_some() {
            trace(c, &s.geometry, (x, y, w, h), &map);
            c.stroke();
        }
        return;
    }
    trace(c, &s.geometry, (x, y, w, h), &map);
    let fill = fill.filter(|_| !open);
    let eo = matches!(s.geometry, Geometry::PathEvenOdd(_));
    match (fill.is_some(), stroke.is_some()) {
        (_, false) if open => c.end_path(),
        (true, true) if eo => c.fill_even_odd_and_stroke(),
        (true, false) if eo => c.fill_even_odd(),
        (true, true) => c.fill_nonzero_and_stroke(),
        (true, false) => c.fill_nonzero(),
        _ => c.stroke(),
    };
}

/// Emits the outline path of a geometry (not lines).
fn trace(c: &mut Content, g: &Geometry, (x, y, w, h): (f32, f32, f32, f32), map: &dyn Fn(f32, f32) -> (f32, f32)) {
    match g {
        Geometry::Rect | Geometry::Line => {
            c.rect(x, y, w, h);
        }
        Geometry::RoundRect(adj) => {
            let r = adj.clamp(0.0, 0.5) * w.min(h);
            let k = r * 0.4477; // r * (1 - kappa)
            c.move_to(x + r, y);
            c.line_to(x + w - r, y);
            c.cubic_to(x + w - k, y, x + w, y + k, x + w, y + r);
            c.line_to(x + w, y + h - r);
            c.cubic_to(x + w, y + h - k, x + w - k, y + h, x + w - r, y + h);
            c.line_to(x + r, y + h);
            c.cubic_to(x + k, y + h, x, y + h - k, x, y + h - r);
            c.line_to(x, y + r);
            c.cubic_to(x, y + k, x + k, y, x + r, y);
            c.close_path();
        }
        Geometry::Ellipse => {
            let (rx, ry) = (w / 2.0, h / 2.0);
            let (cx, cy) = (x + rx, y + ry);
            let (kx, ky) = (rx * 0.5523, ry * 0.5523);
            c.move_to(cx + rx, cy);
            c.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
            c.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
            c.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
            c.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
            c.close_path();
        }
        Geometry::Path(cmds) | Geometry::Open(cmds) | Geometry::PathEvenOdd(cmds) => {
            // A segment needs a current point; paths that start without a
            // move (or continue after a close) begin at the segment's end.
            let mut has_point = false;
            for cmd in cmds {
                match *cmd {
                    PathCmd::Move(u, v) => {
                        let p = map(u, v);
                        c.move_to(p.0, p.1);
                        has_point = true;
                    }
                    PathCmd::Line(u, v) => {
                        let p = map(u, v);
                        if has_point {
                            c.line_to(p.0, p.1);
                        } else {
                            c.move_to(p.0, p.1);
                            has_point = true;
                        }
                    }
                    PathCmd::Cubic(a, b, cc, d, e, f) => {
                        let (p1, p2, p3) = (map(a, b), map(cc, d), map(e, f));
                        if has_point {
                            c.cubic_to(p1.0, p1.1, p2.0, p2.1, p3.0, p3.1);
                        } else {
                            c.move_to(p3.0, p3.1);
                            has_point = true;
                        }
                    }
                    PathCmd::Close => {
                        if has_point {
                            c.close_path();
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- embedded fonts

fn write_embedded(doc: &mut DocWriter, id: Ref, font: &FontFile, usage: &Usage) -> Result<(), String> {
    use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
    let data = usage.subset(font)?;
    let (cid, desc, file, cmap) = (doc.alloc(), doc.alloc(), doc.alloc(), doc.alloc());
    let base = format!("{}+{}", subset_tag(&usage.glyphs), font.postscript_name);
    let info = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };

    doc.obj(id, |c| {
        c.type0_font(id)
            .base_font(Name(base.as_bytes()))
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(cid)
            .to_unicode(cmap);
    });
    doc.obj(cid, |ch| {
        let mut c = ch.cid_font(cid);
        c.subtype(CidFontType::Type2);
        c.base_font(Name(base.as_bytes()));
        c.system_info(info);
        c.font_descriptor(desc);
        c.default_width(0.0);
        c.cid_to_gid_map_predefined(Name(b"Identity"));
        let mut w = c.widths();
        for (g, (_, adv)) in &usage.glyphs {
            w.consecutive(*g, [*adv]);
        }
    });
    let mut flags = FontFlags::NON_SYMBOLIC;
    if font.italic {
        flags |= FontFlags::ITALIC;
    }
    let b = font.bbox;
    doc.obj(desc, |c| {
        c.font_descriptor(desc)
            .name(Name(base.as_bytes()))
            .flags(flags)
            .bbox(PRect::new(b[0], b[1], b[2], b[3]))
            .italic_angle(font.italic_angle)
            .ascent(font.ascent)
            .descent(font.descent)
            .cap_height(font.cap_height)
            .stem_v(if font.bold { 140.0 } else { 80.0 })
            .font_file2(file);
    });
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&data, 9);
    doc.obj(file, |c| {
        c.stream(file, &packed)
            .filter(Filter::FlateDecode)
            .pair(Name(b"Length1"), data.len() as i32);
    });

    let mut map = UnicodeCmap::new(Name(b"Custom"), info);
    for (g, (c, _)) in &usage.glyphs {
        map.pair(*g, *c);
    }
    let cm = map.finish();
    doc.obj(cmap, |c| {
        c.cmap(cmap, &cm);
    });
    Ok(())
}

// ---------------------------------------------------------------- text layout

struct Seg {
    font: FontId,
    size: f32,
    color: Rgb,
    underline: bool,
    bytes: Vec<u8>,
    /// Layout width in points.
    width: f32,
    /// Width of the glyphs drawn unscaled; `draw_seg` compresses them
    /// horizontally to `width`.
    natural: f32,
}

struct Line {
    segs: Vec<Seg>,
    width: f32,
    max_size: f32,
    first: bool,
}

/// Breaks a paragraph into lines that fit `avail_first` / `avail_rest`
/// points. Breaks at spaces; words longer than a line break anywhere.
fn layout_paragraph(p: &Paragraph, scale: f32, avail_first: f32, avail_rest: f32, wrap: bool, fonts: &mut Fonts) -> Vec<Line> {
    // Flatten into glyphs; None marks a hard break.
    let mut glyphs: Vec<Option<Glyph>> = Vec::new();
    for (ri, r) in p.runs.iter().enumerate() {
        if r.line_break {
            glyphs.push(None);
            continue;
        }
        for ch in r.text.chars() {
            match ch {
                '\n' | '\u{b}' | '\u{2028}' => glyphs.push(None),
                // Invisible format characters (zero-width space/joiners, BOM, soft hyphen).
                '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{AD}' => {}
                _ => glyphs.push(Some(fonts.glyph(r, ri, ch))),
            }
        }
    }
    let gw = |g: &Glyph| g.adv / 1000.0 * p.runs[g.run].size * scale;

    let mut lines: Vec<Vec<Glyph>> = vec![Vec::new()];
    let mut width = 0.0;
    let mut last_space: Option<usize> = None;
    for g in glyphs {
        let Some(g) = g else {
            lines.push(Vec::new());
            width = 0.0;
            last_space = None;
            continue;
        };
        let avail = if lines.len() == 1 { avail_first } else { avail_rest };
        let w = gw(&g);
        let cur = lines.last_mut().unwrap();
        if wrap && !g.space && width + w > avail && !cur.is_empty() {
            let tail = match last_space {
                Some(i) => cur.split_off(i + 1),
                None => Vec::new(),
            };
            lines.push(tail);
            width = lines.last().unwrap().iter().map(gw).sum();
            last_space = None;
        }
        let cur = lines.last_mut().unwrap();
        // Break opportunities: after a space, or after a hyphen (kept on the line).
        if g.space || (g.len == 1 && g.code[0] == b'-' && !cur.is_empty()) {
            last_space = Some(cur.len());
        }
        cur.push(g);
        width += w;
    }

    let default_size = p.runs.iter().find(|r| !r.line_break).map(|r| r.size).unwrap_or(p.end_size) * scale;
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut glyphs)| {
            while glyphs.last().map(|g| g.space).unwrap_or(false) {
                glyphs.pop();
            }
            let mut segs: Vec<Seg> = Vec::new();
            let mut cur: Option<(usize, FontId)> = None;
            for g in glyphs {
                let w = gw(&g);
                if cur != Some((g.run, g.font)) {
                    let r = &p.runs[g.run];
                    segs.push(Seg {
                        font: g.font,
                        size: r.size * scale,
                        color: r.color,
                        underline: r.underline,
                        bytes: Vec::new(),
                        width: 0.0,
                        natural: 0.0,
                    });
                    cur = Some((g.run, g.font));
                }
                let s = segs.last_mut().unwrap();
                s.bytes.extend_from_slice(&g.code[..g.len as usize]);
                s.width += w;
                s.natural += g.nat / 1000.0 * p.runs[g.run].size * scale;
            }
            let width = segs.iter().map(|s| s.width).sum();
            let max_size = segs.iter().map(|s| s.size).fold(0.0, f32::max);
            Line {
                segs,
                width,
                max_size: if max_size > 0.0 { max_size } else { default_size },
                first: i == 0,
            }
        })
        .collect()
}

fn draw_text(page: &mut PageCtx, rect: &Rect, t: &TextBody, fonts: &mut Fonts) {
    let s = EMU_PER_PT;
    let (x0, top) = (rect.x / s + t.insets[0] / s, rect.y / s + t.insets[1] / s);
    let avail = (rect.w - t.insets[0] - t.insets[2]) / s;
    let avail_h = (rect.h - t.insets[1] - t.insets[3]) / s;
    let scale = t.font_scale;

    struct Placed<'a> {
        para: &'a Paragraph,
        lines: Vec<Line>,
        before: f32,
        heights: Vec<f32>,
    }
    let mut placed = Vec::new();
    let mut total = 0.0;
    for (pi, p) in t.paragraphs.iter().enumerate() {
        let marl = p.margin_left / s;
        let first_x = if p.bullet.is_some() { marl } else { marl + p.indent / s };
        let lines = layout_paragraph(p, scale, avail - first_x, avail - marl, t.wrap, fonts);
        let heights: Vec<f32> = lines
            .iter()
            .map(|l| match p.line_spacing_pt {
                Some(pt) => pt * scale,
                None => l.max_size * 1.2 * p.line_spacing * (1.0 - t.line_reduction),
            })
            .collect();
        let before = if pi == 0 { 0.0 } else { p.space_before * scale };
        total += before + heights.iter().sum::<f32>() + p.space_after * scale;
        placed.push(Placed {
            para: p,
            lines,
            before,
            heights,
        });
    }
    if let Some(last) = t.paragraphs.last() {
        total -= last.space_after * scale;
    }
    let mut y = top
        + match t.anchor {
            Anchor::Top => 0.0,
            Anchor::Middle => (avail_h - total) / 2.0,
            Anchor::Bottom => avail_h - total,
        };

    for pl in placed {
        let p = pl.para;
        y += pl.before;
        let marl = p.margin_left / s;
        for (line, lh) in pl.lines.iter().zip(&pl.heights) {
            let baseline = y + lh - line.max_size * 0.25 * (lh / (line.max_size * 1.2)).min(1.0);
            let start = if line.first && p.bullet.is_none() {
                marl + p.indent / s
            } else {
                marl
            };
            let room = avail - start;
            let mut x = x0
                + start
                + match p.align {
                    Align::Center => (room - line.width) / 2.0,
                    Align::Right => room - line.width,
                    _ => 0.0,
                };
            if line.first {
                if let (Some(b), Some(first_run)) = (&p.bullet, p.runs.iter().find(|r| !r.line_break)) {
                    let bx = x0 + marl + p.indent / s;
                    let size = first_run.size * scale * b.scale;
                    let mut plain = first_run.clone();
                    plain.bold = false;
                    plain.italic = false;
                    let mut bytes = Vec::new();
                    let mut font = None;
                    for c in b.text.chars() {
                        let g = fonts.glyph(&plain, 0, c);
                        if font.is_some() && font != Some(g.font) {
                            break;
                        }
                        font = Some(g.font);
                        bytes.extend_from_slice(&g.code[..g.len as usize]);
                    }
                    // Keep the text clear of the bullet when the hanging
                    // indent is too small for it (e.g. indent 0 in text boxes).
                    let bw: f32 = b.text.chars().map(|c| fonts.glyph(&plain, 0, c).adv / 1000.0 * size).sum();
                    let min_x = bx + bw + size * 0.25;
                    if x < min_x && p.align != Align::Center && p.align != Align::Right {
                        x = min_x;
                    }
                    if let Some(font) = font {
                        draw_seg(
                            page,
                            &Seg {
                                font,
                                size,
                                color: b.color.unwrap_or(first_run.color),
                                underline: false,
                                width: 0.0,
                                natural: 0.0,
                                bytes,
                            },
                            bx,
                            baseline,
                        );
                    }
                }
            }
            for seg in &line.segs {
                draw_seg(page, seg, x, baseline);
                x += seg.width;
            }
            y += lh;
        }
        y += p.space_after * scale;
    }
}

fn draw_seg(page: &mut PageCtx, seg: &Seg, x: f32, baseline_from_top: f32) {
    if seg.bytes.is_empty() {
        return;
    }
    page.use_font(seg.font);
    let y = page.ph - baseline_from_top;
    let (r, g, b) = seg.color.floats();
    let name = seg.font.res_name();
    let c = &mut page.c;
    c.set_fill_rgb(r, g, b);
    c.begin_text();
    c.set_font(Name(name.as_bytes()), seg.size);
    c.set_text_matrix([1.0, 0.0, 0.0, 1.0, x, y]);
    let squeeze = if seg.natural > 0.0 && seg.width > 0.0 {
        seg.width / seg.natural
    } else {
        1.0
    };
    if (squeeze - 1.0).abs() > 0.005 {
        c.set_horizontal_scaling(squeeze * 100.0);
        c.show(Str(&seg.bytes));
        c.set_horizontal_scaling(100.0);
    } else {
        c.show(Str(&seg.bytes));
    }
    c.end_text();
    if seg.underline {
        c.rect(x, y - seg.size * 0.12, seg.width, (seg.size * 0.05).max(0.5));
        c.fill_nonzero();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_coordinates_are_clamped() {
        let page = PageCtx {
            c: Content::new(),
            ph: 540.0,
            fonts: vec![],
            images: vec![],
            shadings: vec![],
            gstates: vec![],
        };
        let (x, y, w, h) = page.bbox(&Rect {
            x: f32::NAN,
            y: f32::INFINITY,
            w: 1e30,
            h: -f32::INFINITY,
        });
        for v in [x, y, w, h] {
            assert!(v.is_finite() && v.abs() <= 1.0e6, "{v}");
        }
    }

    fn face(family: &str, weight: u16, italic: bool) -> FontFile {
        FontFile {
            data: std::sync::Arc::from(Vec::new()),
            family: family.into(),
            postscript_name: String::new(),
            bold: weight >= 600,
            italic,
            weight,
            weight_axis: None,
            instance: None,
            units_per_em: 1000.0,
            ascent: 0.0,
            descent: 0.0,
            cap_height: 0.0,
            italic_angle: 0.0,
            bbox: [0.0; 4],
        }
    }

    fn run(face: &str, bold: bool, italic: bool) -> Run {
        Run {
            text: "x".into(),
            size: 12.0,
            bold,
            italic,
            underline: false,
            color: Rgb::BLACK,
            family: Family::Sans,
            face: face.into(),
            line_break: false,
        }
    }

    #[test]
    fn runs_take_the_registered_face_of_the_nearest_weight() {
        let files = [
            face("montserrat", 400, false),
            face("montserrat", 800, false),
            face("montserrat", 900, false),
            face("montserrat", 400, true),
            face("outfit", 700, false),
        ];
        let fonts = Fonts {
            files: &files,
            usage: &mut [],
            missing: Vec::new(),
            substituted: Vec::new(),
        };
        assert_eq!(fonts.matching(&run("Montserrat Black", false, false)), Some(2));
        assert_eq!(fonts.matching(&run("Montserrat ExtraBold", false, false)), Some(1));
        // Bold asks for 700: the 800 file is nearer than the 400 one.
        assert_eq!(fonts.matching(&run("Montserrat", true, false)), Some(1));
        assert_eq!(fonts.matching(&run("Montserrat", false, true)), Some(3));
        assert_eq!(fonts.matching(&run("Montserrat Medium", false, false)), Some(0));
        assert_eq!(fonts.matching(&run("Inter", false, false)), None);
    }

    #[test]
    fn a_weight_in_the_typeface_name_draws_bold() {
        assert_eq!(FontKey::of(&run("Montserrat Black", false, false)).base_name(), "Helvetica-Bold");
        assert_eq!(FontKey::of(&run("Montserrat Light", false, false)).base_name(), "Helvetica");
        assert_eq!(
            FontKey::of(&run("Montserrat Italic", false, false)).base_name(),
            "Helvetica-Oblique"
        );
    }

    #[test]
    fn unregistered_typefaces_are_reported_once() {
        let mut fonts = Fonts {
            files: &[],
            usage: &mut [],
            missing: Vec::new(),
            substituted: Vec::new(),
        };
        for r in [
            run("Montserrat", false, false),
            run("Montserrat", true, false),
            run("Arial", false, false),
        ] {
            fonts.glyph(&r, 0, 'a');
        }
        assert_eq!(fonts.substituted, vec![("Montserrat".to_string(), "Helvetica")]);
    }

    #[test]
    fn a_character_missing_from_a_registered_font_is_not_a_missing_font() {
        // The file has no usable glyphs, so every character falls back.
        let files = [face("montserrat", 400, false)];
        let mut usage = [Usage::default()];
        let mut fonts = Fonts {
            files: &files,
            usage: &mut usage,
            missing: Vec::new(),
            substituted: Vec::new(),
        };
        fonts.glyph(&run("Montserrat", false, false), 0, '→');
        assert!(fonts.substituted.is_empty());
    }

    #[test]
    fn spanish_text_is_winansi() {
        for c in "áéíóúñÑ¿¡ü€–—•“”".chars() {
            assert!(winansi(c).is_some(), "{c}");
        }
    }

    #[test]
    fn wraps_at_spaces() {
        let run = Run {
            text: "hola mundo hola mundo".into(),
            size: 10.0,
            bold: false,
            italic: false,
            underline: false,
            color: Rgb::BLACK,
            family: Family::Sans,
            face: String::new(),
            line_break: false,
        };
        let p = Paragraph {
            align: Align::Left,
            margin_left: 0.0,
            indent: 0.0,
            bullet: None,
            line_spacing: 1.0,
            line_spacing_pt: None,
            space_before: 0.0,
            space_after: 0.0,
            runs: vec![run],
            end_size: 10.0,
        };
        let mut fonts = Fonts {
            files: &[],
            usage: &mut [],
            missing: Vec::new(),
            substituted: Vec::new(),
        };
        let lines = layout_paragraph(&p, 1.0, 60.0, 60.0, true, &mut fonts);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].segs[0].bytes, b"hola mundo");
    }
}
