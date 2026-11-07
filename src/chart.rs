//! Charts: drawn from the data PowerPoint caches inside the chart part.
//!
//! A chart part (`ppt/charts/chartN.xml`) keeps a copy of its numbers and
//! labels (`numCache` / `strCache`), so the chart can be redrawn without the
//! embedded workbook. The result is ordinary shapes and text boxes in the
//! IR, so the PDF writer needs nothing chart-specific. The look follows
//! Office's default chart style: gray axis text, light gridlines, theme
//! accent colors.

use crate::ir::*;
use crate::xml::Node;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stack {
    None,
    Stacked,
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// `gap` and `overlap` are fractions of a bar width (gapWidth/100, overlap/100).
    Bar {
        horizontal: bool,
        stack: Stack,
        gap: f32,
        overlap: f32,
    },
    Line {
        stack: Stack,
    },
    Area {
        stack: Stack,
    },
    Pie {
        hole: f32,
        first_angle: f32,
    },
    Scatter {
        lines: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
    /// Scatter x values (numeric), when present.
    pub x: Vec<Option<f64>>,
    pub color: Option<Rgb>,
    pub point_colors: Vec<(usize, Rgb)>,
    pub markers: bool,
}

#[derive(Debug, Clone)]
pub struct Chart {
    pub kind: Kind,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    pub title: Option<String>,
    pub legend: Option<char>, // 'r', 'b', 't', 'l'
    pub labels: bool,
    pub percent_labels: bool,
    pub vary_colors: bool,
    pub notes: Vec<String>,
}

const GROUPS: [&str; 15] = [
    "barChart",
    "bar3DChart",
    "lineChart",
    "line3DChart",
    "areaChart",
    "area3DChart",
    "pieChart",
    "pie3DChart",
    "doughnutChart",
    "ofPieChart",
    "scatterChart",
    "radarChart",
    "bubbleChart",
    "stockChart",
    "surfaceChart",
];

fn texts(n: &Node, out: &mut String) {
    if n.name == "t" || n.name == "v" {
        out.push_str(&n.text);
    }
    for c in &n.children {
        texts(c, out);
    }
}

/// Cached points of a `strRef`/`numRef`/`strLit`/`numLit`, by index.
fn cache(n: Option<&Node>) -> Vec<(usize, String)> {
    let Some(n) = n else { return Vec::new() };
    let Some(c) = ["strCache", "numCache", "multiLvlStrCache"]
        .iter()
        .find_map(|k| n.find(k))
        .or_else(|| n.children.iter().find(|c| c.name.ends_with("Lit")))
    else {
        return Vec::new();
    };
    let mut pts: Vec<(usize, String)> = Vec::new();
    // multi-level caches: the first level holds the leaf labels.
    let holder = c.child("lvl").unwrap_or(c);
    for p in holder.children_named("pt") {
        let idx = p.attr_i64("idx").unwrap_or(0).clamp(0, 100_000) as usize;
        pts.push((idx, p.child("v").map(|v| v.text.clone()).unwrap_or_default()));
    }
    pts
}

fn dense(pts: &[(usize, String)], len: usize) -> Vec<String> {
    let mut v = vec![String::new(); len];
    for (i, s) in pts {
        if *i < len {
            v[*i] = s.clone();
        }
    }
    v
}

/// Parses a chart part. `color` resolves a fill container (`solidFill`
/// parent) to a color; `accents` are the theme's accent1..6.
pub fn parse(root: &Node, color: &dyn Fn(&Node) -> Option<Rgb>) -> Option<Chart> {
    let chart = root.child("chart")?;
    let plot = chart.child("plotArea")?;
    let groups: Vec<&Node> = plot.children.iter().filter(|c| GROUPS.contains(&c.name.as_str())).collect();
    let g = *groups.first()?;
    let mut notes = Vec::new();
    if groups.len() > 1 {
        notes.push("combination chart: only the first chart type is drawn".to_string());
    }
    if g.name.contains("3D") {
        notes.push("3D chart drawn flat".to_string());
    }
    let val = |n: &Node, k: &str| n.child(k).and_then(|c| c.attr("val")).map(String::from);
    let stack = match val(g, "grouping").as_deref() {
        Some("stacked") => Stack::Stacked,
        Some("percentStacked") => Stack::Percent,
        _ => Stack::None,
    };
    let kind = match g.name.as_str() {
        "barChart" | "bar3DChart" => Kind::Bar {
            horizontal: val(g, "barDir").as_deref() == Some("bar"),
            stack,
            gap: val(g, "gapWidth")
                .and_then(|v| v.parse().ok())
                .unwrap_or(150.0f32)
                .clamp(0.0, 500.0)
                / 100.0,
            overlap: val(g, "overlap")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0f32)
                .clamp(-100.0, 100.0)
                / 100.0,
        },
        "lineChart" | "line3DChart" => Kind::Line { stack },
        "areaChart" | "area3DChart" => Kind::Area { stack },
        "pieChart" | "pie3DChart" | "ofPieChart" => Kind::Pie {
            hole: 0.0,
            first_angle: val(g, "firstSliceAng").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        },
        "doughnutChart" => Kind::Pie {
            hole: val(g, "holeSize")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(50.0)
                .clamp(10.0, 90.0)
                / 100.0,
            first_angle: val(g, "firstSliceAng").and_then(|v| v.parse().ok()).unwrap_or(0.0),
        },
        "scatterChart" => Kind::Scatter {
            lines: !matches!(val(g, "scatterStyle").as_deref(), Some("marker")),
        },
        other => {
            notes.push(format!("{other} drawn as a line chart"));
            Kind::Line { stack: Stack::None }
        }
    };

    let mut categories: Vec<String> = Vec::new();
    let mut series = Vec::new();
    for s in g.children_named("ser") {
        let cat = cache(s.child("cat").or_else(|| s.child("xVal")));
        let vals = cache(s.child("val").or_else(|| s.child("yVal")));
        let n = vals
            .iter()
            .map(|(i, _)| i + 1)
            .max()
            .unwrap_or(0)
            .max(cat.iter().map(|(i, _)| i + 1).max().unwrap_or(0))
            .min(10_000);
        if categories.len() < n {
            let c = dense(&cat, n);
            if c.iter().any(|s| !s.is_empty()) || categories.is_empty() {
                categories = c;
            }
        }
        let values: Vec<Option<f64>> = dense(&vals, n)
            .iter()
            .map(|v| v.trim().parse::<f64>().ok().filter(|f| f.is_finite()))
            .collect();
        let x: Vec<Option<f64>> = if s.child("xVal").is_some() {
            dense(&cat, n)
                .iter()
                .map(|v| v.trim().parse::<f64>().ok().filter(|f| f.is_finite()))
                .collect()
        } else {
            Vec::new()
        };
        let mut name = String::new();
        if let Some(tx) = s.child("tx") {
            texts(tx, &mut name);
        }
        let sp = s.child("spPr");
        let fill = sp.and_then(|p| p.child("solidFill")).and_then(color);
        let line = sp.and_then(|p| p.path(&["ln", "solidFill"])).and_then(color);
        let line_none = sp.and_then(|p| p.child("ln")).map(|l| l.child("noFill").is_some()).unwrap_or(false);
        let is_line = matches!(kind, Kind::Line { .. } | Kind::Scatter { .. });
        let point_colors = s
            .children_named("dPt")
            .filter_map(|d| {
                let i = d.child("idx")?.attr_i64("val")?.clamp(0, 100_000) as usize;
                let c = d.child("spPr").and_then(|p| p.child("solidFill")).and_then(color)?;
                Some((i, c))
            })
            .collect();
        // Markers: the series symbol decides; otherwise the chart-level
        // `marker` flag of line charts, and always for scatter.
        let group_markers = g.child("marker").and_then(|m| m.attr_bool("val")).unwrap_or(false);
        let markers = s
            .path(&["marker", "symbol"])
            .map(|m| m.attr("val") != Some("none"))
            .unwrap_or(group_markers || matches!(kind, Kind::Scatter { .. }));
        series.push(Series {
            name,
            values,
            x,
            color: if is_line { line.or(fill) } else { fill.or(line) },
            point_colors,
            markers: markers || (is_line && line_none),
        });
    }
    if series.is_empty() {
        return None;
    }
    if categories.is_empty() {
        let n = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
        categories = (1..=n).map(|i| i.to_string()).collect();
    }
    if categories.len() > 2000 {
        notes.push("chart has more than 2000 points; the rest is dropped".to_string());
        categories.truncate(2000);
    }

    let mut title = None;
    if let Some(t) = chart.child("title") {
        let mut s = String::new();
        if let Some(tx) = t.child("tx") {
            texts(tx, &mut s);
        }
        // A title element without text shows the series name for one
        // series, and Office's placeholder otherwise.
        if s.trim().is_empty() && chart.child("autoTitleDeleted").and_then(|a| a.attr_bool("val")) != Some(true) {
            s = if series.len() == 1 {
                series[0].name.clone()
            } else {
                "Chart Title".to_string()
            };
        }
        if !s.trim().is_empty() {
            title = Some(s.trim().to_string());
        }
    }
    let legend = chart.child("legend").map(|l| {
        l.child("legendPos")
            .and_then(|p| p.attr("val"))
            .and_then(|v| v.chars().next())
            .unwrap_or('r')
    });
    let shows = |k: &str| {
        g.children_named("dLbls")
            .chain(g.children_named("ser").flat_map(|s| s.children_named("dLbls")))
            .any(|d| d.child(k).and_then(|c| c.attr_bool("val")) == Some(true))
    };
    Some(Chart {
        kind,
        categories,
        series,
        title,
        legend,
        labels: shows("showVal"),
        percent_labels: shows("showPercent"),
        vary_colors: g.child("varyColors").and_then(|v| v.attr_bool("val")).unwrap_or(false),
        notes,
    })
}

