//! PPTX (OOXML PresentationML) reader: package -> resolved [`Deck`].
//!
//! The hard part of PPTX is inheritance. A placeholder on a slide usually has
//! no position, size or text style of its own: they come from the matching
//! placeholder on the layout, then on the master, and the master's text
//! styles and colors point into the theme. This module resolves all of that
//! so the IR is flat.

use std::collections::{BTreeSet, HashMap};
use std::io::{Cursor, Read, Seek};

use crate::ir::*;
use crate::presets;
use crate::smartart;
use crate::xml::{self, Node};

// ---------------------------------------------------------------- package

/// Defenses against hostile packages (zip bombs, huge parts).
pub const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 20_000;
pub const MAX_SLIDES: usize = 5_000;

/// An opened PPTX. Reads parts on demand, so it can sit on a file and
/// never hold the whole package in memory.
pub struct Package<R> {
    zip: zip::ZipArchive<R>,
    /// Bytes inflated so far; bounded by MAX_TOTAL_BYTES.
    inflated: u64,
}

#[derive(Debug, Clone)]
struct Rel {
    kind: String,
    target: String,
}

impl<'a> Package<Cursor<&'a [u8]>> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, String> {
        Package::open(Cursor::new(bytes))
    }
}

impl<R: Read + Seek> Package<R> {
    pub fn open(reader: R) -> Result<Self, String> {
        let zip = zip::ZipArchive::new(reader).map_err(|e| format!("not a PPTX (zip) file: {e}"))?;
        if zip.len() > MAX_ENTRIES {
            return Err(format!("package has {} entries, more than the {MAX_ENTRIES} allowed", zip.len()));
        }
        Ok(Self { zip, inflated: 0 })
    }

    fn exists(&mut self, path: &str) -> bool {
        self.zip.by_name(path).is_ok()
    }

    /// Reads a media part (counted only against the per-part limit, since
    /// media is read again while rendering).
    pub fn read_media(&mut self, path: &str) -> Option<Vec<u8>> {
        let f = self.zip.by_name(path).ok()?;
        let mut buf = Vec::with_capacity(f.size().min(MAX_PART_BYTES) as usize);
        f.take(MAX_PART_BYTES + 1).read_to_end(&mut buf).ok()?;
        (buf.len() as u64 <= MAX_PART_BYTES).then_some(buf)
    }

    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let budget = MAX_PART_BYTES.min(MAX_TOTAL_BYTES.saturating_sub(self.inflated));
        let f = self.zip.by_name(path).ok()?;
        // The declared size can lie; `take` enforces the real limit while inflating.
        let mut buf = Vec::with_capacity(f.size().min(budget) as usize);
        f.take(budget + 1).read_to_end(&mut buf).ok()?;
        if buf.len() as u64 > budget {
            return None;
        }
        self.inflated += buf.len() as u64;
        Some(buf)
    }

    fn xml(&mut self, path: &str) -> Option<Node> {
        let bytes = self.read(path)?;
        xml::parse(&bytes).ok()
    }

    /// Relationships of a part, keyed by rId, with targets resolved to
    /// absolute package paths. External targets are dropped.
    fn rels(&mut self, part: &str) -> HashMap<String, Rel> {
        let (dir, file) = match part.rfind('/') {
            Some(i) => (&part[..i], &part[i + 1..]),
            None => ("", part),
        };
        let rels_path = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let mut out = HashMap::new();
        let Some(root) = self.xml(&rels_path) else { return out };
        for r in root.children_named("Relationship") {
            if r.attr("TargetMode") == Some("External") {
                continue;
            }
            let (Some(id), Some(target)) = (r.attr("Id"), r.attr("Target")) else {
                continue;
            };
            let kind = r.attr("Type").unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            out.insert(
                id.to_string(),
                Rel {
                    kind,
                    target: resolve_path(dir, target),
                },
            );
        }
        out
    }
}

fn resolve_path(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(abs) = target.strip_prefix('/') {
        return normalize(abs.split('/').collect());
    } else if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    parts.extend(target.split('/'));
    normalize(parts)
}

fn normalize(parts: Vec<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in parts {
        match p {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

fn rel_of<'r>(rels: &'r HashMap<String, Rel>, kind: &str) -> Option<&'r Rel> {
    rels.values().find(|r| r.kind == kind)
}

// ---------------------------------------------------------------- theme & color

#[derive(Debug, Clone, Default)]
struct Theme {
    colors: HashMap<String, Rgb>,
    major: String,
    minor: String,
    line_widths: Vec<f32>,
    /// Theme fill styles (`fillStyleLst`, `bgFillStyleLst`), referenced by
    /// `fillRef idx` 1.. and `bgRef idx` 1001.. with the ref's color as phClr.
    fill_styles: Vec<Node>,
    bg_fill_styles: Vec<Node>,
}

fn parse_theme(root: &Node) -> Theme {
    let mut t = Theme::default();
    if let Some(scheme) = root.find("clrScheme") {
        for c in &scheme.children {
            if let Some(col) = c.children.first().and_then(raw_color) {
                t.colors.insert(c.name.clone(), col);
            }
        }
    }
    if let Some(fs) = root.find("fontScheme") {
        t.major = fs
            .path(&["majorFont", "latin"])
            .and_then(|n| n.attr("typeface"))
            .unwrap_or("")
            .into();
        t.minor = fs
            .path(&["minorFont", "latin"])
            .and_then(|n| n.attr("typeface"))
            .unwrap_or("")
            .into();
    }
    if let Some(l) = root.find("fillStyleLst") {
        t.fill_styles = l.children.clone();
    }
    if let Some(l) = root.find("bgFillStyleLst") {
        t.bg_fill_styles = l.children.clone();
    }
    if let Some(ls) = root.find("lnStyleLst") {
        t.line_widths = ls.children_named("ln").map(|l| l.attr_i64("w").unwrap_or(9525) as f32).collect();
    }
    t
}

/// Colors that need no theme: srgbClr, sysClr, prstClr.
fn raw_color(n: &Node) -> Option<Rgb> {
    match n.name.as_str() {
        "srgbClr" => n.attr("val").and_then(hex),
        "sysClr" => n.attr("lastClr").and_then(hex).or_else(|| match n.attr("val") {
            Some("window") => Some(Rgb::WHITE),
            _ => Some(Rgb::BLACK),
        }),
        "prstClr" => Some(match n.attr("val").unwrap_or("") {
            "white" => Rgb::WHITE,
            "red" => Rgb(255, 0, 0),
            "green" => Rgb(0, 128, 0),
            "blue" => Rgb(0, 0, 255),
            "yellow" => Rgb(255, 255, 0),
            "gray" | "grey" => Rgb(128, 128, 128),
            "orange" => Rgb(255, 165, 0),
            _ => Rgb::BLACK,
        }),
        _ => None,
    }
}

fn hex(s: &str) -> Option<Rgb> {
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[derive(Clone)]
struct ColorCtx<'a> {
    theme: &'a Theme,
    map: &'a HashMap<String, String>,
    ph: Option<Rgb>,
}

impl ColorCtx<'_> {
    /// Resolves a color element (srgbClr, schemeClr, ...) including its
    /// luminance modifiers.
    fn color(&self, n: &Node) -> Option<Rgb> {
        let base = match n.name.as_str() {
            "schemeClr" => {
                let v = n.attr("val")?;
                if v == "phClr" {
                    self.ph.unwrap_or(Rgb::BLACK)
                } else {
                    let key = self.map.get(v).map(String::as_str).unwrap_or(match v {
                        "bg1" => "lt1",
                        "tx1" => "dk1",
                        "bg2" => "lt2",
                        "tx2" => "dk2",
                        other => other,
                    });
                    *self.theme.colors.get(key)?
                }
            }
            "scrgbClr" => {
                let c = |k| (n.attr_i64(k).unwrap_or(0) as f32 / 100000.0 * 255.0) as u8;
                Rgb(c("r"), c("g"), c("b"))
            }
            _ => raw_color(n)?,
        };
        Some(apply_mods(base, n))
    }

    /// First color found under a fill-like container (`solidFill`, `fgClr`...).
    fn color_in(&self, container: &Node) -> Option<Rgb> {
        container.children.iter().find_map(|c| self.color(c))
    }

    /// Opacity of the first color element in `container` (alpha, alphaMod, alphaOff).
    fn alpha_in(&self, container: &Node) -> f32 {
        let Some(c) = container.children.iter().find(|c| c.name.ends_with("Clr")) else {
            return 1.0;
        };
        let mut a = 1.0f32;
        for m in &c.children {
            let v = m.attr_i64("val").unwrap_or(100000) as f32 / 100000.0;
            match m.name.as_str() {
                "alpha" => a = v,
                "alphaMod" => a *= v,
                "alphaOff" => a += v,
                _ => {}
            }
        }
        a.clamp(0.0, 1.0)
    }
}

fn apply_mods(c: Rgb, n: &Node) -> Rgb {
    let (mut h, mut s, mut l) = rgb_to_hsl(c);
    let (mut r, mut g, mut b) = c.floats();
    let mut hsl_dirty = false;
    for m in &n.children {
        let v = m.attr_i64("val").unwrap_or(100000) as f32 / 100000.0;
        match m.name.as_str() {
            "lumMod" => {
                l *= v;
                hsl_dirty = true;
            }
            "lumOff" => {
                l += v;
                hsl_dirty = true;
            }
            "satMod" => {
                s *= v;
                hsl_dirty = true;
            }
            "satOff" => {
                s += v;
                hsl_dirty = true;
            }
            // Hue values are in 60000ths of a degree, not 1/100000.
            "hueOff" => {
                let deg = m.attr_i64("val").unwrap_or(0) as f32 / 60000.0;
                h = (h + deg / 360.0).rem_euclid(1.0);
                hsl_dirty = true;
            }
            "hueMod" => {
                h = (h * v).rem_euclid(1.0);
                hsl_dirty = true;
            }
            "tint" | "shade" => {
                if hsl_dirty {
                    (r, g, b) = hsl_to_rgb(h, s, l).floats();
                    hsl_dirty = false;
                }
                let f = |x: f32| if m.name == "tint" { x * v + (1.0 - v) } else { x * v };
                (r, g, b) = (f(r), f(g), f(b));
                (h, s, l) = rgb_to_hsl(to_rgb(r, g, b));
            }
            _ => {}
        }
    }
    if hsl_dirty {
        hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
    } else {
        to_rgb(r, g, b)
    }
}

/// `c` blended with white; `t` is the share of `c` kept (1.0 = unchanged).
fn mix_with_white(c: Rgb, t: f32) -> Rgb {
    let (r, g, b) = c.floats();
    to_rgb(r * t + 1.0 - t, g * t + 1.0 - t, b * t + 1.0 - t)
}

/// Relative brightness 0..1 (Rec. 601 weights).
fn luminance(c: Rgb) -> f32 {
    let (r, g, b) = c.floats();
    0.299 * r + 0.587 * g + 0.114 * b
}

fn to_rgb(r: f32, g: f32, b: f32) -> Rgb {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(q(r), q(g), q(b))
}

