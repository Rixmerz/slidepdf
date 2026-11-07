//! Optional embedded TrueType/OpenType fonts (feature `fonts`).
//!
//! By default text uses the base-14 PDF fonts, which cost zero bytes but
//! only cover WinAnsi (Western European). Registering a font file lets the
//! PDF use the deck's real typeface (matched by family name) and covers
//! characters outside WinAnsi when the font is registered as a fallback.
//! Only the glyphs actually used are embedded (subsetting), written as a
//! CIDFontType2 with Identity-H encoding and a ToUnicode map so text stays
//! selectable and searchable.

use std::collections::{BTreeMap, HashMap};

/// A parsed font file the caller wants available to the renderer.
#[derive(Debug, Clone)]
pub struct FontFile {
    pub data: Vec<u8>,
    /// Lowercase family name, used to match `latin typeface` in the deck.
    pub family: String,
    pub postscript_name: String,
    pub bold: bool,
    pub italic: bool,
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub italic_angle: f32,
    pub bbox: [f32; 4],
}

impl FontFile {
    #[cfg(feature = "fonts")]
    pub fn parse(data: Vec<u8>) -> Result<Self, String> {
        use ttf_parser::name_id;
        let face = ttf_parser::Face::parse(&data, 0).map_err(|e| format!("invalid font file: {e}"))?;
        let name = |id: u16| face.names().into_iter().filter(|n| n.name_id == id).find_map(|n| n.to_string());
        let family = name(name_id::TYPOGRAPHIC_FAMILY)
            .or_else(|| name(name_id::FAMILY))
            .unwrap_or_default();
        let postscript_name = name(name_id::POST_SCRIPT_NAME)
            .unwrap_or_else(|| family.replace(' ', ""))
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let upm = face.units_per_em() as f32;
        let s = 1000.0 / upm;
        let bb = face.global_bounding_box();
        let out = FontFile {
            family: family.to_lowercase(),
            postscript_name,
            bold: face.is_bold(),
            italic: face.is_italic(),
            units_per_em: upm,
            ascent: face.ascender() as f32 * s,
            descent: face.descender() as f32 * s,
            cap_height: face.capital_height().unwrap_or(face.ascender()) as f32 * s,
            italic_angle: face.italic_angle(),
            bbox: [bb.x_min as f32 * s, bb.y_min as f32 * s, bb.x_max as f32 * s, bb.y_max as f32 * s],
            data: Vec::new(),
        };
        // Fail now, not at the end of a render: a font that cannot be
        // subset would leave text pointing at a missing font object.
        let mut r = subsetter::GlyphRemapper::new();
        r.remap(0);
        if let Some(g) = face.glyph_index('A') {
            r.remap(g.0);
        }
        subsetter::subset(&data, 0, &r).map_err(|e| format!("font cannot be subset: {e:?}"))?;
        Ok(FontFile { data, ..out })
    }

    #[cfg(not(feature = "fonts"))]
    pub fn parse(_data: Vec<u8>) -> Result<Self, String> {
        Err("this build has no font embedding (feature `fonts`)".into())
    }
}

/// Per-document state of one embedded font: which glyphs were used.
#[derive(Default)]
pub struct Usage {
    #[cfg(feature = "fonts")]
    pub remapper: Option<subsetter::GlyphRemapper>,
    /// new glyph id -> (char, advance in 1/1000 em)
    pub glyphs: BTreeMap<u16, (char, f32)>,
    cache: HashMap<char, Option<(u16, f32)>>,
}

impl Usage {
    /// Returns the subset glyph id and advance (1/1000 em) for `c`, or
    /// `None` when the font has no glyph for it.
    #[cfg(feature = "fonts")]
    pub fn glyph(&mut self, font: &FontFile, c: char) -> Option<(u16, f32)> {
        let entry = *self.cache.entry(c).or_insert_with(|| {
            let face = ttf_parser::Face::parse(&font.data, 0).ok()?;
            let gid = face.glyph_index(c)?;
            let adv = face.glyph_hor_advance(gid).unwrap_or(0) as f32 * 1000.0 / font.units_per_em;
            Some((gid.0, adv))
        });
        let (old, adv) = entry?;
        let remapper = self.remapper.get_or_insert_with(|| {
            let mut r = subsetter::GlyphRemapper::new();
            r.remap(0);
            r
        });
        let new = remapper.remap(old);
        self.glyphs.entry(new).or_insert((c, adv));
        Some((new, adv))
    }

    #[cfg(not(feature = "fonts"))]
    pub fn glyph(&mut self, _font: &FontFile, _c: char) -> Option<(u16, f32)> {
        let _ = &self.cache;
        None
    }

    /// Subsets the font to the used glyphs.
    #[cfg(feature = "fonts")]
    pub fn subset(&self, font: &FontFile) -> Result<Vec<u8>, String> {
        let mut r = self.remapper.clone().unwrap_or_default();
        // Glyph 0 (.notdef) must stay at 0.
        r.remap(0);
        subsetter::subset(&font.data, 0, &r).map_err(|e| format!("font subsetting failed: {e:?}"))
    }

    #[cfg(not(feature = "fonts"))]
    pub fn subset(&self, _font: &FontFile) -> Result<Vec<u8>, String> {
        Err("no font support".into())
    }
}

/// Six-letter subset tag derived from the glyph set, as PDF requires
/// (`ABCDEF+FontName`).
pub fn subset_tag(glyphs: &BTreeMap<u16, (char, f32)>) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for (g, (c, _)) in glyphs {
        for b in g.to_le_bytes().into_iter().chain((*c as u32).to_le_bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    (0..6).map(|i| (b'A' + ((h >> (i * 5)) % 26) as u8) as char).collect()
}