// ---------------------------------------------------------------- layout

const PT: f32 = EMU_PER_PT;
const TEXT: Rgb = Rgb(0x59, 0x59, 0x59);
const GRID: Rgb = Rgb(0xD9, 0xD9, 0xD9);

pub struct Style<'a> {
    pub accents: &'a [Rgb],
    pub face: &'a str,
}

impl Style<'_> {
    fn accent(&self, i: usize) -> Rgb {
        if self.accents.is_empty() {
            return Rgb(68, 114, 196);
        }
        let base = self.accents[i % self.accents.len()];
        // Past the six accents Office reuses them darker, then lighter.
        let round = i / self.accents.len();
        let f = match round {
            0 => 1.0,
            1 => 0.6,
            _ => 1.4,
        };
        let (r, g, b) = base.floats();
        let q = |x: f32| ((if f <= 1.0 { x * f } else { x + (1.0 - x) * (f - 1.0) }).clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb(q(r), q(g), q(b))
    }

    fn series_color(&self, c: &Chart, i: usize) -> Rgb {
        c.series[i].color.unwrap_or_else(|| self.accent(i))
    }

    fn point_color(&self, c: &Chart, s: usize, p: usize) -> Rgb {
        if let Some((_, col)) = c.series[s].point_colors.iter().find(|(i, _)| *i == p) {
            return *col;
        }
        if c.vary_colors || matches!(c.kind, Kind::Pie { .. }) {
            self.accent(p)
        } else {
            self.series_color(c, s)
        }
    }

    fn text(&self, s: &str, r: Rect, size: f32, align: Align, bold: bool) -> Element {
        let run = Run {
            text: s.to_string(),
            size,
            bold,
            italic: false,
            underline: false,
            color: TEXT,
            family: Family::Sans,
            face: self.face.to_string(),
            line_break: false,
        };
        Element::Shape(Shape {
            rect: r,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry: Geometry::Rect,
            fill: None,
            stroke: None,
            text: Some(TextBody {
                insets: [0.0; 4],
                rotation: 0.0,
                anchor: Anchor::Middle,
                wrap: false,
                font_scale: 1.0,
                line_reduction: 0.0,
                paragraphs: vec![Paragraph {
                    align,
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
        })
    }
}

fn shape(r: Rect, geometry: Geometry, fill: Option<Rgb>, stroke: Option<Stroke>) -> Element {
    Element::Shape(Shape {
        rect: r,
        rotation: 0.0,
        flip_h: false,
        flip_v: false,
        geometry,
        fill: fill.map(Fill::Solid),
        stroke,
        text: None,
        is_title: false,
        text_rect: None,
    })
}

fn hline(x0: f32, x1: f32, y: f32, color: Rgb, w: f32) -> Element {
    shape(
        Rect {
            x: x0,
            y,
            w: (x1 - x0).max(1.0),
            h: 0.0,
        },
        Geometry::Line,
        None,
        Some(Stroke { color, width: w }),
    )
}

fn vline(x: f32, y0: f32, y1: f32, color: Rgb, w: f32) -> Element {
    shape(
        Rect {
            x,
            y: y0,
            w: 0.0,
            h: (y1 - y0).max(1.0),
        },
        Geometry::Line,
        None,
        Some(Stroke { color, width: w }),
    )
}

/// Rough text width in EMU for layout decisions (Calibri-ish average).
fn text_w(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.52 * PT
}

/// "Nice" axis: min, max and step from {1, 2, 2.5, 5} x 10^k, about 5 ticks.
fn nice_axis(mut lo: f64, mut hi: f64) -> (f64, f64, f64) {
    if lo > 0.0 {
        lo = 0.0;
    }
    if hi < 0.0 {
        hi = 0.0;
    }
    if (hi - lo).abs() < 1e-12 {
        hi = lo + 1.0;
    }
    // Like Excel, leave about 5% headroom so the tallest bar does not touch the top.
    let span = hi - lo;
    if hi > 0.0 {
        hi += span * 0.05;
    }
    if lo < 0.0 {
        lo -= span * 0.05;
    }
    let raw = (hi - lo) / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    ((lo / step).floor() * step, (hi / step).ceil() * step, step)
}

fn fmt_num(v: f64, percent: bool) -> String {
    if percent {
        return format!("{}%", (v * 100.0).round() as i64);
    }
    if (v - v.round()).abs() < 1e-9 && v.abs() < 1e15 {
        return format!("{}", v.round() as i64);
    }
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Lays the chart out inside `frame` as IR elements.
pub fn layout(c: &Chart, frame: Rect, st: &Style) -> Vec<Element> {
    let mut out = Vec::new();
    let pad = 6.0 * PT;
    let mut area = Rect {
        x: frame.x + pad,
        y: frame.y + pad,
        w: frame.w - 2.0 * pad,
        h: frame.h - 2.0 * pad,
    };
    if area.w <= 0.0 || area.h <= 0.0 {
        return out;
    }
    // Title
    if let Some(t) = &c.title {
        let h = 14.0 * 1.5 * PT;
        out.push(st.text(t, Rect { h, ..area }, 14.0, Align::Center, false));
        area.y += h;
        area.h -= h;
    }
    // Legend
    let pie = matches!(c.kind, Kind::Pie { .. });
    let entries: Vec<(String, Rgb)> = if pie || (c.vary_colors && c.series.len() == 1) {
        c.categories
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), st.point_color(c, 0, i)))
            .collect()
    } else {
        c.series
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), st.series_color(c, i)))
            .collect()
    };
    let ls = 9.0;
    if let Some(pos) = c.legend {
        let sw = ls * 0.8 * PT;
        let item_h = ls * 1.6 * PT;
        if pos == 'r' || pos == 'l' {
            let w = entries.iter().map(|(n, _)| text_w(n, ls)).fold(0.0, f32::max) + sw + 8.0 * PT;
            let w = w.min(area.w * 0.4);
            let x = if pos == 'r' { area.x + area.w - w } else { area.x };
            let total = item_h * entries.len() as f32;
            let mut y = area.y + ((area.h - total) / 2.0).max(0.0);
            for (name, col) in entries.iter().take(40) {
                out.push(shape(
                    Rect {
                        x: x + 2.0 * PT,
                        y: y + (item_h - sw) / 2.0,
                        w: sw,
                        h: sw,
                    },
                    Geometry::Rect,
                    Some(*col),
                    None,
                ));
                out.push(st.text(
                    name,
                    Rect {
                        x: x + sw + 5.0 * PT,
                        y,
                        w: w - sw - 5.0 * PT,
                        h: item_h,
                    },
                    ls,
                    Align::Left,
                    false,
                ));
                y += item_h;
            }
            area.w -= w + 4.0 * PT;
            if pos == 'l' {
                area.x += w + 4.0 * PT;
            }
        } else {
            let widths: Vec<f32> = entries.iter().map(|(n, _)| text_w(n, ls) + sw + 12.0 * PT).collect();
            let total: f32 = widths.iter().sum::<f32>().min(area.w);
            let y = if pos == 't' { area.y } else { area.y + area.h - item_h };
            let mut x = area.x + (area.w - total) / 2.0;
            for ((name, col), w) in entries.iter().zip(&widths).take(40) {
                if x + w > area.x + area.w + 1.0 {
                    break;
                }
                out.push(shape(
                    Rect {
                        x,
                        y: y + (item_h - sw) / 2.0,
                        w: sw,
                        h: sw,
                    },
                    Geometry::Rect,
                    Some(*col),
                    None,
                ));
                out.push(st.text(
                    name,
                    Rect {
                        x: x + sw + 4.0 * PT,
                        y,
                        w: w - sw - 4.0 * PT,
                        h: item_h,
                    },
                    ls,
                    Align::Left,
                    false,
                ));
                x += w;
            }
            area.h -= item_h + 4.0 * PT;
            if pos == 't' {
                area.y += item_h + 4.0 * PT;
            }
        }
    }
    if area.w <= 0.0 || area.h <= 0.0 {
        return out;
    }
    match c.kind {
        Kind::Pie { hole, first_angle } => pie_chart(c, area, st, hole, first_angle, &mut out),
        _ => axis_chart(c, area, st, &mut out),
    }
    out
}