fn rgb_to_hsl(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = c.floats();
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Rgb {
    if s == 0.0 {
        return to_rgb(l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    to_rgb(hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

// ---------------------------------------------------------------- text styles

#[derive(Debug, Clone, Default)]
struct RunStyle {
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    color: Option<Rgb>,
    font: Option<String>,
    /// `cap="all"` / `cap="small"`: drawn in capitals.
    caps: Option<bool>,
}

impl RunStyle {
    fn parse(n: &Node, cc: &ColorCtx) -> Self {
        Self {
            size: n.attr_i64("sz").map(|v| v as f32 / 100.0),
            bold: n.attr_bool("b"),
            italic: n.attr_bool("i"),
            // Hyperlinks take the theme's link color and are underlined.
            color: if n.child("hlinkClick").is_some() {
                cc.theme
                    .colors
                    .get("hlink")
                    .copied()
                    .or_else(|| n.child("solidFill").and_then(|f| cc.color_in(f)))
            } else {
                n.child("solidFill").and_then(|f| cc.color_in(f))
            },
            underline: if n.child("hlinkClick").is_some() {
                Some(true)
            } else {
                n.attr("u").map(|u| u != "none")
            },
            font: n.child("latin").and_then(|l| l.attr("typeface")).map(String::from),
            caps: n.attr("cap").map(|c| c == "all" || c == "small"),
        }
    }

    fn overlay(&mut self, o: &RunStyle) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(size, bold, italic, underline, color, font, caps);
    }
}

#[derive(Debug, Clone, Copy)]
enum Spacing {
    Pct(f32),
    Pts(f32),
}

#[derive(Debug, Clone)]
enum BulletKind {
    None,
    Char(String),
    AutoNum(String, u32),
}

#[derive(Debug, Clone, Default)]
struct ParaStyle {
    align: Option<Align>,
    margin_left: Option<f32>,
    indent: Option<f32>,
    bullet: Option<BulletKind>,
    bullet_symbol_font: Option<bool>,
    bullet_color: Option<Rgb>,
    bullet_scale: Option<f32>,
    line: Option<Spacing>,
    before: Option<Spacing>,
    after: Option<Spacing>,
    run: RunStyle,
}

fn spacing(n: Option<&Node>) -> Option<Spacing> {
    let n = n?;
    if let Some(p) = n.child("spcPct") {
        return Some(Spacing::Pct(p.attr_i64("val")? as f32 / 100000.0));
    }
    n.child("spcPts")
        .and_then(|p| p.attr_i64("val"))
        .map(|v| Spacing::Pts(v as f32 / 100.0))
}

impl ParaStyle {
    fn parse(n: &Node, cc: &ColorCtx) -> Self {
        let mut s = ParaStyle {
            align: n.attr("algn").map(|a| match a {
                "ctr" => Align::Center,
                "r" => Align::Right,
                "just" | "dist" => Align::Justify,
                _ => Align::Left,
            }),
            margin_left: n.attr_i64("marL").map(|v| v as f32),
            indent: n.attr_i64("indent").map(|v| v as f32),
            line: spacing(n.child("lnSpc")),
            before: spacing(n.child("spcBef")),
            after: spacing(n.child("spcAft")),
            ..Default::default()
        };
        if n.child("buNone").is_some() {
            s.bullet = Some(BulletKind::None);
        } else if let Some(c) = n.child("buChar") {
            s.bullet = Some(BulletKind::Char(c.attr("char").unwrap_or("•").to_string()));
        } else if let Some(a) = n.child("buAutoNum") {
            let start = a.attr_i64("startAt").unwrap_or(1).clamp(1, 32767) as u32;
            s.bullet = Some(BulletKind::AutoNum(a.attr("type").unwrap_or("arabicPeriod").into(), start));
        }
        if let Some(f) = n.child("buFont").and_then(|f| f.attr("typeface")) {
            let f = f.to_ascii_lowercase();
            s.bullet_symbol_font = Some(f.contains("wingdings") || f.contains("symbol") || f.contains("webdings"));
        }
        s.bullet_color = n.child("buClr").and_then(|c| cc.color_in(c));
        s.bullet_scale = n.child("buSzPct").and_then(|p| p.attr_i64("val")).map(|v| v as f32 / 100000.0);
        if let Some(r) = n.child("defRPr") {
            s.run = RunStyle::parse(r, cc);
        }
        s
    }

    fn overlay(&mut self, o: &ParaStyle) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(
            align,
            margin_left,
            indent,
            bullet,
            bullet_symbol_font,
            bullet_color,
            bullet_scale,
            line,
            before,
            after
        );
        self.run.overlay(&o.run);
    }
}

/// Nine outline levels of paragraph style, as found in `lstStyle`,
/// `titleStyle`, `bodyStyle`, `otherStyle` and `defaultTextStyle`.
#[derive(Debug, Clone, Default)]
struct ListStyle([ParaStyle; 9]);

impl ListStyle {
    fn parse(n: &Node, cc: &ColorCtx) -> Self {
        let mut ls = ListStyle::default();
        if let Some(d) = n.child("defPPr") {
            let d = ParaStyle::parse(d, cc);
            for l in ls.0.iter_mut() {
                l.overlay(&d);
            }
        }
        for (i, l) in ls.0.iter_mut().enumerate() {
            if let Some(p) = n.child(&format!("lvl{}pPr", i + 1)) {
                l.overlay(&ParaStyle::parse(p, cc));
            }
        }
        ls
    }
}

// ---------------------------------------------------------------- body properties

#[derive(Debug, Clone, Default)]
struct BodyProps {
    vert: Option<f32>,
    insets: [Option<f32>; 4],
    anchor: Option<Anchor>,
    wrap: Option<bool>,
}

impl BodyProps {
    fn parse(n: &Node) -> Self {
        let i = |k| n.attr_i64(k).map(|v| v as f32);
        BodyProps {
            vert: {
                // vert/vert270, plus `rot` (clockwise) and a 3D camera
                // revolution (counter-clockwise), all in 60000ths of a degree.
                let v = n.attr("vert").map(|v| match v {
                    "vert" | "eaVert" | "wordArtVertRtl" => 90.0,
                    "vert270" => 270.0,
                    _ => 0.0,
                });
                let rot = n.attr_i64("rot").map(|r| r as f32 / 60000.0);
                let rev = n
                    .path(&["scene3d", "camera", "rot"])
                    .and_then(|r| r.attr_i64("rev"))
                    .map(|r| r as f32 / 60000.0);
                if v.is_none() && rot.is_none() && rev.is_none() {
                    None
                } else {
                    Some((v.unwrap_or(0.0) + rot.unwrap_or(0.0) - rev.unwrap_or(0.0)).rem_euclid(360.0))
                }
            },
            insets: [i("lIns"), i("tIns"), i("rIns"), i("bIns")],
            anchor: n.attr("anchor").map(|a| match a {
                "ctr" => Anchor::Middle,
                "b" => Anchor::Bottom,
                _ => Anchor::Top,
            }),
            wrap: n.attr("wrap").map(|w| w != "none"),
        }
    }

    fn overlay(&mut self, o: &BodyProps) {
        for k in 0..4 {
            if o.insets[k].is_some() {
                self.insets[k] = o.insets[k];
            }
        }
        if o.anchor.is_some() {
            self.anchor = o.anchor;
        }
        if o.vert.is_some() {
            self.vert = o.vert;
        }
        if o.wrap.is_some() {
            self.wrap = o.wrap;
        }
    }
}

// ---------------------------------------------------------------- placeholders

#[derive(Debug, Clone)]
struct PhKey {
    kind: String,
    idx: Option<i64>,
}

fn ph_of(n: &Node) -> Option<PhKey> {
    let nv = n.children.iter().find(|c| c.name.starts_with("nv"))?;
    let ph = nv.path(&["nvPr", "ph"])?;
    Some(PhKey {
        kind: ph.attr("type").unwrap_or("obj").to_string(),
        idx: ph.attr_i64("idx"),
    })
}

/// Master placeholders only come in a few base types.
fn master_kind(kind: &str) -> &str {
    match kind {
        "ctrTitle" | "title" => "title",
        "dt" | "ftr" | "sldNum" => kind,
        _ => "body",
    }
}

fn shapes_of(root: &Node) -> Vec<&Node> {
    fn walk<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
        for c in &n.children {
            match c.name.as_str() {
                "sp" | "pic" | "graphicFrame" | "cxnSp" => out.push(c),
                "grpSp" => walk(c, out),
                "AlternateContent" => {
                    if let Some(f) = c.child("Fallback").or_else(|| c.child("Choice")) {
                        walk(f, out)
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if let Some(tree) = root.path(&["cSld", "spTree"]) {
        walk(tree, &mut out);
    }
    out
}

fn find_ph<'a>(root: Option<&'a Node>, key: &PhKey, by_master_kind: bool) -> Option<&'a Node> {
    let shapes = shapes_of(root?);
    let with_ph: Vec<(&Node, PhKey)> = shapes.into_iter().filter_map(|s| ph_of(s).map(|k| (s, k))).collect();
    if !by_master_kind {
        if let Some(idx) = key.idx {
            if let Some((s, _)) = with_ph.iter().find(|(_, k)| k.idx == Some(idx)) {
                return Some(s);
            }
        }
        if let Some((s, _)) = with_ph.iter().find(|(_, k)| k.kind == key.kind) {
            return Some(s);
        }
    }
    let mk = master_kind(&key.kind);
    with_ph.iter().find(|(_, k)| master_kind(&k.kind) == mk).map(|(s, _)| *s)
}

// ---------------------------------------------------------------- transforms

/// Maps coordinates inside a group (its child space) to the slide.
///
/// A child point first goes through `p * scale + offset`, which lands it in
/// the group's own rect before any turning. The group's flips and rotation
/// then act around `pivot`, the centre of that rect. Nested groups compose by
/// placing the inner group with the outer transform and building a new
/// `Xform` from the result, so the pivot is always in slide coordinates.
#[derive(Debug, Clone, Copy)]
struct Xform {
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
    pivot: (f32, f32),
    /// Clockwise degrees.
    rot: f32,
    flip_h: bool,
    flip_v: bool,
}

/// A shape's rect, rotation and flips once its group transform is applied.
#[derive(Debug, Clone, Copy)]
struct Placed {
    rect: Rect,
    rotation: f32,
    flip_h: bool,
    flip_v: bool,
}

impl Xform {
    const ID: Xform = Xform {
        sx: 1.0,
        sy: 1.0,
        tx: 0.0,
        ty: 0.0,
        pivot: (0.0, 0.0),
        rot: 0.0,
        flip_h: false,
        flip_v: false,
    };

    /// True when the group only scales and moves (no turning, no mirroring).
    fn is_plain(&self) -> bool {
        self.rot == 0.0 && !self.flip_h && !self.flip_v
    }

    /// Places a child shape. Like PowerPoint, the group scales the child's
    /// rect along its own axes and adds the group's turn to the child's
    /// rotation; it never shears a rotated child. A mirrored group (exactly
    /// one flip) also reverses the sense of the child's rotation.
    fn place(&self, r: Rect, rotation: f32, flip_h: bool, flip_v: bool) -> Placed {
        self.turn(self.rect(r), rotation, flip_h, flip_v)
    }

    /// The rect a child occupies once the group scales and moves it, before
    /// any turning. Shapes made of many pieces (tables, charts, metafiles) are
    /// built here and then turned with `turn_since`.
    fn rect(&self, r: Rect) -> Rect {
        Rect {
            x: r.x * self.sx + self.tx,
            y: r.y * self.sy + self.ty,
            w: r.w * self.sx,
            h: r.h * self.sy,
        }
    }

    /// Applies the group's flips and rotation, around its centre, to a shape
    /// already placed on the slide by `rect`.
    fn turn(&self, r: Rect, rotation: f32, flip_h: bool, flip_v: bool) -> Placed {
        if self.is_plain() {
            return Placed {
                rect: r,
                rotation,
                flip_h,
                flip_v,
            };
        }
        let mut dx = r.x + r.w / 2.0 - self.pivot.0;
        let mut dy = r.y + r.h / 2.0 - self.pivot.1;
        if self.flip_h {
            dx = -dx;
        }
        if self.flip_v {
            dy = -dy;
        }
        // y grows downward, so this matrix turns clockwise for positive angles.
        let (s, c) = self.rot.to_radians().sin_cos();
        let (cx, cy) = (self.pivot.0 + dx * c - dy * s, self.pivot.1 + dx * s + dy * c);
        let mirrored = self.flip_h != self.flip_v;
        let turned = if mirrored { self.rot - rotation } else { self.rot + rotation };
        Placed {
            rect: Rect {
                x: cx - r.w / 2.0,
                y: cy - r.h / 2.0,
                w: r.w,
                h: r.h,
            },
            rotation: turned.rem_euclid(360.0),
            flip_h: self.flip_h != flip_h,
            flip_v: self.flip_v != flip_v,
        }
    }

    /// Turns every element in `elements` like a child of this group. The
    /// elements must already sit at their slide position (see `rect`).
    fn turn_elements(&self, elements: &mut [Element]) {
        if self.is_plain() {
            return;
        }
        for e in elements {
            match e {
                Element::Shape(s) => {
                    let p = self.turn(s.rect, s.rotation, s.flip_h, s.flip_v);
                    // The text box keeps its offset from the shape's centre.
                    s.text_rect = s.text_rect.map(|t| Rect {
                        x: t.x + p.rect.x - s.rect.x,
                        y: t.y + p.rect.y - s.rect.y,
                        ..t
                    });
                    (s.rect, s.rotation, s.flip_h, s.flip_v) = (p.rect, p.rotation, p.flip_h, p.flip_v);
                }
                Element::Picture(pic) => {
                    let p = self.turn(pic.rect, pic.rotation, pic.flip_h, pic.flip_v);
                    (pic.rect, pic.rotation, pic.flip_h, pic.flip_v) = (p.rect, p.rotation, p.flip_h, p.flip_v);
                }
            }
        }
    }

    /// The child space of a nested group with the given `xfrm` values.
    fn group(&self, off: Rect, child: Rect, rotation: f32, flip_h: bool, flip_v: bool) -> Xform {
        let p = self.place(off, rotation, flip_h, flip_v);
        // A degenerate child extent keeps the parent's scale.
        let sx = if child.w > 0.0 { p.rect.w / child.w } else { self.sx };
        let sy = if child.h > 0.0 { p.rect.h / child.h } else { self.sy };
        Xform {
            sx,
            sy,
            tx: p.rect.x - child.x * sx,
            ty: p.rect.y - child.y * sy,
            pivot: (p.rect.x + p.rect.w / 2.0, p.rect.y + p.rect.h / 2.0),
            rot: p.rotation,
            flip_h: p.flip_h,
            flip_v: p.flip_v,
        }
    }

    /// Moves the child space origin by (`dx`, `dy`) in child units.
    fn shifted(&self, dx: f32, dy: f32) -> Xform {
        Xform {
            tx: self.tx + dx * self.sx,
            ty: self.ty + dy * self.sy,
            ..*self
        }
    }

    /// The text box `t` of a shape `r` that was placed at `at`. The box keeps
    /// its offset from the shape's centre, which the shape's own rotation then
    /// carries along when the page is drawn.
    fn text_rect(&self, r: Rect, at: Rect, t: Rect) -> Rect {
        if self.is_plain() {
            return self.rect(t);
        }
        let cx = at.x + at.w / 2.0 + ((t.x + t.w / 2.0) - (r.x + r.w / 2.0)) * self.sx;
        let cy = at.y + at.h / 2.0 + ((t.y + t.h / 2.0) - (r.y + r.h / 2.0)) * self.sy;
        let (w, h) = (t.w * self.sx, t.h * self.sy);
        Rect {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            w,
            h,
        }
    }
}

/// The child space of a `grpSpPr/xfrm` inside `parent`, or None when the
/// group has no usable rect. A missing `chOff`/`chExt` means the child space
/// is the group's own rect.
fn group_xform(parent: &Xform, x: &Node) -> Option<Xform> {
    let (off, ext) = (x.child("off")?, x.child("ext")?);
    let (cho, che) = (x.child("chOff").unwrap_or(off), x.child("chExt").unwrap_or(ext));
    let rect = |o: &Node, e: &Node| Rect {
        x: o.attr_i64("x").unwrap_or(0) as f32,
        y: o.attr_i64("y").unwrap_or(0) as f32,
        w: e.attr_i64("cx").unwrap_or(0) as f32,
        h: e.attr_i64("cy").unwrap_or(0) as f32,
    };
    Some(parent.group(
        rect(off, ext),
        rect(cho, che),
        x.attr_i64("rot").unwrap_or(0) as f32 / 60000.0,
        x.attr_bool("flipH").unwrap_or(false),
        x.attr_bool("flipV").unwrap_or(false),
    ))
}

struct RawXfrm {
    rect: Rect,
    rot: f32,
    flip_h: bool,
    flip_v: bool,
}

fn parse_xfrm(x: &Node) -> Option<RawXfrm> {
    let off = x.child("off")?;
    let ext = x.child("ext")?;
    Some(RawXfrm {
        rect: Rect {
            x: off.attr_i64("x").unwrap_or(0) as f32,
            y: off.attr_i64("y").unwrap_or(0) as f32,
            w: ext.attr_i64("cx").unwrap_or(0) as f32,
            h: ext.attr_i64("cy").unwrap_or(0) as f32,
        },
        rot: x.attr_i64("rot").unwrap_or(0) as f32 / 60000.0,
        flip_h: x.attr_bool("flipH").unwrap_or(false),
        flip_v: x.attr_bool("flipV").unwrap_or(false),
    })
}

fn xfrm_node(shape: &Node) -> Option<&Node> {
    match shape.name.as_str() {
        "graphicFrame" => shape.child("xfrm"),
        "grpSp" => shape.path(&["grpSpPr", "xfrm"]),
        _ => shape.path(&["spPr", "xfrm"]),
    }
}

// ---------------------------------------------------------------- deck parsing

/// Options that affect parsing.
#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    pub include_hidden: bool,
}

struct Part {
    root: Node,
    rels: HashMap<String, Rel>,
}

struct MasterCtx {
    part: Part,
    theme: Theme,
    map: HashMap<String, String>,
    title: ListStyle,
    body: ListStyle,
    other: ListStyle,
}

pub fn parse(bytes: &[u8], opts: &ParseOptions) -> Result<Deck, String> {
    parse_package(&mut Package::from_bytes(bytes)?, opts)
}

/// What a cheap look at a package tells about the work ahead. See [`inspect`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inspection {
    /// Slides listed by the presentation, hidden ones included.
    pub slides: usize,
    /// Page size in PDF points.
    pub width: f32,
    pub height: f32,
    /// Entries in the package.
    pub parts: usize,
    /// Uncompressed size of every part, as declared.
    pub bytes: u64,
    /// Pictures, videos and other media parts, and their declared bytes.
    pub media: usize,
    pub media_bytes: u64,
    /// The biggest media part: the one that drives peak memory.
    pub largest_media_bytes: u64,
    pub largest_media: String,
    /// Embedded font parts (ppt/fonts/*), which the converter ignores.
    pub embedded_fonts: usize,
}

/// Sizes up a package without converting it: only the zip directory and
/// presentation.xml are read, so it costs a few milliseconds whatever the
/// deck. Use it to route big decks to a bigger queue, or to refuse them,
/// before spending a conversion slot.
pub fn inspect<R: Read + Seek>(pkg: &mut Package<R>) -> Result<Inspection, String> {
    let mut i = Inspection {
        parts: pkg.zip.len(),
        ..Default::default()
    };
    for idx in 0..pkg.zip.len() {
        let Ok(f) = pkg.zip.by_index_raw(idx) else { continue };
        let size = f.size();
        i.bytes += size;
        let name = f.name();
        if name.starts_with("ppt/media/") {
            i.media += 1;
            i.media_bytes += size;
            if size > i.largest_media_bytes {
                i.largest_media_bytes = size;
                i.largest_media = name.to_string();
            }
        } else if name.starts_with("ppt/fonts/") {
            i.embedded_fonts += 1;
        }
    }
    let root_rels = pkg.rels("");
    let pres_path = rel_of(&root_rels, "officeDocument")
        .map(|r| r.target.clone())
        .unwrap_or_else(|| "ppt/presentation.xml".into());
    let pres = pkg.xml(&pres_path).ok_or("missing presentation.xml")?;
    let (w, h) = pres
        .child("sldSz")
        .map(|s| (s.attr_i64("cx").unwrap_or(9144000), s.attr_i64("cy").unwrap_or(6858000)))
        .unwrap_or((9144000, 6858000));
    i.width = w as f32 / crate::ir::EMU_PER_PT;
    i.height = h as f32 / crate::ir::EMU_PER_PT;
    i.slides = pres.child("sldIdLst").map(|l| l.children_named("sldId").count()).unwrap_or(0);
    Ok(i)
}

/// Parses the deck structure. Image bytes are not loaded: `Deck::media`
/// lists their paths, read later with [`Package::read_media`].
pub fn parse_package<R: Read + Seek>(pkg: &mut Package<R>, opts: &ParseOptions) -> Result<Deck, String> {
    let root_rels = pkg.rels("");
    let pres_path = rel_of(&root_rels, "officeDocument")
        .map(|r| r.target.clone())
        .unwrap_or_else(|| "ppt/presentation.xml".into());
    let pres = pkg.xml(&pres_path).ok_or("missing presentation.xml")?;
    let pres_rels = pkg.rels(&pres_path);

    let (width, height) = pres
        .child("sldSz")
        .map(|s| {
            (
                s.attr_i64("cx").unwrap_or(9144000) as f32,
                s.attr_i64("cy").unwrap_or(6858000) as f32,
            )
        })
        .unwrap_or((9144000.0, 6858000.0));

    let mut deck = Deck {
        width,
        height,
        slides: Vec::new(),
        media: BTreeSet::new(),
        embedded: std::collections::BTreeMap::new(),
        warnings: Vec::new(),
    };
    let mut masters: HashMap<String, MasterCtx> = HashMap::new();
    let mut layouts: HashMap<String, Part> = HashMap::new();
    let empty_theme = Theme::default();
    let default_text_xml = pres.child("defaultTextStyle").cloned();
    let table_styles: HashMap<String, Node> = rel_of(&pres_rels, "tableStyles")
        .and_then(|r| pkg.xml(&r.target.clone()))
        .map(|root| {
            root.children_named("tblStyle")
                .filter_map(|s| Some((s.attr("styleId")?.to_string(), s.clone())))
                .collect()
        })
        .unwrap_or_default();

    let slide_paths: Vec<String> = pres
        .child("sldIdLst")
        .map(|l| {
            l.children_named("sldId")
                .filter_map(|s| s.attr("r:id"))
                .filter_map(|rid| pres_rels.get(rid).map(|r| r.target.clone()))
                .collect()
        })
        .unwrap_or_default();

    if slide_paths.len() > MAX_SLIDES {
        return Err(format!("deck has {} slides, more than the {MAX_SLIDES} allowed", slide_paths.len()));
    }
    let mut number = 0;
    for (deck_index, slide_path) in slide_paths.into_iter().enumerate() {
        let deck_position = deck_index + 1;
        let Some(root) = pkg.xml(&slide_path) else {
            deck.warnings.push(Warning {
                slide: None,
                message: format!("{slide_path}: missing or invalid XML, skipped"),
            });
            continue;
        };
        let hidden = root.attr("show") == Some("0");
        if hidden && !opts.include_hidden {
            continue;
        }
        number += 1;
        let rels = pkg.rels(&slide_path);
        let slide = Part { root, rels };

        let layout_path = rel_of(&slide.rels, "slideLayout").map(|r| r.target.clone());
        if let Some(lp) = &layout_path {
            if !layouts.contains_key(lp) {
                if let Some(root) = pkg.xml(lp) {
                    let rels = pkg.rels(lp);
                    layouts.insert(lp.clone(), Part { root, rels });
                }
            }
        }
        let layout = layout_path.as_ref().and_then(|p| layouts.get(p));
        let master_path = layout.and_then(|l| rel_of(&l.rels, "slideMaster")).map(|r| r.target.clone());
        if let Some(mp) = &master_path {
            if !masters.contains_key(mp) {
                if let Some(m) = load_master(pkg, mp) {
                    masters.insert(mp.clone(), m);
                }
            }
        }
        let master = master_path.as_ref().and_then(|p| masters.get(p));

        let theme = master.map(|m| &m.theme).unwrap_or(&empty_theme);
        // Color map: master clrMap, then layout and slide overrides.
        let mut slide_map: HashMap<String, String> = master.map(|m| m.map.clone()).unwrap_or_default();
        let mut overridden = false;
        for p in [layout.map(|l| &l.root), Some(&slide.root)].into_iter().flatten() {
            if let Some(o) = p.path(&["clrMapOvr", "overrideClrMapping"]) {
                for (k, v) in &o.attrs {
                    if !k.contains(':') && k != "xmlns" {
                        slide_map.insert(k.clone(), v.clone());
                    }
                }
                overridden = true;
            }
        }
        let map = &slide_map;
        let cc = ColorCtx { theme, map, ph: None };
        // Master text styles were resolved with the master's map; redo them
        // when this slide remaps colors (e.g. a dark layout: tx1 -> lt1).
        let restyled = match (overridden, master) {
            (true, Some(m)) => {
                let st = m.part.root.child("txStyles");
                let get = |n: &str| st.and_then(|s| s.child(n)).map(|s| ListStyle::parse(s, &cc)).unwrap_or_default();
                Some([get("titleStyle"), get("bodyStyle"), get("otherStyle")])
            }
            _ => None,
        };
        let default_text = default_text_xml.as_ref().map(|n| ListStyle::parse(n, &cc)).unwrap_or_default();

        let mut b = SlideBuilder {
            pkg: &mut *pkg,
            _z: std::marker::PhantomData,
            deck_media: &mut deck.media,
            embedded: &mut deck.embedded,
            warnings: &mut deck.warnings,
            number,
            theme,
            map,
            master,
            layout,
            default_text: &default_text,
            table_styles: &table_styles,
            in_diagram: false,
            restyled: restyled.as_ref(),
            deck_position,
            elements: Vec::new(),
        };
        let background = b.background(&slide);
        let show_master = slide.root.attr("showMasterSp") != Some("0");
        let layout_shows_master = layout.map(|l| l.root.attr("showMasterSp") != Some("0")).unwrap_or(true);
        if show_master {
            if layout_shows_master {
                if let Some(m) = master {
                    b.walk_part(&m.part, true);
                }
            }
            if let Some(l) = layout {
                b.walk_part(l, true);
            }
        }
        b.walk_part(&slide, false);
        let elements = std::mem::take(&mut b.elements);
        deck.slides.push(Slide {
            number,
            background,
            elements,
            hidden,
        });
    }
    Ok(deck)
}

fn load_master<R: Read + Seek>(pkg: &mut Package<R>, path: &str) -> Option<MasterCtx> {
    let root = pkg.xml(path)?;
    let rels = pkg.rels(path);
    let theme = rel_of(&rels, "theme")
        .and_then(|r| pkg.xml(&r.target.clone()))
        .map(|t| parse_theme(&t))
        .unwrap_or_default();
    let map: HashMap<String, String> = root.child("clrMap").map(|m| m.attrs.iter().cloned().collect()).unwrap_or_default();
    let cc = ColorCtx {
        theme: &theme,
        map: &map,
        ph: None,
    };
    let styles = root.child("txStyles");
    let get = |n: &str| {
        styles
            .and_then(|s| s.child(n))
            .map(|s| ListStyle::parse(s, &cc))
            .unwrap_or_default()
    };
    let (title, body, other) = (get("titleStyle"), get("bodyStyle"), get("otherStyle"));
    Some(MasterCtx {
        part: Part { root, rels },
        theme,
        map,
        title,
        body,
        other,
    })
}

struct SlideBuilder<'p, 'z, 'c, R> {
    pkg: &'p mut Package<R>,
    _z: std::marker::PhantomData<&'z ()>,
    deck_media: &'p mut BTreeSet<String>,
    /// PNGs extracted from metafiles (see `Deck::embedded`).
    embedded: &'p mut std::collections::BTreeMap<String, Vec<u8>>,
    warnings: &'p mut Vec<Warning>,
    number: usize,
    theme: &'c Theme,
    map: &'c HashMap<String, String>,
    master: Option<&'c MasterCtx>,
    layout: Option<&'c Part>,
    default_text: &'c ListStyle,
    /// `tableStyles.xml` entries by style id.
    table_styles: &'c HashMap<String, Node>,
    /// True while drawing a SmartArt drawing: nested SmartArt is not
    /// expanded, so a drawing that references itself cannot recurse forever.
    in_diagram: bool,
    /// Master title/body/other styles re-resolved with this slide's color map.
    restyled: Option<&'c [ListStyle; 3]>,
    /// Position in the deck (hidden slides included), for slide-number fields.
    deck_position: usize,
    elements: Vec<Element>,
}

enum FillSpec {
    None,
    Some(Fill),
}

impl<'p, 'z, 'c, R: Read + Seek> SlideBuilder<'p, 'z, 'c, R> {
    fn cc(&self) -> ColorCtx<'c> {
        ColorCtx {
            theme: self.theme,
            map: self.map,
            ph: None,
        }
    }

    fn warn(&mut self, msg: impl Into<String>) {
        let m = Warning {
            slide: Some(self.number),
            message: msg.into(),
        };
        if !self.warnings.contains(&m) {
            self.warnings.push(m);
        }
    }

    fn media(&mut self, part: &Part, rid: &str) -> Option<String> {
        let target = part.rels.get(rid)?.target.clone();
        let lower = target.to_ascii_lowercase();
        if !(lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png") || lower.ends_with(".gif")) {
            let ext = lower.rsplit('.').next().unwrap_or("?").to_string();
            self.warn(format!("image format .{ext} is not supported yet ({target}), skipped"));
            return None;
        }
        if !self.deck_media.contains(&target) {
            if !self.pkg.exists(&target) {
                return None;
            }
            self.deck_media.insert(target.clone());
        }
        Some(target)
    }

    fn fill(&mut self, container: &Node, part: &Part, cc: &ColorCtx) -> Option<FillSpec> {
        for c in &container.children {
            match c.name.as_str() {
                "noFill" => return Some(FillSpec::None),
                "solidFill" => {
                    let alpha = cc.alpha_in(c);
                    return cc.color_in(c).map(|col| {
                        FillSpec::Some(if alpha < 0.995 {
                            Fill::Translucent(col, alpha)
                        } else {
                            Fill::Solid(col)
                        })
                    });
                }
                "gradFill" => {
                    let mut stops: Vec<(f32, Rgb)> = c
                        .path(&["gsLst"])
                        .map(|g| {
                            g.children_named("gs")
                                .filter_map(|s| Some(((s.attr_i64("pos").unwrap_or(0) as f32 / 100000.0).clamp(0.0, 1.0), cc.color_in(s)?)))
                                .collect()
                        })
                        .unwrap_or_default();
                    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                    if stops.len() < 2 {
                        return stops.first().map(|s| FillSpec::Some(Fill::Solid(s.1)));
                    }
                    let kind = if let Some(p) = c.child("path") {
                        let r = p.child("fillToRect");
                        let f = |k| r.and_then(|r| r.attr_i64(k)).unwrap_or(50000) as f32 / 100000.0;
                        GradientKind::Radial((f("l") + 1.0 - f("r")) / 2.0, (f("t") + 1.0 - f("b")) / 2.0)
                    } else {
                        GradientKind::Linear(c.child("lin").and_then(|l| l.attr_i64("ang")).unwrap_or(0) as f32 / 60000.0)
                    };
                    return Some(FillSpec::Some(Fill::Gradient(Gradient { stops, kind })));
                }
                "pattFill" => {
                    return c
                        .child("fgClr")
                        .and_then(|f| cc.color_in(f))
                        .map(|c| FillSpec::Some(Fill::Solid(c)))
                }
                "blipFill" => {
                    let rid = c.child("blip").and_then(|b| b.attr("embed"))?.to_string();
                    return self.media(part, &rid).map(|m| FillSpec::Some(Fill::Image(m)));
                }
                _ => {}
            }
        }
        None
    }

    fn background(&mut self, slide: &Part) -> Option<Fill> {
        let parts: Vec<&Part> = [Some(slide), self.layout, self.master.map(|m| &m.part)]
            .into_iter()
            .flatten()
            .collect();
        for part in parts {
            let Some(bg) = part.root.path(&["cSld", "bg"]) else { continue };
            if let Some(pr) = bg.child("bgPr") {
                let cc = self.cc();
                match self.fill(pr, part, &cc) {
                    Some(FillSpec::Some(f)) => return Some(f),
                    Some(FillSpec::None) => return None,
                    None => {}
                }
            }
            if let Some(r) = bg.child("bgRef") {
                let c = self.cc().color_in(r)?;
                let idx = r.attr_i64("idx").unwrap_or(0);
                return self.theme_fill(idx, c, part).or(Some(Fill::Solid(c)));
            }
        }
        None
    }

    /// Resolves a theme fill style reference with `color` as the
    /// placeholder color (phClr). idx 1-999: fillStyleLst, 1001+: bgFillStyleLst.
    fn theme_fill(&mut self, idx: i64, color: Rgb, part: &Part) -> Option<Fill> {
        let theme = self.theme;
        let node = match idx {
            1..=999 => theme.fill_styles.get(idx as usize - 1),
            1001.. => theme.bg_fill_styles.get(idx as usize - 1001),
            _ => None,
        }?;
        let wrapper = Node {
            children: vec![node.clone()],
            ..Default::default()
        };
        let cc = ColorCtx {
            theme,
            map: self.map,
            ph: Some(color),
        };
        match self.fill(&wrapper, part, &cc) {
            Some(FillSpec::Some(f)) => Some(f),
            _ => None,
        }
    }

    /// Walks a part's shape tree. On master and layout only decorative
    /// shapes are drawn; their placeholders are templates, not content.
    fn walk_part(&mut self, part: &Part, template: bool) {
        if let Some(tree) = part.root.path(&["cSld", "spTree"]) {
            self.walk(&tree.children, part, Xform::ID, template);
        }
    }

    fn walk(&mut self, nodes: &[Node], part: &Part, xf: Xform, template: bool) {
        for n in nodes {
            match n.name.as_str() {
                "sp" | "cxnSp" | "pic" | "graphicFrame" => {
                    // Hidden in PowerPoint's selection pane: not drawn.
                    let hidden = n
                        .children
                        .iter()
                        .find(|c| c.name.starts_with("nv"))
                        .and_then(|nv| nv.child("cNvPr"))
                        .and_then(|c| c.attr_bool("hidden"));
                    if hidden == Some(true) {
                        continue;
                    }
                    let ph = ph_of(n);
                    if template && ph.is_some() {
                        continue;
                    }
                    match n.name.as_str() {
                        "pic" => self.picture(n, part, ph.as_ref(), xf),
                        "graphicFrame" => self.graphic_frame(n, part, xf),
                        _ => self.shape(n, part, ph.as_ref(), xf),
                    }
                }
                "grpSp" => {
                    let Some(x) = n.path(&["grpSpPr", "xfrm"]) else {
                        self.walk(&n.children, part, xf, template);
                        continue;
                    };
                    let Some(combined) = group_xform(&xf, x) else { continue };
                    self.walk(&n.children, part, combined, template);
                }
                "AlternateContent" => {
                    if let Some(f) = n.child("Fallback").or_else(|| n.child("Choice")) {
                        self.walk(&f.children, part, xf, template);
                    }
                }
                _ => {}
            }
        }
    }

    /// The (layout, master) placeholder nodes a slide placeholder inherits from.
    fn inherited(&self, ph: Option<&PhKey>) -> (Option<&'_ Node>, Option<&'_ Node>) {
        let Some(key) = ph else { return (None, None) };
        let layout = find_ph(self.layout.map(|l| &l.root), key, false);
        let mkey = layout.and_then(ph_of).unwrap_or_else(|| key.clone());
        let master = find_ph(self.master.map(|m| &m.part.root), &mkey, true);
        (layout, master)
    }

    fn resolve_xfrm(&self, n: &Node, ph: Option<&PhKey>) -> Option<RawXfrm> {
        if let Some(x) = xfrm_node(n).and_then(parse_xfrm) {
            return Some(x);
        }
        let (l, m) = self.inherited(ph);
        [l, m].into_iter().flatten().find_map(|p| xfrm_node(p).and_then(parse_xfrm))
    }

    fn picture(&mut self, n: &Node, part: &Part, ph: Option<&PhKey>, xf: Xform) {
        let Some(x) = self.resolve_xfrm(n, ph) else { return };
        let Some(rid) = n.path(&["blipFill", "blip"]).and_then(|b| b.attr("embed")) else {
            return;
        };
        if self.metafile(part, rid, x.rect, xf) {
            return;
        }
        let placed = xf.place(x.rect, x.rot, x.flip_h, x.flip_v);
        let Some(media) = self.media(part, rid) else { return };
        let crop = n
            .path(&["blipFill", "srcRect"])
            .map(|s| {
                let f = |k| s.attr_i64(k).unwrap_or(0) as f32 / 100000.0;
                [f("l"), f("t"), f("r"), f("b")]
            })
            .unwrap_or([0.0; 4]);
        self.elements.push(Element::Picture(Picture {
            rect: placed.rect,
            rotation: placed.rotation,
            media,
            crop,
            flip_h: placed.flip_h,
            flip_v: placed.flip_v,
        }));
    }

    fn shape(&mut self, n: &Node, part: &Part, ph: Option<&PhKey>, xf: Xform) {
        let Some(x) = self.resolve_xfrm(n, ph) else { return };
        let (lph, mph) = self.inherited(ph);
        let (lph, mph) = (lph.cloned(), mph.cloned());
        let placed = xf.place(x.rect, x.rot, x.flip_h, x.flip_v);
        let rect = placed.rect;
        let cc = self.cc();

        // p:style supplies theme-referenced defaults for autoshapes.
        let style = n.child("style");
        let style_color = |name: &str| {
            style
                .and_then(|s| s.child(name))
                .map(|r| (r.attr_i64("idx").unwrap_or(0), cc.color_in(r)))
        };

        let geometry = self.geometry(n, rect);
        let is_line = geometry == Geometry::Line;

        // Fill: slide spPr -> layout ph -> master ph -> style fillRef.
        let mut fill = None;
        let mut resolved = false;
        for src in [Some(n), lph.as_ref(), mph.as_ref()].into_iter().flatten() {
            if let Some(sp) = src.child("spPr") {
                match self.fill(sp, part, &cc) {
                    Some(FillSpec::Some(f)) => {
                        fill = Some(f);
                        resolved = true;
                    }
                    Some(FillSpec::None) => resolved = true,
                    None => {}
                }
            }
            if resolved {
                break;
            }
        }
        if !resolved && !is_line {
            if let Some((idx, Some(c))) = style_color("fillRef") {
                if idx > 0 {
                    fill = self.theme_fill(idx, c, part).or(Some(Fill::Solid(c)));
                }
            }
        }

        // Outline: spPr/ln, else style lnRef.
        let mut stroke = None;
        let ln = [Some(n), lph.as_ref(), mph.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|s| s.path(&["spPr", "ln"]));
        let ln_explicit_none = ln.map(|l| l.child("noFill").is_some()).unwrap_or(false);
        if !ln_explicit_none {
            let (ref_idx, ref_color) = style_color("lnRef").unwrap_or((0, None));
            let color = ln
                .and_then(|l| l.child("solidFill"))
                .and_then(|f| cc.color_in(f))
                .or(if ref_idx > 0 { ref_color } else { None });
            let width = ln.and_then(|l| l.attr_i64("w")).map(|w| w as f32).unwrap_or_else(|| {
                let i = (ref_idx.max(1) - 1) as usize;
                self.theme.line_widths.get(i).copied().unwrap_or(9525.0)
            });
            if let Some(color) = color {
                // Line widths are absolute: PowerPoint keeps them when a
                // group is resized, and legacy groups (child space in old
                // master units, scale in the hundreds) would otherwise turn
                // into blobs.
                stroke = Some(Stroke { color, width });
            }
        }

        let font_ref_color = style_color("fontRef").and_then(|(_, c)| c);
        let is_title = ph.map(|k| master_kind(&k.kind) == "title").unwrap_or(false);
        let text = n
            .child("txBody")
            .and_then(|tb| self.text_body(tb, ph, lph.as_ref(), mph.as_ref(), font_ref_color));

        // Empty placeholders only show their prompt while editing; in a
        // presentation they are invisible unless the slide itself fills them.
        let own_fill = n
            .child("spPr")
            .map(|sp| sp.children.iter().any(|c| c.name.ends_with("Fill") && c.name != "noFill"))
            .unwrap_or(false);
        if ph.is_some() && text.is_none() && !own_fill {
            return;
        }
        self.elements.push(Element::Shape(Shape {
            rect,
            rotation: placed.rotation,
            flip_h: placed.flip_h,
            flip_v: placed.flip_v,
            geometry,
            fill,
            stroke,
            text,
            is_title,
            // SmartArt shapes carry their own text box (txXfrm).
            text_rect: n.child("txXfrm").and_then(parse_xfrm).map(|t| xf.text_rect(x.rect, rect, t.rect)),
        }));
    }

    fn geometry(&mut self, n: &Node, r: Rect) -> Geometry {
        if n.name == "cxnSp" {
            return Geometry::Line;
        }
        let sp = n.child("spPr");
        if let Some(cg) = sp.and_then(|s| s.child("custGeom")) {
            return custom_geometry(cg).unwrap_or(Geometry::Rect);
        }
        let Some(pg) = sp.and_then(|s| s.child("prstGeom")) else {
            return Geometry::Rect;
        };
        let prst = pg.attr("prst").unwrap_or("rect");
        // An adjust value in the file's own units (fractions are 100000ths,
        // angles 60000ths of a degree).
        let raw = |name: &str, def: f32| {
            pg.path(&["avLst"])
                .and_then(|a| a.children.iter().find(|g| g.attr("name") == Some(name)))
                .and_then(|g| g.attr("fmla"))
                .and_then(|f| f.strip_prefix("val "))
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite() && v.abs() <= 2_147_483_647.0)
                .unwrap_or(def)
        };
        let adj = |name: &str, def: f32| raw(name, def) / 100000.0;
        let ss = r.w.min(r.h);
        let (fx, fy) = if r.w > 0.0 && r.h > 0.0 { (ss / r.w, ss / r.h) } else { (0.0, 0.0) };
        let poly = |pts: &[(f32, f32)]| {
            let mut v: Vec<PathCmd> = pts
                .iter()
                .enumerate()
                .map(|(i, &(x, y))| if i == 0 { PathCmd::Move(x, y) } else { PathCmd::Line(x, y) })
                .collect();
            v.push(PathCmd::Close);
            Geometry::Path(v)
        };
        match prst {
            "rect" | "flowChartProcess" => Geometry::Rect,
            "roundRect" | "flowChartAlternateProcess" => Geometry::RoundRect(adj("adj", 16667.0)),
            "ellipse" | "flowChartConnector" => Geometry::Ellipse,
            "line" | "straightConnector1" | "bentConnector2" | "bentConnector3" | "bentConnector4" | "bentConnector5"
            | "curvedConnector2" | "curvedConnector3" | "curvedConnector4" | "curvedConnector5" => Geometry::Line,
            "triangle" => {
                let a = adj("adj", 50000.0);
                poly(&[(a, 0.0), (1.0, 1.0), (0.0, 1.0)])
            }
            "rtTriangle" => poly(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            "diamond" | "flowChartDecision" => poly(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]),
            "parallelogram" | "flowChartInputOutput" => {
                let a = adj("adj", 25000.0) * fx;
                poly(&[(a, 0.0), (1.0, 0.0), (1.0 - a, 1.0), (0.0, 1.0)])
            }
            "trapezoid" => {
                let a = adj("adj", 25000.0) * fx;
                poly(&[(a, 0.0), (1.0 - a, 0.0), (1.0, 1.0), (0.0, 1.0)])
            }
            "homePlate" => {
                let a = adj("adj", 50000.0) * fx;
                poly(&[(0.0, 0.0), (1.0 - a, 0.0), (1.0, 0.5), (1.0 - a, 1.0), (0.0, 1.0)])
            }
            "chevron" => {
                let a = adj("adj", 50000.0) * fx;
                poly(&[(0.0, 0.0), (1.0 - a, 0.0), (1.0, 0.5), (1.0 - a, 1.0), (0.0, 1.0), (a, 0.5)])
            }
            "pentagon" => poly(&[(0.5, 0.0), (1.0, 0.38), (0.81, 1.0), (0.19, 1.0), (0.0, 0.38)]),
            "hexagon" => {
                let a = adj("adj", 25000.0) * fx;
                poly(&[(a, 0.0), (1.0 - a, 0.0), (1.0, 0.5), (1.0 - a, 1.0), (a, 1.0), (0.0, 0.5)])
            }
            "octagon" => {
                let (a, b) = (adj("adj", 29289.0) * fx, adj("adj", 29289.0) * fy);
                poly(&[
                    (a, 0.0),
                    (1.0 - a, 0.0),
                    (1.0, b),
                    (1.0, 1.0 - b),
                    (1.0 - a, 1.0),
                    (a, 1.0),
                    (0.0, 1.0 - b),
                    (0.0, b),
                ])
            }
            "rightArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fx);
                let (t, b) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                poly(&[
                    (0.0, t),
                    (1.0 - a2, t),
                    (1.0 - a2, 0.0),
                    (1.0, 0.5),
                    (1.0 - a2, 1.0),
                    (1.0 - a2, b),
                    (0.0, b),
                ])
            }
            "leftArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fx);
                let (t, b) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                poly(&[(1.0, t), (a2, t), (a2, 0.0), (0.0, 0.5), (a2, 1.0), (a2, b), (1.0, b)])
            }
            "downArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fy);
                let (l, r) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                poly(&[
                    (l, 0.0),
                    (r, 0.0),
                    (r, 1.0 - a2),
                    (1.0, 1.0 - a2),
                    (0.5, 1.0),
                    (0.0, 1.0 - a2),
                    (l, 1.0 - a2),
                ])
            }
            "upArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fy);
                let (l, r) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                poly(&[(l, 1.0), (r, 1.0), (r, a2), (1.0, a2), (0.5, 0.0), (0.0, a2), (l, a2)])
            }
            "stripedRightArrow" | "notchedRightArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fx);
                let (t, b) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                let notch = if prst == "notchedRightArrow" { a2 * a1 } else { 0.0 };
                poly(&[
                    (0.0, t),
                    (1.0 - a2, t),
                    (1.0 - a2, 0.0),
                    (1.0, 0.5),
                    (1.0 - a2, 1.0),
                    (1.0 - a2, b),
                    (0.0, b),
                    (notch, 0.5),
                ])
            }
            "leftRightArrow" => {
                let (a1, a2) = (adj("adj1", 50000.0), adj("adj2", 50000.0) * fx);
                let (t, b) = (0.5 - a1 / 2.0, 0.5 + a1 / 2.0);
                poly(&[
                    (0.0, 0.5),
                    (a2, 0.0),
                    (a2, t),
                    (1.0 - a2, t),
                    (1.0 - a2, 0.0),
                    (1.0, 0.5),
                    (1.0 - a2, 1.0),
                    (1.0 - a2, b),
                    (a2, b),
                    (a2, 1.0),
                ])
            }
            "flowChartManualInput" => poly(&[(0.0, 0.2), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            "flowChartManualOperation" => poly(&[(0.0, 0.0), (1.0, 0.0), (0.8, 1.0), (0.2, 1.0)]),
            "flowChartMerge" => poly(&[(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)]),
            "flowChartExtract" => poly(&[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            "flowChartPreparation" => poly(&[(0.2, 0.0), (0.8, 0.0), (1.0, 0.5), (0.8, 1.0), (0.2, 1.0), (0.0, 0.5)]),
            "flowChartOffpageConnector" => poly(&[(0.0, 0.0), (1.0, 0.0), (1.0, 0.8), (0.5, 1.0), (0.0, 0.8)]),
            "flowChartDelay" => Geometry::Path(vec![
                PathCmd::Move(0.0, 0.0),
                PathCmd::Line(0.5, 0.0),
                PathCmd::Cubic(0.776, 0.0, 1.0, 0.224, 1.0, 0.5),
                PathCmd::Cubic(1.0, 0.776, 0.776, 1.0, 0.5, 1.0),
                PathCmd::Line(0.0, 1.0),
                PathCmd::Close,
            ]),
            "flowChartTerminator" | "round2DiagRect" | "round2SameRect" | "round1Rect" | "plaque" | "flowChartPunchedCard" => {
                Geometry::RoundRect(if prst == "flowChartTerminator" { 0.5 } else { 0.16667 })
            }
            "flowChartMagneticDisk"
            | "can"
            | "flowChartMagneticDrum"
            | "flowChartMagneticTape"
            | "flowChartPunchedTape"
            | "flowChartDocument"
            | "wave" => Geometry::RoundRect(0.12),
            "teardrop" | "sun" | "smileyFace" | "noSmoking" | "donut" | "flowChartOr" | "flowChartSummingJunction" => Geometry::Ellipse,
            "wedgeRectCallout" | "wedgeRoundRectCallout" | "wedgeEllipseCallout" => {
                // Tail tip relative to the center, in fractions of the box.
                let tip = (0.5 + adj("adj1", -20833.0), 0.5 + adj("adj2", 62500.0));
                let (tx, ty) = tip;
                let pts: Vec<(f32, f32)> = if ty > 1.0 {
                    vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.42, 1.0), tip, (0.25, 1.0), (0.0, 1.0)]
                } else if ty < 0.0 {
                    vec![(0.0, 0.0), (0.25, 0.0), tip, (0.42, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
                } else if tx < 0.0 {
                    vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.58), tip, (0.0, 0.42)]
                } else {
                    vec![(0.0, 0.0), (1.0, 0.0), (1.0, 0.42), tip, (1.0, 0.58), (1.0, 1.0), (0.0, 1.0)]
                };
                poly(&pts)
            }
            "plus" => {
                let (a, b) = (adj("adj", 25000.0) * fx, adj("adj", 25000.0) * fy);
                poly(&[
                    (a, 0.0),
                    (1.0 - a, 0.0),
                    (1.0 - a, b),
                    (1.0, b),
                    (1.0, 1.0 - b),
                    (1.0 - a, 1.0 - b),
                    (1.0 - a, 1.0),
                    (a, 1.0),
                    (a, 1.0 - b),
                    (0.0, 1.0 - b),
                    (0.0, b),
                    (a, b),
                ])
            }
            "corner" => {
                let (a, b) = (adj("adj2", 50000.0) * fx, adj("adj1", 50000.0) * fy);
                poly(&[(0.0, 0.0), (a, 0.0), (a, 1.0 - b), (1.0, 1.0 - b), (1.0, 1.0), (0.0, 1.0)])
            }
            "frame" => {
                let (a, b) = (adj("adj1", 12500.0) * fx, adj("adj1", 12500.0) * fy);
                use PathCmd::*;
                Geometry::Path(vec![
                    Move(0.0, 0.0),
                    Line(1.0, 0.0),
                    Line(1.0, 1.0),
                    Line(0.0, 1.0),
                    Close,
                    Move(a, b),
                    Line(a, 1.0 - b),
                    Line(1.0 - a, 1.0 - b),
                    Line(1.0 - a, b),
                    Close,
                ])
            }
            "halfFrame" => {
                let (a, b) = (adj("adj2", 33333.0) * fx, adj("adj1", 33333.0) * fy);
                poly(&[(0.0, 0.0), (1.0, 0.0), (1.0 - a, b), (a, b), (a, 1.0 - b), (0.0, 1.0)])
            }
            "diagStripe" => {
                let a = adj("adj", 50000.0);
                poly(&[(0.0, a), (a, 0.0), (1.0, 0.0), (0.0, 1.0)])
            }
            "lightningBolt" => poly(&[
                (0.39, 0.0),
                (0.62, 0.32),
                (0.52, 0.37),
                (0.8, 0.62),
                (0.7, 0.66),
                (1.0, 1.0),
                (0.45, 0.72),
                (0.56, 0.68),
                (0.2, 0.45),
                (0.3, 0.41),
                (0.0, 0.17),
            ]),
            "heart" => Geometry::Path(vec![
                PathCmd::Move(0.5, 0.25),
                PathCmd::Cubic(0.5, -0.05, 0.0, -0.05, 0.0, 0.3),
                PathCmd::Cubic(0.0, 0.6, 0.45, 0.75, 0.5, 1.0),
                PathCmd::Cubic(0.55, 0.75, 1.0, 0.6, 1.0, 0.3),
                PathCmd::Cubic(1.0, -0.05, 0.5, -0.05, 0.5, 0.25),
                PathCmd::Close,
            ]),
            "heptagon" | "decagon" | "dodecagon" => {
                let n = match prst {
                    "heptagon" => 7,
                    "decagon" => 10,
                    _ => 12,
                };
                poly(&regular(n, 1.0, 0.0))
            }
            "star4" | "star5" | "star6" | "star7" | "star8" | "star10" | "star12" | "star16" | "star24" | "star32" => {
                let n: usize = prst[4..].parse().unwrap_or(5);
                let def = match n {
                    4 => 12500.0,
                    5 => 19098.0,
                    6 => 28868.0,
                    7 => 34601.0,
                    8 => 38250.0,
                    10 => 42533.0,
                    _ => 37500.0,
                };
                poly(&regular(n, adj("adj", def) * 2.0, 0.5))
            }
            "pie" => {
                let a1 = adj("adj1", 0.0) * 100000.0 / 60000.0;
                let mut a2 = adj("adj2", 16_200_000.0) * 100000.0 / 60000.0;
                if a2 <= a1 {
                    a2 += 360.0;
                }
                let mut pts = vec![(0.5, 0.5)];
                for k in 0..=24 {
                    let t = (a1 + (a2 - a1) * k as f32 / 24.0).to_radians();
                    pts.push((0.5 + 0.5 * t.cos(), 0.5 + 0.5 * t.sin()));
                }
                poly(&pts)
            }
            "leftBracket" | "rightBracket" | "leftBrace" | "rightBrace" => {
                use PathCmd::*;
                let right = prst.starts_with("right");
                let (o, i) = if right { (0.0, 1.0) } else { (1.0, 0.0) };
                Geometry::Open(if prst.ends_with("Bracket") {
                    vec![Move(o, 0.0), Line(i, 0.0), Line(i, 1.0), Line(o, 1.0)]
                } else {
                    let m = (o + i) / 2.0;
                    vec![
                        Move(o, 0.0),
                        Cubic(m, 0.0, m, 0.0, m, 0.1),
                        Line(m, 0.4),
                        Cubic(m, 0.5, i, 0.5, i, 0.5),
                        Cubic(m, 0.5, m, 0.5, m, 0.6),
                        Line(m, 0.9),
                        Cubic(m, 1.0, m, 1.0, o, 1.0),
                    ]
                })
            }
            "actionButtonBlank" | "flowChartAlternateProcess2" => Geometry::Rect,
            other => match presets::outline(other, &raw, r) {
                Some(g) => g,
                None => {
                    self.warn(format!("preset shape '{other}' drawn as a rectangle"));
                    Geometry::Rect
                }
            },
        }
    }

    fn text_body(
        &mut self,
        tb: &Node,
        ph: Option<&PhKey>,
        lph: Option<&Node>,
        mph: Option<&Node>,
        font_ref: Option<Rgb>,
    ) -> Option<TextBody> {
        let cc = self.cc();
        // Base list style: master txStyles for placeholders, presentation
        // defaultTextStyle for everything else.
        let mut chain: Vec<ListStyle> = vec![match (ph, self.master) {
            (Some(k), Some(m)) => {
                let (title, body, other) = match self.restyled {
                    Some([t, b, o]) => (t, b, o),
                    None => (&m.title, &m.body, &m.other),
                };
                match master_kind(&k.kind) {
                    "title" => title.clone(),
                    "body" => body.clone(),
                    _ => other.clone(),
                }
            }
            _ => self.default_text.clone(),
        }];
        if let Some(c) = font_ref {
            let mut ls = ListStyle::default();
            for l in ls.0.iter_mut() {
                l.run.color = Some(c);
            }
            chain.push(ls);
        }
        let mut body = BodyProps::default();
        for src in [mph, lph].into_iter().flatten() {
            if let Some(t) = src.child("txBody") {
                if let Some(ls) = t.child("lstStyle") {
                    chain.push(ListStyle::parse(ls, &cc));
                }
                if let Some(bp) = t.child("bodyPr") {
                    body.overlay(&BodyProps::parse(bp));
                }
            }
        }
        if let Some(ls) = tb.child("lstStyle") {
            chain.push(ListStyle::parse(ls, &cc));
        }
        let bp = tb.child("bodyPr");
        if let Some(bp) = bp {
            body.overlay(&BodyProps::parse(bp));
        }
        let autofit = bp.and_then(|b| b.child("normAutofit"));
        let font_scale = autofit
            .and_then(|a| a.attr_i64("fontScale"))
            .map(|v| v as f32 / 100000.0)
            .unwrap_or(1.0);
        let line_reduction = autofit
            .and_then(|a| a.attr_i64("lnSpcReduction"))
            .map(|v| v as f32 / 100000.0)
            .unwrap_or(0.0);

        let mut paragraphs = Vec::new();
        let mut counters = [0u32; 9];
        let mut any_text = false;
        for p in tb.children_named("p") {
            let ppr = p.child("pPr");
            let lvl = ppr.and_then(|n| n.attr_i64("lvl")).unwrap_or(0).clamp(0, 8) as usize;
            let mut style = ParaStyle::default();
            for ls in &chain {
                style.overlay(&ls.0[lvl]);
            }
            if let Some(ppr) = ppr {
                style.overlay(&ParaStyle::parse(ppr, &cc));
            }
            let base_run = style.run.clone();
            let mut runs = Vec::new();
            for r in &p.children {
                let (text, line_break) = match r.name.as_str() {
                    "r" => (r.child("t").map(|t| t.text.clone()).unwrap_or_default(), false),
                    "fld" => {
                        let t = if r.attr("type") == Some("slidenum") {
                            self.deck_position.to_string()
                        } else {
                            r.child("t").map(|t| t.text.clone()).unwrap_or_default()
                        };
                        (t, false)
                    }
                    "br" => (String::new(), true),
                    _ => continue,
                };
                let mut rs = base_run.clone();
                if let Some(rpr) = r.child("rPr") {
                    rs.overlay(&RunStyle::parse(rpr, &cc));
                }
                runs.push(self.make_run(text, &rs, line_break));
            }
            let mut end = base_run.clone();
            if let Some(e) = p.child("endParaRPr") {
                end.overlay(&RunStyle::parse(e, &cc));
            }
            let end_size = end.size.unwrap_or(18.0);
            let has_text = runs.iter().any(|r| !r.text.trim().is_empty());
            any_text |= has_text;

            let bullet = if !has_text {
                None
            } else {
                match &style.bullet {
                    Some(BulletKind::Char(c)) => {
                        let text = if style.bullet_symbol_font.unwrap_or(false) {
                            "•".to_string()
                        } else {
                            c.clone()
                        };
                        Some(Bullet {
                            text,
                            color: style.bullet_color,
                            scale: style.bullet_scale.unwrap_or(1.0),
                        })
                    }
                    Some(BulletKind::AutoNum(kind, start)) => {
                        counters[lvl] = counters[lvl].saturating_add(1).min(100_000);
                        let n = start.saturating_add(counters[lvl] - 1);
                        Some(Bullet {
                            text: autonum(kind, n),
                            color: style.bullet_color,
                            scale: style.bullet_scale.unwrap_or(1.0),
                        })
                    }
                    _ => None,
                }
            };
            if !matches!(style.bullet, Some(BulletKind::AutoNum(..))) {
                counters[lvl] = 0;
            }
            // A paragraph at level L restarts the numbering of deeper levels.
            if has_text {
                counters[lvl + 1..].iter_mut().for_each(|c| *c = 0);
            }
            let first_size = runs.iter().find(|r| !r.line_break).map(|r| r.size).unwrap_or(end_size);
            let sp = |s: Option<Spacing>| match s {
                Some(Spacing::Pts(p)) => p,
                Some(Spacing::Pct(f)) => f * first_size * 1.2,
                None => 0.0,
            };
            let (line_spacing, line_spacing_pt) = match style.line {
                Some(Spacing::Pct(f)) => (f, None),
                Some(Spacing::Pts(p)) => (1.0, Some(p)),
                None => (1.0, None),
            };
            paragraphs.push(Paragraph {
                align: style.align.unwrap_or(Align::Left),
                margin_left: style.margin_left.unwrap_or(0.0),
                indent: style.indent.unwrap_or(0.0),
                bullet,
                line_spacing,
                line_spacing_pt,
                space_before: sp(style.before),
                space_after: sp(style.after),
                runs,
                end_size,
            });
        }
        if !any_text {
            return None;
        }
        Some(TextBody {
            insets: [
                body.insets[0].unwrap_or(91440.0),
                body.insets[1].unwrap_or(45720.0),
                body.insets[2].unwrap_or(91440.0),
                body.insets[3].unwrap_or(45720.0),
            ],
            rotation: body.vert.unwrap_or(0.0),
            anchor: body.anchor.unwrap_or(Anchor::Top),
            wrap: body.wrap.unwrap_or(true),
            font_scale,
            line_reduction,
            paragraphs,
        })
    }

    fn make_run(&self, text: String, rs: &RunStyle, line_break: bool) -> Run {
        let face = match rs.font.as_deref() {
            Some(f) if f.starts_with("+mj") => self.theme.major.clone(),
            Some(f) if f.starts_with("+mn") => self.theme.minor.clone(),
            Some(f) => f.to_string(),
            None => self.theme.minor.clone(),
        };
        Run {
            text: if rs.caps.unwrap_or(false) { text.to_uppercase() } else { text },
            size: rs.size.unwrap_or(18.0),
            bold: rs.bold.unwrap_or(false),
            italic: rs.italic.unwrap_or(false),
            underline: rs.underline.unwrap_or(false),
            color: rs.color.unwrap_or(Rgb::BLACK),
            family: family_of(&face),
            face,
            line_break,
        }
    }

    fn graphic_frame(&mut self, n: &Node, part: &Part, xf: Xform) {
        let Some(data) = n.path(&["graphic", "graphicData"]) else { return };
        let uri = data.attr("uri").unwrap_or("");
        if let Some(tbl) = data.child("tbl") {
            let Some(x) = n.child("xfrm").and_then(parse_xfrm) else { return };
            let first = self.elements.len();
            self.table(tbl, part, xf.rect(x.rect));
            xf.turn_elements(&mut self.elements[first..]);
        } else if uri.ends_with("/ole") {
            // Embedded documents (Excel, Word, think-cell...) carry a preview
            // picture; draw it in the frame.
            let pic = data.find("pic");
            let rid = pic.and_then(|p| p.path(&["blipFill", "blip"])).and_then(|b| b.attr("embed"));
            match (rid, n.child("xfrm").and_then(parse_xfrm)) {
                (Some(rid), Some(x)) => {
                    if self.metafile(part, rid, x.rect, xf) {
                        return;
                    }
                    let placed = xf.place(x.rect, x.rot, false, false);
                    if let Some(media) = self.media(part, rid) {
                        self.elements.push(Element::Picture(Picture {
                            rect: placed.rect,
                            rotation: placed.rotation,
                            media,
                            crop: [0.0; 4],
                            flip_h: placed.flip_h,
                            flip_v: placed.flip_v,
                        }));
                    }
                }
                _ => self.warn("embedded object skipped (no preview picture)"),
            }
        } else if uri.ends_with("/chart") {
            let Some(x) = n.child("xfrm").and_then(parse_xfrm) else { return };
            let first = self.elements.len();
            if self.chart(data, part, xf.rect(x.rect)) {
                xf.turn_elements(&mut self.elements[first..]);
            } else {
                self.warn("chart skipped (no cached data)");
            }
        } else if uri.ends_with("/diagram") {
            let Some(x) = n.child("xfrm").and_then(parse_xfrm) else { return };
            if !self.smartart(data, part, xf, x.rect) {
                self.warn("SmartArt skipped (no saved drawing)");
            }
        } else {
            self.warn(format!("graphic frame '{uri}' skipped"));
        }
    }

    /// EMF/WMF pictures are replayed into vector shapes. Returns true when
    /// `rid` is a metafile (drawn, or skipped with a warning). `frame` is the
    /// picture's rect in the child space of `xf`.
    fn metafile(&mut self, part: &Part, rid: &str, frame: Rect, xf: Xform) -> bool {
        let Some(target) = part.rels.get(rid).map(|r| r.target.clone()) else {
            return false;
        };
        let lower = target.to_ascii_lowercase();
        if !(lower.ends_with(".emf") || lower.ends_with(".wmf")) {
            return false;
        }
        let Some(bytes) = self.pkg.read_media(&target) else {
            self.warn(format!("{target}: missing or too large, skipped"));
            return true;
        };
        match crate::metafile::render(&bytes, xf.rect(frame)) {
            Ok((elements, bitmaps)) => {
                if elements.is_empty() {
                    self.warn(format!(
                        "{target}: metafile has no drawable content we support (EMF+ only?), skipped"
                    ));
                    return true;
                }
                // Bitmaps live in memory until rendering; keep them bounded.
                let mut used: usize = self.embedded.values().map(|v| v.len()).sum();
                for (i, png) in bitmaps.into_iter().enumerate() {
                    if used + png.len() > 64 << 20 {
                        self.warn(format!("{target}: too many embedded bitmaps, some were skipped"));
                        break;
                    }
                    used += png.len();
                    self.embedded.insert(format!("{target}#bitmap{i}"), png);
                }
                let first = self.elements.len();
                for mut e in elements {
                    if let Element::Picture(p) = &mut e {
                        p.media = format!("{target}{}", p.media);
                        if !self.embedded.contains_key(&p.media) {
                            continue;
                        }
                        self.deck_media.insert(p.media.clone());
                    }
                    self.elements.push(e);
                }
                xf.turn_elements(&mut self.elements[first..]);
            }
            Err(e) => self.warn(format!("{target}: {e}, skipped")),
        }
        true
    }

    /// Charts are redrawn from the values cached in the chart part.
    fn chart(&mut self, data: &Node, part: &Part, frame: Rect) -> bool {
        let Some(path) = data
            .child("chart")
            .and_then(|c| c.attr("r:id"))
            .and_then(|id| part.rels.get(id))
            .map(|r| r.target.clone())
        else {
            return false;
        };
        let Some(root) = self.pkg.xml(&path) else { return false };
        let cc = self.cc();
        let Some(chart) = crate::chart::parse(&root, &|fill: &Node| cc.color_in(fill)) else {
            return false;
        };
        // The chart's own background (solid, gradient, picture or texture).
        if let Some(sp) = root.child("spPr") {
            let rels = self.pkg.rels(&path);
            let chart_part = Part {
                root: Node::default(),
                rels,
            };
            if let Some(FillSpec::Some(fill)) = self.fill(sp, &chart_part, &cc) {
                self.elements.push(Element::Shape(Shape {
                    rect: frame,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                    geometry: Geometry::Rect,
                    fill: Some(fill),
                    stroke: None,
                    text: None,
                    is_title: false,
                    text_rect: None,
                }));
            }
        }
        for note in &chart.notes {
            self.warn(note.clone());
        }
        let accents: Vec<Rgb> = (1..=6)
            .filter_map(|i| self.theme.colors.get(&format!("accent{i}")).copied())
            .collect();
        let style = crate::chart::Style {
            accents: &accents,
            face: &self.theme.minor,
        };
        let elements = crate::chart::layout(&chart, frame, &style);
        self.elements.extend(elements);
        true
    }

    /// SmartArt: PowerPoint saves a pre-laid-out copy (`diagrams/drawingN.xml`)
    /// of every diagram; other writers keep only the data model, which is laid
    /// out here. False when neither is available.
    fn smartart(&mut self, data: &Node, part: &Part, xf: Xform, frame: Rect) -> bool {
        if self.in_diagram {
            return false;
        }
        self.smartart_drawing(data, part, xf, frame) || self.smartart_laid_out(data, part, xf, frame)
    }

    /// The saved drawing (`diagrams/drawingN.xml`): ordinary shapes,
    /// positioned relative to the graphic frame.
    fn smartart_drawing(&mut self, data: &Node, part: &Part, xf: Xform, frame: Rect) -> bool {
        // data part -> dataModelExt relId -> drawing (a relationship of the slide).
        let dm = data
            .child("relIds")
            .and_then(|r| r.attr("r:dm"))
            .and_then(|id| part.rels.get(id))
            .map(|r| r.target.clone());
        let via_data = dm
            .and_then(|p| self.pkg.xml(&p))
            .and_then(|d| d.find("dataModelExt").and_then(|e| e.attr("relId")).map(String::from))
            .and_then(|id| part.rels.get(&id).map(|r| r.target.clone()));
        let drawings: Vec<&Rel> = part.rels.values().filter(|r| r.kind == "diagramDrawing").collect();
        let path = via_data.or_else(|| (drawings.len() == 1).then(|| drawings[0].target.clone()));
        let Some(path) = path else { return false };
        let Some(root) = self.pkg.xml(&path) else { return false };
        let rels = self.pkg.rels(&path);
        let drawing = Part { root, rels };
        let Some(tree) = drawing.root.child("spTree") else { return false };
        self.in_diagram = true;
        self.walk(&tree.children, &drawing, xf.shifted(frame.x, frame.y), false);
        self.in_diagram = false;
        true
    }

    /// A diagram with no saved drawing, laid out from its data model by
    /// [`smartart`]: a layout family picked from the layout's name, the
    /// accent colors of the diagram's color part, and each node's text fitted
    /// to its box. False when the frame has no readable data part.
    fn smartart_laid_out(&mut self, data: &Node, part: &Part, xf: Xform, frame: Rect) -> bool {
        let rel = |key: &str| {
            data.child("relIds")
                .and_then(|r| r.attr(key))
                .and_then(|id| part.rels.get(id))
                .map(|r| r.target.clone())
        };
        let Some(root) = rel("r:dm").and_then(|p| self.pkg.xml(&p)) else {
            return false;
        };
        let model = smartart::Model::parse(&root);
        if model.roots.is_empty() {
            // An empty diagram has nothing to lose.
            return true;
        }
        let layout_id = match &model.layout_id {
            Some(id) => id.clone(),
            None => rel("r:lo")
                .and_then(|p| self.pkg.xml(&p))
                .and_then(|l| l.attr("uniqueId").map(String::from))
                .unwrap_or_default(),
        };
        let colors = self.smartart_colors(rel("r:cs"));
        let pieces = smartart::layout(&model, smartart::family_of(&layout_id), frame.w, frame.h);
        let dark = self.theme.colors.get("dk1").copied().unwrap_or(Rgb::BLACK);
        let pts: &[Node] = root.child("ptLst").map(|l| l.children.as_slice()).unwrap_or(&[]);
        let to_slide = |r: Rect| {
            xf.rect(Rect {
                x: frame.x + r.x,
                y: frame.y + r.y,
                ..r
            })
        };

        let first = self.elements.len();
        // (element index, text, author chose the size, role) of every label.
        let mut texts: Vec<(usize, TextBody, bool, smartart::Role)> = Vec::new();
        for piece in &pieces {
            let base = colors[piece.color % colors.len()];
            let (fill, stroke) = match piece.role {
                smartart::Role::Primary => (
                    Some(Fill::Solid(base)),
                    Some(Stroke {
                        color: Rgb::WHITE,
                        width: 12700.0,
                    }),
                ),
                smartart::Role::Secondary => (
                    Some(Fill::Solid(mix_with_white(base, 0.2))),
                    Some(Stroke {
                        color: base,
                        width: 12700.0,
                    }),
                ),
                smartart::Role::Arrow => (Some(Fill::Solid(mix_with_white(base, 0.6))), None),
                smartart::Role::Link => (
                    None,
                    Some(Stroke {
                        color: colors[0],
                        width: 12700.0,
                    }),
                ),
            };
            let mut text_rect = None;
            if let Some(label) = &piece.label {
                let at = to_slide(label.rect);
                text_rect = Some(at);
                // Light text on a dark accent, dark text on a pale one.
                let on = match piece.role {
                    smartart::Role::Primary if luminance(base) < 0.6 => Rgb::WHITE,
                    smartart::Role::Primary => dark,
                    _ => dark,
                };
                if let Some((body, explicit)) = self.smartart_text(pts, &model, label, on, at) {
                    texts.push((self.elements.len(), body, explicit, piece.role));
                }
            }
            self.elements.push(Element::Shape(Shape {
                rect: to_slide(piece.rect),
                rotation: piece.rotation,
                flip_h: piece.flip_h,
                flip_v: piece.flip_v,
                geometry: piece.geometry.clone(),
                fill,
                stroke,
                text: None,
                is_title: false,
                text_rect,
            }));
        }
        // One size for all the nodes of a kind, as PowerPoint does; text with
        // a size of its own only shrinks to fit.
        for role in [smartart::Role::Primary, smartart::Role::Secondary] {
            let fitted: Vec<(usize, f32, bool)> = texts
                .iter()
                .filter(|t| t.3 == role)
                .map(|(i, body, explicit, _)| {
                    let Some(Element::Shape(s)) = self.elements.get(*i) else {
                        return (*i, 1.0, *explicit);
                    };
                    let at = s.text_rect.unwrap_or(s.rect);
                    let size = body
                        .paragraphs
                        .iter()
                        .flat_map(|p| p.runs.iter().map(|r| r.size))
                        .fold(0.0, f32::max);
                    (
                        *i,
                        smartart::fit_scale(body, at.w, at.h, smartart::max_scale(size, *explicit)),
                        *explicit,
                    )
                })
                .collect();
            let shared = fitted.iter().filter(|f| !f.2).map(|f| f.1).fold(f32::INFINITY, f32::min);
            for (i, own, explicit) in fitted {
                if let Some(t) = texts.iter_mut().find(|t| t.0 == i) {
                    t.1.font_scale = if explicit { own } else { shared };
                }
            }
        }
        for (i, mut body, ..) in texts {
            smartart::finish_bullets(&mut body);
            if let Some(Element::Shape(s)) = self.elements.get_mut(i) {
                s.text = Some(body);
            }
        }
        xf.turn_elements(&mut self.elements[first..]);
        self.warn("SmartArt laid out by slidepdf, not PowerPoint");
        if model.truncated {
            self.warn(format!(
                "SmartArt has more than {} nodes or {} levels; the rest was left out",
                smartart::MAX_NODES,
                smartart::MAX_DEPTH
            ));
        }
        true
    }

    /// The fill colors of a diagram's color part (`styleLbl node1`), or the
    /// theme's first accent.
    fn smartart_colors(&mut self, part: Option<String>) -> Vec<Rgb> {
        let cc = self.cc();
        let listed = part.and_then(|p| self.pkg.xml(&p)).and_then(|root| {
            let label = ["node1", "node0"]
                .iter()
                .find_map(|n| root.children_named("styleLbl").find(|s| s.attr("name") == Some(n)))?;
            let colors: Vec<Rgb> = label.child("fillClrLst")?.children.iter().filter_map(|c| cc.color(c)).collect();
            (!colors.is_empty()).then_some(colors)
        });
        listed.unwrap_or_else(|| vec![self.theme.colors.get("accent1").copied().unwrap_or(Rgb(68, 114, 196))])
    }

    /// The text of a label: the heading centred, the bullets as a list,
    /// anchored in the middle of `at`. The size is set by the caller.
    fn smartart_text(
        &mut self,
        pts: &[Node],
        model: &smartart::Model,
        label: &smartart::Label,
        color: Rgb,
        at: Rect,
    ) -> Option<(TextBody, bool)> {
        let mut merged: Option<TextBody> = None;
        let mut explicit = false;
        let headings = label.heading.iter().count();
        for (k, &n) in label.heading.iter().chain(&label.bullets).enumerate() {
            let Some(t) = pts.get(model.nodes[n].pt).and_then(|p| p.child("t")) else {
                continue;
            };
            let Some(mut body) = self.text_body(t, None, None, None, Some(color)) else {
                continue;
            };
            explicit |= smartart::has_explicit_size(t);
            for p in &mut body.paragraphs {
                if k < headings {
                    p.align = Align::Center;
                } else {
                    p.align = Align::Left;
                    if p.runs.iter().any(|r| !r.text.trim().is_empty()) {
                        p.bullet = Some(Bullet {
                            text: "\u{2022}".into(),
                            color: None,
                            scale: 1.0,
                        });
                    }
                }
            }
            match &mut merged {
                None => merged = Some(body),
                Some(m) => m.paragraphs.extend(body.paragraphs),
            }
        }
        let mut body = merged?;
        let inset = (at.w.min(at.h) * 0.06).clamp(20_000.0, 120_000.0);
        body.insets = [inset; 4];
        body.anchor = Anchor::Middle;
        body.wrap = true;
        body.rotation = 0.0;
        body.line_reduction = 0.0;
        Some((body, explicit))
    }

    /// Tables become one rectangle shape per cell. Explicit cell fills win;
    /// otherwise a "Medium Style 2" look (the PowerPoint default) is applied.
    fn table(&mut self, tbl: &Node, part: &Part, rect: Rect) {
        let cc = self.cc();
        let cols: Vec<f32> = tbl
            .path(&["tblGrid"])
            .map(|g| g.children_named("gridCol").map(|c| c.attr_i64("w").unwrap_or(0) as f32).collect())
            .unwrap_or_default();
        let total_w: f32 = cols.iter().sum();
        let sx = if total_w > 0.0 { rect.w / total_w } else { 1.0 };
        let pr = tbl.child("tblPr");
        let first_row = pr.and_then(|p| p.attr_bool("firstRow")).unwrap_or(false);
        let band_row = pr.and_then(|p| p.attr_bool("bandRow")).unwrap_or(false);
        let styled = pr.and_then(|p| p.child("tableStyleId")).is_some();
        let flag = |k: &str| pr.and_then(|p| p.attr_bool(k)).unwrap_or(false);
        let (first_col, last_col, last_row, band_col) = (flag("firstCol"), flag("lastCol"), flag("lastRow"), flag("bandCol"));
        // A style defined in the deck; built-in Office styles that are not
        // written to tableStyles.xml fall back to the default look below.
        let defined = pr
            .and_then(|p| p.child("tableStyleId"))
            .and_then(|n| self.table_styles.get(n.text.trim()))
            .cloned();
        let n_rows = tbl.children_named("tr").count();
        // Table background (tblBg) under all cells.
        if let Some(bg) = defined.as_ref().and_then(|s| s.child("tblBg")) {
            let fill_ref = bg.child("fillRef");
            let fill = match fill_ref.and_then(|r| cc.color_in(r)) {
                Some(c) => self
                    .theme_fill(fill_ref.and_then(|r| r.attr_i64("idx")).unwrap_or(0), c, part)
                    .or(Some(Fill::Solid(c))),
                None => match self.fill(bg, part, &cc) {
                    Some(FillSpec::Some(f)) => Some(f),
                    _ => None,
                },
            };
            if fill.is_some() {
                let h: f32 = tbl.children_named("tr").map(|r| r.attr_i64("h").unwrap_or(370840) as f32).sum();
                self.elements.push(Element::Shape(Shape {
                    rect: Rect { h, ..rect },
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                    geometry: Geometry::Rect,
                    fill,
                    stroke: None,
                    text: None,
                    is_title: false,
                    text_rect: None,
                }));
            }
        }
        let accent = self.theme.colors.get("accent1").copied().unwrap_or(Rgb(68, 114, 196));
        let tint = |c: Rgb, t: f32| {
            let (r, g, b) = c.floats();
            to_rgb(r * t + 1.0 - t, g * t + 1.0 - t, b * t + 1.0 - t)
        };

        let row_h: Vec<f32> = tbl.children_named("tr").map(|r| r.attr_i64("h").unwrap_or(370840) as f32).collect();
        let mut y = rect.y;
        for (ri, tr) in tbl.children_named("tr").enumerate() {
            let header = first_row && ri == 0;
            let n_cols = tr.children_named("tc").count();
            for (ci, tc) in tr.children_named("tc").enumerate() {
                // One tc per grid column (merged ones are hMerge/vMerge
                // placeholders), so the position comes from the grid prefix.
                let span = tc.attr_i64("gridSpan").unwrap_or(1).clamp(1, 1000) as usize;
                let row_span = tc.attr_i64("rowSpan").unwrap_or(1).clamp(1, 1000) as usize;
                let x = rect.x + cols.iter().take(ci).sum::<f32>() * sx;
                let w: f32 = cols.iter().skip(ci).take(span).sum::<f32>() * sx;
                let w = if w > 0.0 {
                    w
                } else {
                    rect.w / tr.children_named("tc").count().max(1) as f32
                };
                if tc.attr_bool("hMerge").unwrap_or(false) || tc.attr_bool("vMerge").unwrap_or(false) {
                    continue;
                }
                let h: f32 = row_h.iter().skip(ri).take(row_span).sum();
                let tcpr = tc.child("tcPr");
                let explicit = tcpr.and_then(|p| self.fill(p, part, &cc));
                let mut style_fill: Option<Option<Fill>> = None;
                let mut style_text: Option<Rgb> = None;
                let mut style_bold = false;
                let mut style_border: Option<Option<Stroke>> = None;
                if let Some(st) = &defined {
                    // Precedence, lowest first, as in the spec.
                    let data_row = ri.saturating_sub(first_row as usize);
                    let mut parts = vec!["wholeTbl"];
                    if band_row && !(header || (last_row && ri + 1 == n_rows)) {
                        parts.push(if data_row % 2 == 0 { "band1H" } else { "band2H" });
                    }
                    if band_col {
                        parts.push(if ci % 2 == 0 { "band1V" } else { "band2V" });
                    }
                    if first_col && ci == 0 {
                        parts.push("firstCol");
                    }
                    if last_col && ci + 1 == n_cols {
                        parts.push("lastCol");
                    }
                    if last_row && ri + 1 == n_rows {
                        parts.push("lastRow");
                    }
                    if header {
                        parts.push("firstRow");
                    }
                    for name in parts {
                        let Some(p) = st.child(name) else { continue };
                        if let Some(ts) = p.child("tcStyle") {
                            if let Some(f) = ts.child("fill") {
                                match self.fill(f, part, &cc) {
                                    Some(FillSpec::Some(x)) => style_fill = Some(Some(x)),
                                    Some(FillSpec::None) => style_fill = Some(None),
                                    None => {}
                                }
                            } else if let Some(c) = ts.child("fillRef").and_then(|r| cc.color_in(r)) {
                                style_fill = Some(Some(Fill::Solid(c)));
                            }
                            if let Some(ln) = ts
                                .child("tcBdr")
                                .and_then(|b| b.child("insideH").or_else(|| b.child("bottom")).or_else(|| b.child("left")))
                                .and_then(|e| e.child("ln"))
                            {
                                style_border = Some(ln.child("solidFill").and_then(|f| cc.color_in(f)).map(|color| Stroke {
                                    color,
                                    width: ln.attr_i64("w").unwrap_or(12700) as f32,
                                }));
                            }
                        }
                        if let Some(tx) = p.child("tcTxStyle") {
                            if let Some(c) = tx.children.iter().find_map(|c| cc.color(c)) {
                                style_text = Some(c);
                            }
                            if tx.attr("b") == Some("on") {
                                style_bold = true;
                            }
                        }
                    }
                }
                let fill = match explicit {
                    Some(FillSpec::Some(f)) => Some(f),
                    Some(FillSpec::None) => None,
                    None if defined.is_some() => style_fill.flatten(),
                    None if styled && header => Some(Fill::Solid(accent)),
                    None if styled => Some(Fill::Solid(tint(accent, if band_row && ri % 2 == 1 { 0.4 } else { 0.2 }))),
                    None => None,
                };
                let bold = if defined.is_some() { style_bold } else { styled && header };
                let header_color = if defined.is_some() {
                    style_text
                } else if styled && header {
                    Some(Rgb::WHITE)
                } else {
                    None
                };
                let mut text = tc.child("txBody").and_then(|t| self.text_body(t, None, None, None, header_color));
                if let (Some(t), true) = (&mut text, bold) {
                    for p in &mut t.paragraphs {
                        for r in &mut p.runs {
                            r.bold = true;
                        }
                    }
                }
                if let (Some(t), Some(p)) = (&mut text, tcpr) {
                    let m = |k, d| p.attr_i64(k).map(|v| v as f32).unwrap_or(d);
                    t.insets = [m("marL", 91440.0), m("marT", 45720.0), m("marR", 91440.0), m("marB", 45720.0)];
                    if let Some(a) = p.attr("anchor") {
                        t.anchor = match a {
                            "ctr" => Anchor::Middle,
                            "b" => Anchor::Bottom,
                            _ => Anchor::Top,
                        };
                    }
                }
                self.elements.push(Element::Shape(Shape {
                    rect: Rect { x, y, w, h },
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                    geometry: Geometry::Rect,
                    fill,
                    stroke: match style_border {
                        Some(b) if defined.is_some() => b,
                        _ if defined.is_some() => None,
                        _ => Some(Stroke {
                            color: if styled { Rgb::WHITE } else { Rgb::BLACK },
                            width: 12700.0,
                        }),
                    },
                    text,
                    is_title: false,
                    text_rect: None,
                }));
            }
            y += row_h[ri];
        }
    }
}

