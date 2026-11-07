//! Intermediate representation: a fully resolved deck.
//!
//! Everything here is already flattened: inheritance (slide -> layout ->
//! master -> theme) is resolved, group transforms are applied, and colors are
//! plain RGB. Backends never look at OOXML again. Coordinates are EMU
//! (914400 per inch, 12700 per point), origin top-left.

use std::collections::BTreeSet;

pub const EMU_PER_PT: f32 = 12700.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    pub const WHITE: Rgb = Rgb(255, 255, 255);

    pub fn floats(self) -> (f32, f32, f32) {
        (self.0 as f32 / 255.0, self.1 as f32 / 255.0, self.2 as f32 / 255.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone)]
pub struct Deck {
    pub width: f32,
    pub height: f32,
    pub slides: Vec<Slide>,
    /// Package paths of the images used (e.g. `ppt/media/image1.jpg`).
    /// The bytes stay in the package until rendering needs them.
    pub media: BTreeSet<String>,
    pub warnings: Vec<Warning>,
}

/// Something the converter could not reproduce faithfully.
#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    /// 1-based slide number, or `None` for deck-wide issues.
    pub slide: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Slide {
    /// 1-based position in the deck.
    pub number: usize,
    pub background: Option<Fill>,
    pub elements: Vec<Element>,
    pub hidden: bool,
}

impl Slide {
    /// Plain text of the slide, in drawing order. Useful for search indexes.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for e in &self.elements {
            if let Element::Shape(s) = e {
                if let Some(tb) = &s.text {
                    for p in &tb.paragraphs {
                        let line: String = p.runs.iter().map(|r| r.text.as_str()).collect();
                        if !line.trim().is_empty() {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(line.trim());
                        }
                    }
                }
            }
        }
        out
    }

    /// Text of the title placeholder, if the slide has one.
    pub fn title(&self) -> Option<String> {
        self.elements.iter().find_map(|e| match e {
            Element::Shape(s) if s.is_title => s.text.as_ref().map(|t| {
                t.paragraphs
                    .iter()
                    .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim()
                    .to_string()
            }),
            _ => None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Fill {
    Solid(Rgb),
    /// Solid color with opacity below 1 (0..1).
    Translucent(Rgb, f32),
    Image(String),
    Gradient(Gradient),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// (position 0..1, color), sorted by position.
    pub stops: Vec<(f32, Rgb)>,
    pub kind: GradientKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientKind {
    /// Angle in degrees, clockwise from the x axis (PowerPoint `lin ang`).
    Linear(f32),
    /// Center in unit box coordinates (top-left origin).
    Radial(f32, f32),
}

#[derive(Debug, Clone)]
pub enum Element {
    Shape(Shape),
    Picture(Picture),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Rect,
    RoundRect(f32),
    Ellipse,
    Line,
    /// Polygon or path in unit coordinates (0..1 of the shape box).
    Path(Vec<PathCmd>),
    /// Like `Path`, but only stroked (brackets and braces).
    Open(Vec<PathCmd>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Debug, Clone, Copy)]
pub struct Stroke {
    pub color: Rgb,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct Shape {
    pub rect: Rect,
    pub rotation: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub geometry: Geometry,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub text: Option<TextBody>,
    pub is_title: bool,
    /// Where the text goes when it differs from the shape box (SmartArt).
    pub text_rect: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct Picture {
    pub rect: Rect,
    pub rotation: f32,
    pub media: String,
    /// Cropping as fractions of the image: left, top, right, bottom.
    /// Negative values pad the image instead.
    pub crop: [f32; 4],
    pub flip_h: bool,
    pub flip_v: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone)]
pub struct TextBody {
    pub insets: [f32; 4], // left, top, right, bottom
    /// Text direction: 0, or 90 / 270 degrees for vertical text.
    pub rotation: f32,
    pub anchor: Anchor,
    pub wrap: bool,
    pub font_scale: f32,
    pub line_reduction: f32,
    pub paragraphs: Vec<Paragraph>,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub align: Align,
    pub margin_left: f32,
    pub indent: f32,
    pub bullet: Option<Bullet>,
    /// Line spacing as a multiple of single spacing (1.0 = 100%).
    pub line_spacing: f32,
    /// Exact line spacing in points, overriding `line_spacing` when set.
    pub line_spacing_pt: Option<f32>,
    pub space_before: f32,
    pub space_after: f32,
    pub runs: Vec<Run>,
    /// Font size used for empty paragraphs and the bullet.
    pub end_size: f32,
}

#[derive(Debug, Clone)]
pub struct Bullet {
    pub text: String,
    pub color: Option<Rgb>,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub text: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub color: Rgb,
    pub family: Family,
    /// Typeface name as written in the deck (theme fonts resolved).
    pub face: String,
    /// True for a hard line break (`a:br`); `text` is empty.
    pub line_break: bool,
}