fn arc_points(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<PathCmd> {
    // Angles in degrees, clockwise from 12 o'clock, unit-box coordinates.
    let to_xy = |a: f32| {
        let t = (a - 90.0).to_radians();
        (cx + r * t.cos(), cy + r * t.sin())
    };
    let segs = (((a1 - a0).abs() / 90.0).ceil() as usize).max(1);
    let step = (a1 - a0) / segs as f32;
    let k = 4.0 / 3.0 * (step.to_radians() / 4.0).tan();
    let mut v = Vec::new();
    for i in 0..segs {
        let (s, e) = (a0 + step * i as f32, a0 + step * (i + 1) as f32);
        let (p0, p3) = (to_xy(s), to_xy(e));
        let (ts, te) = ((s - 90.0).to_radians(), (e - 90.0).to_radians());
        let c1 = (p0.0 - k * r * ts.sin(), p0.1 + k * r * ts.cos());
        let c2 = (p3.0 + k * r * te.sin(), p3.1 - k * r * te.cos());
        v.push(PathCmd::Cubic(c1.0, c1.1, c2.0, c2.1, p3.0, p3.1));
    }
    v
}

fn pie_chart(c: &Chart, area: Rect, st: &Style, hole: f32, first: f32, out: &mut Vec<Element>) {
    let vals: Vec<f64> = c.series[0].values.iter().map(|v| v.unwrap_or(0.0).max(0.0)).collect();
    let total: f64 = vals.iter().sum();
    if total <= 0.0 {
        return;
    }
    let d = area.w.min(area.h) * 0.92;
    let sq = Rect {
        x: area.x + (area.w - d) / 2.0,
        y: area.y + (area.h - d) / 2.0,
        w: d,
        h: d,
    };
    let mut a = first;
    for (i, v) in vals.iter().enumerate() {
        if *v <= 0.0 {
            continue;
        }
        let sweep = (*v / total * 360.0) as f32;
        let (a0, a1) = (a, a + sweep);
        let start = |r: f32, ang: f32| {
            let t = (ang - 90.0).to_radians();
            (0.5 + r * t.cos(), 0.5 + r * t.sin())
        };
        let mut cmds = Vec::new();
        if hole > 0.0 {
            let p = start(0.5, a0);
            cmds.push(PathCmd::Move(p.0, p.1));
            cmds.extend(arc_points(0.5, 0.5, 0.5, a0, a1));
            let q = start(0.5 * hole, a1);
            cmds.push(PathCmd::Line(q.0, q.1));
            cmds.extend(arc_points(0.5, 0.5, 0.5 * hole, a1, a0));
        } else {
            cmds.push(PathCmd::Move(0.5, 0.5));
            let p = start(0.5, a0);
            cmds.push(PathCmd::Line(p.0, p.1));
            cmds.extend(arc_points(0.5, 0.5, 0.5, a0, a1));
        }
        cmds.push(PathCmd::Close);
        out.push(shape(
            sq,
            Geometry::Path(cmds),
            Some(st.point_color(c, 0, i)),
            Some(Stroke {
                color: Rgb::WHITE,
                width: 9525.0,
            }),
        ));
        if c.labels || c.percent_labels {
            let mid = start(if hole > 0.0 { 0.25 * (1.0 + hole) } else { 0.33 }, a0 + sweep / 2.0);
            let label = if c.percent_labels {
                format!("{}%", (*v / total * 100.0).round() as i64)
            } else {
                fmt_num(*v, false)
            };
            let (lx, ly) = (sq.x + mid.0 * d, sq.y + mid.1 * d);
            let w = text_w(&label, 9.0) + 4.0 * PT;
            let h = 12.0 * PT;
            out.push(st.text(
                &label,
                Rect {
                    x: lx - w / 2.0,
                    y: ly - h / 2.0,
                    w,
                    h,
                },
                9.0,
                Align::Center,
                false,
            ));
        }
        a = a1;
    }
}

// Index loops are clearer here: several parallel arrays share the category index.
#[allow(clippy::needless_range_loop)]
fn axis_chart(c: &Chart, area: Rect, st: &Style, out: &mut Vec<Element>) {
    let ncat = c.categories.len().max(1);
    let (horizontal, stack) = match c.kind {
        Kind::Bar { horizontal, stack, .. } => (horizontal, stack),
        Kind::Line { stack } | Kind::Area { stack } => (false, stack),
        _ => (false, Stack::None),
    };
    let scatter = matches!(c.kind, Kind::Scatter { .. });
    // Value range (sums per category when stacked).
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    if stack == Stack::Percent {
        (lo, hi) = (0.0, 1.0);
    } else if stack == Stack::Stacked {
        for i in 0..ncat {
            let (mut p, mut n) = (0.0, 0.0);
            for s in &c.series {
                let v = s.values.get(i).copied().flatten().unwrap_or(0.0);
                if v >= 0.0 {
                    p += v
                } else {
                    n += v
                }
            }
            lo = lo.min(n);
            hi = hi.max(p);
        }
    } else {
        for v in c.series.iter().flat_map(|s| s.values.iter().flatten()) {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
    }
    if lo > hi {
        (lo, hi) = (0.0, 1.0);
    }
    let (vmin, vmax, step) = nice_axis(lo, hi);
    let ticks: Vec<f64> = (0..=((vmax - vmin) / step).round().clamp(1.0, 50.0) as usize)
        .map(|i| vmin + step * i as f64)
        .collect();
    let pct = stack == Stack::Percent;

    // Scatter x range.
    let (xmin, xmax, xstep) = if scatter {
        let xs: Vec<f64> = c.series.iter().flat_map(|s| s.x.iter().flatten().copied()).collect();
        if xs.is_empty() {
            nice_axis(1.0, ncat as f64)
        } else {
            nice_axis(
                xs.iter().cloned().fold(f64::MAX, f64::min),
                xs.iter().cloned().fold(f64::MIN, f64::max),
            )
        }
    } else {
        (0.0, 1.0, 1.0)
    };

    let fs = 9.0;
    let val_labels: Vec<String> = ticks.iter().map(|t| fmt_num(*t, pct)).collect();
    let cat_labels: Vec<String> = if scatter {
        let n = ((xmax - xmin) / xstep).round().clamp(1.0, 50.0) as usize;
        (0..=n).map(|i| fmt_num(xmin + xstep * i as f64, false)).collect()
    } else {
        c.categories.clone()
    };
    // Space for the axis labels.
    let left_w = if horizontal {
        cat_labels.iter().map(|l| text_w(l, fs)).fold(0.0, f32::max).min(area.w * 0.35)
    } else {
        val_labels.iter().map(|l| text_w(l, fs)).fold(0.0, f32::max)
    } + 4.0 * PT;
    let bottom_h = fs * 1.8 * PT;
    let plot = Rect {
        x: area.x + left_w,
        y: area.y + fs * 0.6 * PT,
        w: (area.w - left_w).max(1.0),
        h: (area.h - bottom_h - fs * 0.6 * PT).max(1.0),
    };

    // Value -> coordinate along the value axis.
    let vpos = |v: f64| -> f32 {
        let t = ((v - vmin) / (vmax - vmin)) as f32;
        if horizontal {
            plot.x + t * plot.w
        } else {
            plot.y + plot.h - t * plot.h
        }
    };
    // Gridlines and value labels.
    for (t, label) in ticks.iter().zip(&val_labels) {
        let p = vpos(*t);
        if horizontal {
            out.push(vline(p, plot.y, plot.y + plot.h, GRID, 9525.0));
            let w = text_w(label, fs) + 4.0 * PT;
            out.push(st.text(
                label,
                Rect {
                    x: p - w / 2.0,
                    y: plot.y + plot.h + 2.0 * PT,
                    w,
                    h: fs * 1.4 * PT,
                },
                fs,
                Align::Center,
                false,
            ));
        } else {
            out.push(hline(plot.x, plot.x + plot.w, p, GRID, 9525.0));
            out.push(st.text(
                label,
                Rect {
                    x: area.x,
                    y: p - fs * 0.7 * PT,
                    w: left_w - 4.0 * PT,
                    h: fs * 1.4 * PT,
                },
                fs,
                Align::Right,
                false,
            ));
        }
    }
    // Category axis line at zero.
    let zero = vpos(0.0f64.clamp(vmin, vmax));
    if horizontal {
        out.push(vline(zero, plot.y, plot.y + plot.h, Rgb(0xBF, 0xBF, 0xBF), 9525.0));
    } else {
        out.push(hline(plot.x, plot.x + plot.w, zero, Rgb(0xBF, 0xBF, 0xBF), 9525.0));
    }

    // Category labels (skip some when they would overlap).
    let band = if horizontal { plot.h } else { plot.w } / ncat as f32;
    if !scatter {
        let widest = cat_labels.iter().map(|l| text_w(l, fs)).fold(0.0, f32::max);
        let every = if horizontal { 1 } else { ((widest / band).ceil() as usize).max(1) };
        for (i, l) in cat_labels.iter().enumerate().step_by(every) {
            if horizontal {
                // First category at the bottom, as Office draws bar charts.
                let y = plot.y + plot.h - band * (i as f32 + 1.0);
                out.push(st.text(
                    l,
                    Rect {
                        x: area.x,
                        y,
                        w: left_w - 4.0 * PT,
                        h: band,
                    },
                    fs,
                    Align::Right,
                    false,
                ));
            } else {
                let x = plot.x + band * i as f32;
                let w = band * every as f32;
                out.push(st.text(
                    l,
                    Rect {
                        x: x - (w - band) / 2.0,
                        y: plot.y + plot.h + 2.0 * PT,
                        w,
                        h: fs * 1.4 * PT,
                    },
                    fs,
                    Align::Center,
                    false,
                ));
            }
        }
    } else {
        let n = cat_labels.len().max(2) - 1;
        for (i, l) in cat_labels.iter().enumerate() {
            let x = plot.x + plot.w * i as f32 / n as f32;
            let w = text_w(l, fs) + 4.0 * PT;
            out.push(st.text(
                l,
                Rect {
                    x: x - w / 2.0,
                    y: plot.y + plot.h + 2.0 * PT,
                    w,
                    h: fs * 1.4 * PT,
                },
                fs,
                Align::Center,
                false,
            ));
        }
    }

    let nser = c.series.len().max(1);
    let total_at = |i: usize| -> f64 {
        c.series
            .iter()
            .map(|s| s.values.get(i).copied().flatten().unwrap_or(0.0).abs())
            .sum::<f64>()
            .max(1e-12)
    };
    let value_at = |s: usize, i: usize| -> Option<f64> {
        let v = c.series[s].values.get(i).copied().flatten()?;
        Some(if pct { v / total_at(i) } else { v })
    };
    match c.kind {
        Kind::Bar { gap, overlap, .. } => {
            // Office: band = n*w - (n-1)*w*overlap + gap*w (stacked bars act as n = 1).
            let (n, ov) = if stack == Stack::None { (nser as f32, overlap) } else { (1.0, 1.0) };
            let bar_w = band / (n - (n - 1.0) * ov + gap);
            let group = bar_w * (n - (n - 1.0) * ov);
            let mut pos_base = vec![0.0f64; ncat];
            let mut neg_base = vec![0.0f64; ncat];
            for s in 0..nser {
                for i in 0..ncat {
                    let Some(v) = value_at(s, i) else { continue };
                    let (from, to) = if stack == Stack::None {
                        (0.0, v)
                    } else {
                        let base = if v >= 0.0 { &mut pos_base[i] } else { &mut neg_base[i] };
                        let from = *base;
                        *base += v;
                        (from, *base)
                    };
                    let (a, b) = (vpos(from.clamp(vmin, vmax)), vpos(to.clamp(vmin, vmax)));
                    let (w, offset) = if stack == Stack::None {
                        (bar_w, bar_w * (1.0 - ov) * s as f32)
                    } else {
                        (bar_w, 0.0)
                    };
                    let start = band * i as f32 + (band - group) / 2.0 + offset;
                    let r = if horizontal {
                        Rect {
                            x: a.min(b),
                            y: plot.y + plot.h - start - w,
                            w: (a - b).abs(),
                            h: w,
                        }
                    } else {
                        Rect {
                            x: plot.x + start,
                            y: a.min(b),
                            w,
                            h: (a - b).abs(),
                        }
                    };
                    out.push(shape(r, Geometry::Rect, Some(st.point_color(c, s, i)), None));
                    if c.labels {
                        let label = fmt_num(c.series[s].values[i].unwrap_or(0.0), false);
                        let lw = text_w(&label, 8.0) + 4.0 * PT;
                        let lr = if horizontal {
                            Rect {
                                x: r.x + r.w + 2.0 * PT,
                                y: r.y,
                                w: lw,
                                h: r.h,
                            }
                        } else {
                            Rect {
                                x: r.x + r.w / 2.0 - lw / 2.0,
                                y: r.y - 11.0 * PT,
                                w: lw,
                                h: 10.0 * PT,
                            }
                        };
                        out.push(st.text(&label, lr, 8.0, Align::Center, false));
                    }
                }
            }
        }
        Kind::Line { .. } | Kind::Area { .. } | Kind::Scatter { .. } => {
            let area_chart = matches!(c.kind, Kind::Area { .. });
            let lines = !matches!(c.kind, Kind::Scatter { lines: false });
            let mut base = vec![0.0f64; ncat];
            let mut previous: Vec<(f32, f32)> = (0..ncat).map(|i| (plot.x + band * (i as f32 + 0.5), zero)).collect();
            for s in 0..nser {
                let mut pts: Vec<(f32, f32)> = Vec::new();
                for i in 0..ncat {
                    let Some(mut v) = value_at(s, i) else { continue };
                    if stack != Stack::None {
                        base[i] += v;
                        v = base[i];
                    }
                    let x = if scatter {
                        let xv = c.series[s].x.get(i).copied().flatten().unwrap_or(i as f64 + 1.0);
                        plot.x + ((xv - xmin) / (xmax - xmin)) as f32 * plot.w
                    } else if area_chart {
                        plot.x + plot.w * i as f32 / (ncat.max(2) - 1) as f32
                    } else {
                        plot.x + band * (i as f32 + 0.5)
                    };
                    pts.push((x, vpos(v.clamp(vmin, vmax))));
                }
                if pts.is_empty() {
                    continue;
                }
                let col = st.series_color(c, s);
                let unit = |(x, y): (f32, f32)| ((x - plot.x) / plot.w, (y - plot.y) / plot.h);
                if area_chart {
                    let floor: Vec<(f32, f32)> = if stack != Stack::None && s > 0 {
                        previous.clone()
                    } else {
                        pts.iter().map(|p| (p.0, zero)).collect()
                    };
                    let mut cmds = vec![];
                    for (k, p) in pts.iter().enumerate() {
                        let (u, v) = unit(*p);
                        cmds.push(if k == 0 { PathCmd::Move(u, v) } else { PathCmd::Line(u, v) });
                    }
                    for p in floor.iter().rev().take(pts.len()) {
                        let (u, v) = unit(*p);
                        cmds.push(PathCmd::Line(u, v));
                    }
                    cmds.push(PathCmd::Close);
                    out.push(shape(plot, Geometry::Path(cmds), Some(col), None));
                    previous = pts.clone();
                } else if lines && pts.len() > 1 {
                    let mut cmds = vec![];
                    for (k, p) in pts.iter().enumerate() {
                        let (u, v) = unit(*p);
                        cmds.push(if k == 0 { PathCmd::Move(u, v) } else { PathCmd::Line(u, v) });
                    }
                    out.push(shape(
                        plot,
                        Geometry::Open(cmds),
                        None,
                        Some(Stroke {
                            color: col,
                            width: 28575.0,
                        }),
                    ));
                }
                if c.series[s].markers && !area_chart {
                    let m = 5.0 * PT;
                    for p in &pts {
                        out.push(shape(
                            Rect {
                                x: p.0 - m / 2.0,
                                y: p.1 - m / 2.0,
                                w: m,
                                h: m,
                            },
                            Geometry::Ellipse,
                            Some(col),
                            None,
                        ));
                    }
                }
                if c.labels {
                    for (k, p) in pts.iter().enumerate() {
                        let label = c.series[s]
                            .values
                            .iter()
                            .flatten()
                            .nth(k)
                            .map(|v| fmt_num(*v, false))
                            .unwrap_or_default();
                        let lw = text_w(&label, 8.0) + 4.0 * PT;
                        out.push(st.text(
                            &label,
                            Rect {
                                x: p.0 - lw / 2.0,
                                y: p.1 - 13.0 * PT,
                                w: lw,
                                h: 10.0 * PT,
                            },
                            8.0,
                            Align::Center,
                            false,
                        ));
                    }
                }
            }
        }
        Kind::Pie { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_axis_steps() {
        assert_eq!(nice_axis(0.0, 8.2), (0.0, 10.0, 2.0));
        assert_eq!(nice_axis(0.0, 5.0), (0.0, 6.0, 2.0));
        assert_eq!(nice_axis(3.0, 47.0), (0.0, 50.0, 10.0));
        let (lo, hi, _) = nice_axis(-12.0, 30.0);
        assert!(lo <= -12.0 && hi >= 30.0);
    }

    #[test]
    fn number_format() {
        assert_eq!(fmt_num(4.0, false), "4");
        assert_eq!(fmt_num(4.25, false), "4.25");
        assert_eq!(fmt_num(0.5, true), "50%");
    }
}