fn custom_geometry(cg: &Node) -> Option<Geometry> {
    let mut cmds = Vec::new();
    for path in cg.path(&["pathLst"])?.children_named("path") {
        let w = path.attr_i64("w").unwrap_or(1).max(1) as f32;
        let h = path.attr_i64("h").unwrap_or(1).max(1) as f32;
        let pt = |n: &Node| (n.attr_i64("x").unwrap_or(0) as f32 / w, n.attr_i64("y").unwrap_or(0) as f32 / h);
        let mut last = (0.0, 0.0);
        for c in &path.children {
            let pts: Vec<(f32, f32)> = c.children_named("pt").map(pt).collect();
            match (c.name.as_str(), pts.as_slice()) {
                ("moveTo", [p, ..]) => {
                    cmds.push(PathCmd::Move(p.0, p.1));
                    last = *p;
                }
                ("lnTo", [p, ..]) => {
                    cmds.push(PathCmd::Line(p.0, p.1));
                    last = *p;
                }
                ("cubicBezTo", [a, b, p, ..]) => {
                    cmds.push(PathCmd::Cubic(a.0, a.1, b.0, b.1, p.0, p.1));
                    last = *p;
                }
                ("quadBezTo", [q, p, ..]) => {
                    let c1 = (last.0 + 2.0 / 3.0 * (q.0 - last.0), last.1 + 2.0 / 3.0 * (q.1 - last.1));
                    let c2 = (p.0 + 2.0 / 3.0 * (q.0 - p.0), p.1 + 2.0 / 3.0 * (q.1 - p.1));
                    cmds.push(PathCmd::Cubic(c1.0, c1.1, c2.0, c2.1, p.0, p.1));
                    last = *p;
                }
                ("close", _) => cmds.push(PathCmd::Close),
                _ => {}
            }
        }
    }
    if cmds.is_empty() {
        None
    } else {
        Some(Geometry::Path(cmds))
    }
}

