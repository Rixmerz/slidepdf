//! Optional embedded TrueType/OpenType fonts (feature `fonts`).
//!
//! By default text uses the base-14 PDF fonts, which cost zero bytes but
//! only cover WinAnsi (Western European). Registering a font file lets the
//! PDF use the deck's real typeface (matched by family name) and covers
//! characters outside WinAnsi when the font is registered as a fallback.
//! Only the glyphs actually used are embedded (subsetting), written as a
//! CIDFontType2 with Identity-H encoding and a ToUnicode map so text stays
//! selectable and searchable.
//!
//! Decks name weights in the typeface itself ("Montserrat Black",
//! "Segoe UI Semibold"). [`face_style`] splits such a name into family,
//! weight and slant, so the run is matched to the registered file of the
//! nearest weight, or drawn with the bold base-14 font when none is
//! registered. A variable font (`wght` axis) is instanced at the weight the
//! run asks for, so its default (often Thin) outlines are never drawn.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// A parsed font file the caller wants available to the renderer.
#[derive(Debug, Clone)]
pub struct FontFile {
    /// The font file, shared by the instances of a variable font.
    pub data: Arc<[u8]>,
    /// Lowercase family name, used to match `latin typeface` in the deck.
    pub family: String,
    pub postscript_name: String,
    pub bold: bool,
    pub italic: bool,
    /// Weight class, 100 (Thin) to 900 (Black).
    pub weight: u16,
    /// `(min, max)` of the `wght` axis when this is a variable font.
    pub weight_axis: Option<(f32, f32)>,
    /// The `wght` value this face is drawn at; `None` uses the outlines as
    /// stored in the file.
    pub instance: Option<f32>,
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
        let weight_axis = face
            .variation_axes()
            .into_iter()
            .find(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
            .map(|a| (a.min_value, a.max_value));
        let weight = match face
            .variation_axes()
            .into_iter()
            .find(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
        {
            Some(a) => a.def_value.round().clamp(1.0, 1000.0) as u16,
            None => face.weight().to_number(),
        };
        let out = FontFile {
            family: family.to_lowercase(),
            postscript_name,
            bold: face.is_bold() || weight >= 600,
            italic: face.is_italic(),
            weight,
            weight_axis,
            instance: None,
            units_per_em: upm,
            ascent: face.ascender() as f32 * s,
            descent: face.descender() as f32 * s,
            cap_height: face.capital_height().unwrap_or(face.ascender()) as f32 * s,
            italic_angle: face.italic_angle(),
            bbox: [bb.x_min as f32 * s, bb.y_min as f32 * s, bb.x_max as f32 * s, bb.y_max as f32 * s],
            data: Arc::from(Vec::new()),
        };
        // Fail now, not at the end of a render: a font that cannot be
        // subset would leave text pointing at a missing font object.
        let mut r = subsetter::GlyphRemapper::new();
        r.remap(0);
        if let Some(g) = face.glyph_index('A') {
            r.remap(g.0);
        }
        subsetter::subset(&data, 0, &r).map_err(|e| format!("font cannot be subset: {e:?}"))?;
        Ok(FontFile {
            data: Arc::from(data),
            ..out
        })
    }

    #[cfg(not(feature = "fonts"))]
    pub fn parse(_data: Vec<u8>) -> Result<Self, String> {
        Err("this build has no font embedding (feature `fonts`)".into())
    }

    /// The faces this file provides: itself for a static font; for a
    /// variable font, one instance per standard weight (100, 200 ... 900)
    /// its `wght` axis covers. Instances share the file's bytes, and only
    /// the ones a deck uses are embedded. A range holding no standard weight
    /// (say 410..490) yields one instance at the axis default.
    pub fn instances(&self) -> Vec<FontFile> {
        let Some((min, max)) = self.weight_axis.filter(|_| self.instance.is_none()) else {
            return vec![self.clone()];
        };
        let stem = self.postscript_name.split('-').next().unwrap_or(&self.postscript_name).to_string();
        let slant = if self.italic { "Italic" } else { "" };
        let make = |w: u16, at: f32| FontFile {
            postscript_name: format!("{stem}-{}{slant}", weight_name(w)),
            bold: w >= 600,
            weight: w,
            instance: Some(at),
            ..self.clone()
        };
        let standard: Vec<FontFile> = (1..=9u16)
            .map(|i| i * 100)
            .filter(|&w| (min..=max).contains(&(w as f32)))
            .map(|w| make(w, w as f32))
            .collect();
        if !standard.is_empty() {
            return standard;
        }
        // `weight` is the axis default; keep it inside the range the axis allows.
        let at = (self.weight as f32).clamp(min, max);
        vec![make(self.weight, at)]
    }
}

/// Weight words as they appear at the end of typeface names, longest
/// first so "extra bold" is not read as "bold".
const WEIGHTS: [(&str, u16); 16] = [
    ("extralight", 200),
    ("ultralight", 200),
    ("extrabold", 800),
    ("ultrabold", 800),
    ("semibold", 600),
    ("demibold", 600),
    ("hairline", 100),
    ("regular", 400),
    ("medium", 500),
    ("normal", 400),
    ("light", 300),
    ("black", 900),
    ("heavy", 900),
    ("thin", 100),
    ("bold", 700),
    ("book", 400),
];

fn weight_of(word: &str) -> Option<u16> {
    WEIGHTS.iter().find(|(n, _)| *n == word).map(|&(_, w)| w)
}

/// Splits a typeface name as written in a deck into its lowercase family,
/// the weight named in it (if any) and whether it names an italic:
/// "Montserrat ExtraBold Italic" -> ("montserrat", Some(800), true).
pub fn face_style(face: &str) -> (String, Option<u16>, bool) {
    let mut words: Vec<String> = face.split_whitespace().map(|w| w.to_lowercase()).collect();
    let (mut weight, mut italic) = (None, false);
    while words.len() > 1 {
        let last = words.last().map(|w| w.replace('-', "")).unwrap_or_default();
        if !italic && (last == "italic" || last == "oblique") {
            italic = true;
        } else if let (None, Some(w)) = (weight, weight_of(&last)) {
            weight = Some(w);
            // "Extra Bold", "Semi Bold": the modifier is a word of its own.
            if words.len() > 2 {
                if let Some(w2) = weight_of(&format!("{}{last}", words[words.len() - 2])) {
                    weight = Some(w2);
                    words.pop();
                }
            }
        } else {
            break;
        }
        words.pop();
    }
    (words.join(" "), weight, italic)
}

/// The weight a run is drawn at: the one its typeface names (400 if none),
/// raised to bold when the run is bold.
pub fn run_weight(face: &str, bold: bool) -> u16 {
    let w = face_style(face).1.unwrap_or(400);
    if bold {
        w.max(700)
    } else {
        w
    }
}

fn weight_name(w: u16) -> &'static str {
    match w {
        100 => "Thin",
        200 => "ExtraLight",
        300 => "Light",
        500 => "Medium",
        600 => "SemiBold",
        700 => "Bold",
        800 => "ExtraBold",
        900 => "Black",
        _ => "Regular",
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
            let mut face = ttf_parser::Face::parse(&font.data, 0).ok()?;
            if let Some(w) = font.instance {
                // Advances change with weight (HVAR); measure the instance.
                face.set_variation(ttf_parser::Tag::from_bytes(b"wght"), w);
            }
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
        match font.instance {
            Some(w) => subsetter::subset_with_variations(&font.data, 0, &[(subsetter::Tag::new(b"wght"), w)], &r),
            None => subsetter::subset(&font.data, 0, &r),
        }
        .map_err(|e| format!("font subsetting failed: {e:?}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_and_slant_come_out_of_the_typeface_name() {
        assert_eq!(face_style("Montserrat Black"), ("montserrat".into(), Some(900), false));
        assert_eq!(face_style("Montserrat ExtraBold"), ("montserrat".into(), Some(800), false));
        assert_eq!(face_style("Montserrat Extra Bold Italic"), ("montserrat".into(), Some(800), true));
        assert_eq!(face_style("Segoe UI Semibold"), ("segoe ui".into(), Some(600), false));
        assert_eq!(face_style("Calibri Light"), ("calibri".into(), Some(300), false));
        assert_eq!(face_style("Calibri"), ("calibri".into(), None, false));
        // A lone word is a family, never a weight.
        assert_eq!(face_style("Black"), ("black".into(), None, false));
        assert_eq!(face_style("Times New Roman"), ("times new roman".into(), None, false));
    }

    #[test]
    fn a_variable_font_without_a_standard_weight_keeps_one_instance() {
        let font = |axis, weight| FontFile {
            data: Arc::from(Vec::new()),
            family: "narrow".into(),
            postscript_name: "Narrow-Regular".into(),
            bold: false,
            italic: false,
            weight,
            weight_axis: Some(axis),
            instance: None,
            units_per_em: 1000.0,
            ascent: 0.0,
            descent: 0.0,
            cap_height: 0.0,
            italic_angle: 0.0,
            bbox: [0.0; 4],
        };
        let one = font((410.0, 490.0), 450).instances();
        assert_eq!(one.len(), 1);
        assert_eq!((one[0].weight, one[0].instance), (450, Some(450.0)));
        // A range with standard weights is unchanged: 300, 400, 500.
        let weights: Vec<u16> = font((300.0, 500.0), 400).instances().iter().map(|f| f.weight).collect();
        assert_eq!(weights, vec![300, 400, 500]);
    }

    #[test]
    fn bold_runs_never_get_lighter() {
        assert_eq!(run_weight("Montserrat", true), 700);
        assert_eq!(run_weight("Montserrat Black", true), 900);
        assert_eq!(run_weight("Montserrat Light", false), 300);
    }
}