/// Points of a regular polygon (or a star when `inner < 1`), unit box,
/// first vertex at the top. `inner` is the inner radius ratio for stars.
fn regular(n: usize, inner: f32, star: f32) -> Vec<(f32, f32)> {
    let steps = if star > 0.0 { n * 2 } else { n };
    (0..steps)
        .map(|k| {
            let r = if star > 0.0 && k % 2 == 1 { 0.5 * inner } else { 0.5 };
            let t = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / steps as f32;
            (0.5 + r * t.cos(), 0.5 + r * t.sin())
        })
        .collect()
}

fn family_of(face: &str) -> Family {
    let f = face.to_ascii_lowercase();
    const SERIF: [&str; 10] = [
        "times",
        "georgia",
        "garamond",
        "cambria",
        "book",
        "serif",
        "palatino",
        "baskerville",
        "didot",
        "constantia",
    ];
    const MONO: [&str; 5] = ["courier", "consolas", "mono", "menlo", "lucida console"];
    if MONO.iter().any(|m| f.contains(m)) {
        Family::Mono
    } else if SERIF.iter().any(|s| f.contains(s)) && !f.contains("sans") {
        Family::Serif
    } else {
        Family::Sans
    }
}

fn autonum(kind: &str, n: u32) -> String {
    let alpha = |n: u32| {
        let mut s = String::new();
        let mut n = n;
        while n > 0 {
            n -= 1;
            s.insert(0, (b'a' + (n % 26) as u8) as char);
            n /= 26;
        }
        s
    };
    let roman = |mut n: u32| {
        let table = [
            (1000, "m"),
            (900, "cm"),
            (500, "d"),
            (400, "cd"),
            (100, "c"),
            (90, "xc"),
            (50, "l"),
            (40, "xl"),
            (10, "x"),
            (9, "ix"),
            (5, "v"),
            (4, "iv"),
            (1, "i"),
        ];
        let mut s = String::new();
        for (v, r) in table {
            while n >= v {
                s.push_str(r);
                n -= v;
            }
        }
        s
    };
    let body = if kind.starts_with("alphaLc") {
        alpha(n)
    } else if kind.starts_with("alphaUc") {
        alpha(n).to_uppercase()
    } else if kind.starts_with("romanLc") {
        roman(n)
    } else if kind.starts_with("romanUc") {
        roman(n).to_uppercase()
    } else {
        n.to_string()
    };
    if kind.ends_with("ParenBoth") {
        format!("({body})")
    } else if kind.ends_with("ParenR") {
        format!("{body})")
    } else if kind.ends_with("Plain") {
        body
    } else {
        format!("{body}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_targets() {
        assert_eq!(resolve_path("ppt/slides", "../media/image1.png"), "ppt/media/image1.png");
        assert_eq!(resolve_path("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_path("ppt/slides", "/ppt/media/a.jpg"), "ppt/media/a.jpg");
    }

    #[test]
    fn autonumbering() {
        assert_eq!(autonum("arabicPeriod", 3), "3.");
        assert_eq!(autonum("alphaLcParenR", 28), "ab)");
        assert_eq!(autonum("romanUcPeriod", 14), "XIV.");
    }

    #[test]
    fn luminance_modifiers() {
        let n = xml::parse(br#"<a:schemeClr val="accent1"><a:lumMod val="50000"/></a:schemeClr>"#).unwrap();
        let mut theme = Theme::default();
        theme.colors.insert("accent1".into(), Rgb(0xff, 0, 0));
        let map = HashMap::new();
        let cc = ColorCtx {
            theme: &theme,
            map: &map,
            ph: None,
        };
        assert_eq!(cc.color(&n), Some(Rgb(128, 0, 0)));
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    fn assert_placed(p: Placed, rect: [f32; 4], rotation: f32, flips: (bool, bool)) {
        let got = [p.rect.x, p.rect.y, p.rect.w, p.rect.h];
        assert!(got.iter().zip(rect).all(|(a, b)| close(*a, b)), "rect {got:?} != {rect:?}");
        assert!(close(p.rotation, rotation), "rotation {} != {rotation}", p.rotation);
        assert_eq!((p.flip_h, p.flip_v), flips);
    }

    fn group(parent: &Xform, attrs: &str, off: (i64, i64), ext: (i64, i64), ch: (i64, i64, i64, i64)) -> Xform {
        let xml = format!(
            r#"<p:xfrm {attrs}><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/><a:chOff x="{}" y="{}"/><a:chExt cx="{}" cy="{}"/></p:xfrm>"#,
            off.0, off.1, ext.0, ext.1, ch.0, ch.1, ch.2, ch.3
        );
        group_xform(parent, &xml::parse(xml.as_bytes()).unwrap()).unwrap()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn plain_group_scales_and_moves() {
        let g = group(&Xform::ID, "", (1000, 500), (4000, 2000), (0, 0, 2000, 1000));
        let p = g.place(rect(100.0, 100.0, 200.0, 50.0), 30.0, true, false);
        assert_placed(p, [1200.0, 700.0, 400.0, 100.0], 30.0, (true, false));
    }

    #[test]
    fn rotated_group_turns_children_around_its_centre() {
        // 90 degrees clockwise: the child in the top-left corner of the group
        // ends up in the top-right corner of the turned box.
        let g = group(&Xform::ID, r#"rot="5400000""#, (1000, 1000), (2000, 1000), (0, 0, 2000, 1000));
        let p = g.place(rect(0.0, 0.0, 1000.0, 500.0), 0.0, false, false);
        assert_placed(p, [1750.0, 750.0, 1000.0, 500.0], 90.0, (false, false));
    }

    #[test]
    fn rotated_group_adds_to_the_childs_own_rotation_and_flip() {
        let g = group(&Xform::ID, r#"rot="5400000""#, (1000, 1000), (2000, 1000), (0, 0, 2000, 1000));
        let p = g.place(rect(0.0, 0.0, 1000.0, 500.0), 30.0, true, false);
        assert_placed(p, [1750.0, 750.0, 1000.0, 500.0], 120.0, (true, false));
    }

    #[test]
    fn flipped_group_mirrors_children_and_reverses_their_rotation() {
        // flipH about the group centre (x = 1000): a child at the left edge
        // moves to the right edge; a child turned 30 degrees now leans the other way.
        let g = group(&Xform::ID, r#"flipH="1""#, (0, 0), (2000, 1000), (0, 0, 2000, 1000));
        let p = g.place(rect(0.0, 0.0, 500.0, 500.0), 30.0, false, false);
        assert_placed(p, [1500.0, 0.0, 500.0, 500.0], 330.0, (true, false));
        // Flipping an already flipped child cancels out.
        let p = g.place(rect(0.0, 0.0, 500.0, 500.0), 0.0, true, true);
        assert_placed(p, [1500.0, 0.0, 500.0, 500.0], 0.0, (false, true));
    }

    #[test]
    fn rotated_and_flipped_group_with_flipped_child() {
        // flipH then 90 degrees clockwise, around the centre (2000, 1500).
        let g = group(
            &Xform::ID,
            r#"rot="5400000" flipH="1""#,
            (1000, 1000),
            (2000, 1000),
            (0, 0, 2000, 1000),
        );
        let p = g.place(rect(0.0, 0.0, 1000.0, 500.0), 30.0, true, false);
        // The child centre sits (-500, -250) from the pivot; flipH gives (500, -250),
        // the quarter turn (250, 500), so the centre lands at (2250, 2000). A mirrored
        // group reverses the child's 30 degrees (90 - 30), and the two flips cancel.
        assert_placed(p, [1750.0, 1750.0, 1000.0, 500.0], 60.0, (false, false));
    }

    #[test]
    fn nested_groups_compose_scale_and_turn() {
        // The outer group doubles its children; the inner one (500,0 1000x1000
        // in outer child units) turns 90 degrees.
        let outer = group(&Xform::ID, "", (0, 0), (4000, 2000), (0, 0, 2000, 1000));
        let inner = group(&outer, r#"rot="5400000""#, (500, 0), (1000, 1000), (0, 0, 100, 100));
        assert!(close(inner.sx, 20.0) && close(inner.sy, 20.0));
        let p = inner.place(rect(0.0, 0.0, 50.0, 50.0), 0.0, false, false);
        assert_placed(p, [2000.0, 0.0, 1000.0, 1000.0], 90.0, (false, false));
    }

    #[test]
    fn nested_rotations_add_up() {
        let outer = group(&Xform::ID, r#"rot="5400000""#, (0, 0), (2000, 2000), (0, 0, 2000, 2000));
        let inner = group(&outer, r#"rot="5400000""#, (0, 0), (1000, 1000), (0, 0, 1000, 1000));
        let p = inner.place(rect(0.0, 0.0, 500.0, 500.0), 0.0, false, false);
        assert_placed(p, [1500.0, 500.0, 500.0, 500.0], 180.0, (false, false));
    }

    #[test]
    fn group_without_child_extent_keeps_the_parent_scale() {
        let outer = group(&Xform::ID, "", (0, 0), (4000, 2000), (0, 0, 2000, 1000));
        let inner = group(&outer, "", (100, 100), (500, 500), (0, 0, 0, 0));
        assert!(close(inner.sx, 2.0) && close(inner.sy, 2.0));
    }

    #[test]
    fn text_box_keeps_its_offset_from_a_turned_shape() {
        let g = group(&Xform::ID, r#"rot="5400000""#, (0, 0), (2000, 2000), (0, 0, 2000, 2000));
        let shape = rect(0.0, 0.0, 1000.0, 1000.0);
        let at = g.place(shape, 0.0, false, false).rect;
        let t = g.text_rect(shape, at, rect(100.0, 100.0, 800.0, 800.0));
        assert!(close(t.x + t.w / 2.0, at.x + at.w / 2.0));
        assert!(close(t.w, 800.0) && close(t.h, 800.0));
    }

    fn piece(r: Rect, rotation: f32, flip_h: bool) -> Element {
        Element::Shape(Shape {
            rect: r,
            rotation,
            flip_h,
            flip_v: false,
            geometry: Geometry::Rect,
            fill: None,
            stroke: None,
            text: None,
            is_title: false,
            text_rect: Some(rect(r.x + 10.0, r.y + 10.0, r.w - 20.0, r.h - 20.0)),
        })
    }

    #[test]
    fn pieces_of_a_table_or_chart_turn_like_a_child_shape() {
        // A frame of several pieces (table cells, chart bars) is built at its
        // scaled position and then turned; each piece must land exactly where
        // a plain child with the same rect would.
        let g = group(
            &Xform::ID,
            r#"rot="5400000" flipH="1""#,
            (1000, 1000),
            (2000, 1000),
            (0, 0, 1000, 500),
        );
        let child = rect(100.0, 50.0, 300.0, 100.0);
        let mut pieces = [piece(g.rect(child), 30.0, true)];
        g.turn_elements(&mut pieces);
        let Element::Shape(s) = &pieces[0] else { panic!("not a shape") };
        let want = g.place(child, 30.0, true, false);
        assert_placed(
            Placed {
                rect: s.rect,
                rotation: s.rotation,
                flip_h: s.flip_h,
                flip_v: s.flip_v,
            },
            [want.rect.x, want.rect.y, want.rect.w, want.rect.h],
            want.rotation,
            (want.flip_h, want.flip_v),
        );
        // The text box moved with its shape.
        let t = s.text_rect.unwrap();
        assert!(close(t.x - s.rect.x, 10.0) && close(t.y - s.rect.y, 10.0));
        // Guard against a no-op: the piece really did move and turn.
        assert!(!close(s.rect.x, g.rect(child).x) && s.rotation != 30.0);
    }

    #[test]
    fn pieces_inside_a_plain_group_stay_put() {
        let g = group(&Xform::ID, "", (1000, 1000), (2000, 1000), (0, 0, 1000, 500));
        let r = g.rect(rect(100.0, 50.0, 300.0, 100.0));
        let mut pieces = [piece(r, 0.0, false)];
        g.turn_elements(&mut pieces);
        let Element::Shape(s) = &pieces[0] else { panic!("not a shape") };
        assert_eq!((s.rect.x, s.rect.y, s.rotation), (r.x, r.y, 0.0));
    }
}
