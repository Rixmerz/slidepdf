//! SmartArt without a saved drawing.
//!
//! PowerPoint stores a pre-laid-out copy of every diagram
//! (`diagrams/drawingN.xml`). Other writers (LibreOffice, Google Slides,
//! python-pptx, old Office) keep only the data model, and PowerPoint lays it
//! out again when the file is opened. This module does the same for the
//! common layout families, with plain geometry: [`Model::parse`] reads the
//! tree of nodes, [`family_of`] picks a family from the layout's name,
//! [`layout`] returns the pieces to draw in the frame's own coordinates, and
//! [`fit_scale`] sizes a node's text to its box. Colors, text styles and the
//! conversion to IR shapes stay in `pptx.rs`.

use std::collections::{HashMap, HashSet};

use crate::ir::{Geometry, Paragraph, PathCmd, Rect, TextBody, EMU_PER_PT};
use crate::xml::Node;

/// Nodes laid out per diagram, and how deep the tree may go. A hostile data
/// model (a million nodes, a connection cycle) is cut here, not downstream.
pub const MAX_NODES: usize = 300;
pub const MAX_DEPTH: usize = 8;

/// Fraction of a box left empty between neighbours.
const GAP: f32 = 0.1;

// ---------------------------------------------------------------- data model

/// One node of the diagram's tree.
#[derive(Debug, Clone)]
pub struct DNode {
    /// Index of the `pt` element in the data part's `ptLst`, which holds the
    /// node's rich text (`pt/t`).
    pub pt: usize,
    /// An assistant (`type="asst"`): drawn between its parent and the
    /// parent's other children in a hierarchy.
    pub assistant: bool,
    /// Indices into [`Model::nodes`], in drawing order.
    pub children: Vec<usize>,
    /// The node's text, paragraphs joined by newlines.
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub nodes: Vec<DNode>,
    /// Children of the document node, in order.
    pub roots: Vec<usize>,
    /// The layout's identifier (`loTypeId` of the document node).
    pub layout_id: Option<String>,
    /// True when nodes were dropped for exceeding [`MAX_NODES`] or [`MAX_DEPTH`].
    pub truncated: bool,
}

impl Model {
    /// Reads `dgm:dataModel`: the `node`, `asst` and `doc` points and the
    /// parent-child connections between them. Presentation points, transition
    /// points and everything else PowerPoint adds for its own layout engine
    /// are ignored.
    pub fn parse(data: &Node) -> Model {
        let pts: &[Node] = data.child("ptLst").map(|l| l.children.as_slice()).unwrap_or(&[]);
        let kind = |p: &Node| p.attr("type").unwrap_or("node").to_string();
        let mut by_id: HashMap<&str, usize> = HashMap::new();
        for (i, p) in pts.iter().enumerate() {
            if p.name == "pt" && matches!(kind(p).as_str(), "node" | "asst" | "doc") {
                if let Some(id) = p.attr("modelId") {
                    by_id.insert(id, i);
                }
            }
        }
        let mut kids: HashMap<usize, Vec<(i64, usize)>> = HashMap::new();
        let mut has_parent: HashSet<usize> = HashSet::new();
        for c in data.child("cxnLst").into_iter().flat_map(|l| l.children_named("cxn")) {
            if c.attr("type").unwrap_or("parOf") != "parOf" {
                continue;
            }
            let (Some(&src), Some(&dest)) = (
                c.attr("srcId").and_then(|s| by_id.get(s)),
                c.attr("destId").and_then(|d| by_id.get(d)),
            ) else {
                continue;
            };
            kids.entry(src).or_default().push((c.attr_i64("srcOrd").unwrap_or(0), dest));
            has_parent.insert(dest);
        }
        for v in kids.values_mut() {
            v.sort_by_key(|k| k.0);
        }
        let doc = pts.iter().position(|p| p.name == "pt" && kind(p) == "doc");
        let top: Vec<usize> = match doc {
            Some(d) => kids.get(&d).map(|v| v.iter().map(|k| k.1).collect()).unwrap_or_default(),
            None => (0..pts.len())
                .filter(|i| by_id.values().any(|v| v == i) && !has_parent.contains(i))
                .collect(),
        };

        let mut model = Model {
            layout_id: doc
                .and_then(|d| pts[d].child("prSet"))
                .and_then(|s| s.attr("loTypeId"))
                .map(String::from),
            ..Model::default()
        };
        // Depth-first, children in order; `seen` stops connection cycles.
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<(usize, Option<usize>, usize)> = top.iter().rev().map(|&p| (p, None, 0)).collect();
        while let Some((pt, parent, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                model.truncated = true;
                continue;
            }
            if model.nodes.len() >= MAX_NODES {
                model.truncated = true;
                break;
            }
            if !seen.insert(pt) {
                continue;
            }
            let idx = model.nodes.len();
            model.nodes.push(DNode {
                pt,
                assistant: kind(&pts[pt]) == "asst",
                children: Vec::new(),
                text: plain_text(&pts[pt]),
            });
            match parent {
                Some(p) => model.nodes[p].children.push(idx),
                None => model.roots.push(idx),
            }
            for k in kids.get(&pt).into_iter().flatten().rev() {
                stack.push((k.1, Some(idx), depth + 1));
            }
        }
        model
    }

    /// The descendants of `node` that carry text, in reading order.
    fn descendants(&self, node: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.nodes[node].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if !self.nodes[n].text.trim().is_empty() {
                out.push(n);
            }
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }
}

/// The text of a point: the runs of `pt/t`, one line per paragraph.
fn plain_text(pt: &Node) -> String {
    let Some(t) = pt.child("t") else { return String::new() };
    t.children_named("p")
        .map(|p| {
            p.children
                .iter()
                .map(|r| match r.name.as_str() {
                    "r" | "fld" => r.child("t").map(|t| t.text.as_str()).unwrap_or(""),
                    "br" => "\n",
                    _ => "",
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when the node's text carries an explicit font size (`sz`), which
/// PowerPoint keeps instead of fitting the text to the box.
pub fn has_explicit_size(t: &Node) -> bool {
    t.children_named("p")
        .any(|p| p.children.iter().filter_map(|c| c.child("rPr")).any(|r| r.attr("sz").is_some()))
}

// ---------------------------------------------------------------- families

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// "Basic Block List": boxes in rows.
    BlockGrid,
    /// Bars with their children in a box below. Also the fallback.
    VerticalList,
    /// Columns with a header bar and the children below it.
    HorizontalList,
    /// Boxes left to right with arrows.
    Process,
    /// Boxes top to bottom with arrows.
    VerticalProcess,
    /// Overlapping chevrons left to right.
    Chevrons,
    Cycle,
    /// The first node in the middle, its children around it.
    Radial,
    Hierarchy,
    Matrix,
}

/// Picks a family from the layout's identifier
/// (`urn:microsoft.com/office/officeart/2005/8/layout/hList1`; a `#2` suffix
/// marks a second diagram built from the same layout). Names it does not know
/// get the vertical list.
pub fn family_of(layout_id: &str) -> Family {
    let name = layout_id.rsplit('/').next().unwrap_or("");
    let name = name.split('#').next().unwrap_or("").to_ascii_lowercase();
    let starts = |p: &str| name.starts_with(p);
    if starts("radial") {
        Family::Radial
    } else if starts("cycle") {
        Family::Cycle
    } else if starts("hierarchy") || starts("orgchart") {
        Family::Hierarchy
    } else if starts("matrix") {
        Family::Matrix
    } else if starts("chevron") || starts("hchevron") {
        Family::Chevrons
    } else if starts("vprocess") || name == "process2" {
        Family::VerticalProcess
    } else if starts("process") || starts("hprocess") || starts("bprocess") || starts("arrow") {
        Family::Process
    } else if starts("hlist") {
        Family::HorizontalList
    } else if name == "default" {
        Family::BlockGrid
    } else {
        Family::VerticalList
    }
}

// ---------------------------------------------------------------- layout

/// What a piece is, so the caller can pick its colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A node: accent fill, light text.
    Primary,
    /// A box of child text: light fill, dark text.
    Secondary,
    /// An arrow between nodes.
    Arrow,
    /// A connecting line.
    Link,
}

/// Which nodes' text goes into a piece, and where.
#[derive(Debug, Clone)]
pub struct Label {
    pub heading: Option<usize>,
    /// Shown as bulleted paragraphs after the heading.
    pub bullets: Vec<usize>,
    pub rect: Rect,
}

/// One shape of the laid-out diagram, in EMU relative to the frame's corner.
#[derive(Debug, Clone)]
pub struct Piece {
    pub rect: Rect,
    pub rotation: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub geometry: Geometry,
    pub role: Role,
    /// Position in the color list (repeats when the list is shorter).
    pub color: usize,
    pub label: Option<Label>,
}

impl Piece {
    fn new(rect: Rect, geometry: Geometry, role: Role, color: usize) -> Piece {
        Piece {
            rect,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            geometry,
            role,
            color,
            label: None,
        }
    }

    fn labelled(mut self, heading: Option<usize>, bullets: Vec<usize>, rect: Rect) -> Piece {
        self.label = Some(Label { heading, bullets, rect });
        self
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// A polygon in unit coordinates.
fn polygon(points: &[(f32, f32)]) -> Geometry {
    let mut v: Vec<PathCmd> = points
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| if i == 0 { PathCmd::Move(x, y) } else { PathCmd::Line(x, y) })
        .collect();
    v.push(PathCmd::Close);
    Geometry::Path(v)
}

/// A right-pointing block arrow filling its box.
fn arrow_geometry() -> Geometry {
    polygon(&[
        (0.0, 0.25),
        (0.55, 0.25),
        (0.55, 0.0),
        (1.0, 0.5),
        (0.55, 1.0),
        (0.55, 0.75),
        (0.0, 0.75),
    ])
}

/// A block arrow of length `len` and thickness `thick` centred on (`cx`,
/// `cy`), turned `angle` degrees clockwise from pointing right.
fn arrow(cx: f32, cy: f32, len: f32, thick: f32, angle: f32, color: usize) -> Piece {
    let mut p = Piece::new(
        rect(cx - len / 2.0, cy - thick / 2.0, len, thick),
        arrow_geometry(),
        Role::Arrow,
        color,
    );
    p.rotation = angle.rem_euclid(360.0);
    p
}

/// A straight line from (`x0`, `y0`) to (`x1`, `y1`).
fn link(x0: f32, y0: f32, x1: f32, y1: f32) -> Piece {
    let mut p = Piece::new(
        rect(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()),
        Geometry::Line,
        Role::Link,
        0,
    );
    // A line runs from the box's top-left to its bottom-right corner.
    p.flip_h = (x1 - x0) * (y1 - y0) < 0.0;
    p
}

/// The text box of a node drawn in `r`: the whole box, or the part of an
/// ellipse a rectangle can use.
fn text_box(r: Rect, geometry: &Geometry) -> Rect {
    match geometry {
        Geometry::Ellipse => {
            let (w, h) = (r.w * 0.72, r.h * 0.72);
            rect(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h)
        }
        _ => r,
    }
}

/// The diameter of the nodes on a ring of `n` of them in a square of side
/// `m`: `share` of the square, but never so large that neighbours overlap.
fn ring_node(m: f32, n: usize, share: f32) -> f32 {
    let base = m * share;
    if n < 3 {
        return base;
    }
    base.min(m * 0.5 * (std::f32::consts::PI / n as f32).sin() * 1.05)
}

/// A node drawn as one box with its children's text as bullets.
fn node_piece(model: &Model, node: usize, r: Rect, geometry: Geometry, color: usize) -> Piece {
    let tb = text_box(r, &geometry);
    Piece::new(r, geometry, Role::Primary, color).labelled(Some(node), model.descendants(node), tb)
}

/// Lays `model` out in a frame of `w` x `h` EMU as `family` says. Pieces come
/// in drawing order (lines and arrows under the boxes they join).
pub fn layout(model: &Model, family: Family, w: f32, h: f32) -> Vec<Piece> {
    if model.roots.is_empty() || !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return Vec::new();
    }
    match family {
        Family::BlockGrid => block_grid(model, w, h),
        Family::VerticalList => vertical_list(model, w, h),
        Family::HorizontalList => horizontal_list(model, w, h),
        Family::Process => process(model, w, h),
        Family::VerticalProcess => vertical_process(model, w, h),
        Family::Chevrons => chevrons(model, w, h),
        Family::Cycle => cycle(model, w, h),
        Family::Radial => radial(model, w, h),
        Family::Hierarchy => hierarchy(model, w, h),
        Family::Matrix => matrix(model, w, h),
    }
}

/// Boxes of `aspect` (height per width) in rows, the last row centred.
fn block_grid(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    const ASPECT: f32 = 0.6;
    let n = model.roots.len();
    let mut best = (1usize, 0.0f32);
    for cols in 1..=n {
        let rows = n.div_ceil(cols) as f32;
        let c = cols as f32;
        let bw = (w / (c + GAP * (c - 1.0))).min(h / (rows * ASPECT + GAP * (rows - 1.0)));
        if bw > best.1 {
            best = (cols, bw);
        }
    }
    let (cols, bw) = best;
    let bh = bw * ASPECT;
    let gap = bw * GAP;
    let rows = n.div_ceil(cols);
    let y0 = (h - (rows as f32 * bh + (rows - 1) as f32 * gap)) / 2.0;
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let in_row = (n - row * cols).min(cols) as f32;
        let x0 = (w - (in_row * bw + (in_row - 1.0) * gap)) / 2.0;
        let r = rect(x0 + col as f32 * (bw + gap), y0 + row as f32 * (bh + gap), bw, bh);
        out.push(node_piece(model, node, r, Geometry::Rect, i));
    }
    out
}

/// A grid filling the frame; the basic matrix puts four boxes in a square.
fn matrix(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let n = model.roots.len();
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let gap = w.min(h) * 0.03;
    let (cw, ch) = (
        (w - gap * (cols - 1) as f32) / cols as f32,
        (h - gap * (rows - 1) as f32) / rows as f32,
    );
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let in_row = (n - row * cols).min(cols) as f32;
        let x0 = (w - (in_row * cw + (in_row - 1.0) * gap)) / 2.0;
        let r = rect(x0 + col as f32 * (cw + gap), row as f32 * (ch + gap), cw, ch);
        out.push(node_piece(model, node, r, Geometry::Rect, i));
    }
    out
}

/// Bars stacked top to bottom; a node with children gets a box of their text
/// under its bar.
fn vertical_list(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    const GROUP_GAP: f32 = 0.2;
    // Heights in units of one bar.
    let child_units = |node: usize| -> f32 {
        let k = model.descendants(node).len();
        if k == 0 {
            0.0
        } else {
            0.4 + 0.3 * k.min(8) as f32
        }
    };
    let total: f32 = model.roots.iter().map(|&n| 1.0 + child_units(n)).sum::<f32>() + GROUP_GAP * (model.roots.len() - 1) as f32;
    // A single bar stays bar-shaped instead of filling a tall frame.
    let unit = (h / total).min(w * 0.2);
    let mut y = (h - unit * total) / 2.0;
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let bar = rect(0.0, y, w, unit);
        out.push(Piece::new(bar, Geometry::RoundRect(0.16667), Role::Primary, i).labelled(Some(node), Vec::new(), bar));
        y += unit;
        let cu = child_units(node);
        if cu > 0.0 {
            let b = rect(w * 0.03, y, w * 0.94, unit * cu);
            out.push(Piece::new(b, Geometry::Rect, Role::Secondary, i).labelled(None, model.descendants(node), b));
            y += unit * cu;
        }
        y += unit * GROUP_GAP;
    }
    out
}

/// Columns: a header bar on top, the children's text below it.
fn horizontal_list(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let n = model.roots.len() as f32;
    let cw = w / (n + GAP * (n - 1.0));
    let gap = cw * GAP;
    let any_children = model.roots.iter().any(|&r| !model.descendants(r).is_empty());
    let (hh, top) = if any_children {
        ((h * 0.25).min(cw * 0.5), 0.0)
    } else {
        let hh = h.min(cw * 0.6);
        (hh, (h - hh) / 2.0)
    };
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let x = i as f32 * (cw + gap);
        let head = rect(x, top, cw, hh);
        out.push(Piece::new(head, Geometry::Rect, Role::Primary, i).labelled(Some(node), Vec::new(), head));
        let below = model.descendants(node);
        if !below.is_empty() {
            let b = rect(x, top + hh, cw, h - hh);
            out.push(Piece::new(b, Geometry::Rect, Role::Secondary, i).labelled(None, below, b));
        }
    }
    out
}

/// Boxes left to right, a block arrow in each gap.
fn process(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    const ARROW: f32 = 0.4;
    let n = model.roots.len() as f32;
    let bw = w / (n + ARROW * (n - 1.0));
    let bh = h.min(bw * 0.6);
    let y = (h - bh) / 2.0;
    let aw = bw * ARROW;
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let x = i as f32 * (bw + aw);
        if i > 0 {
            let len = aw * 0.7;
            out.push(arrow(x - aw / 2.0, y + bh / 2.0, len, (bh * 0.5).min(len * 1.4), 0.0, i));
        }
        out.push(node_piece(model, node, rect(x, y, bw, bh), Geometry::RoundRect(0.16667), i));
    }
    out
}

fn vertical_process(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    const ARROW: f32 = 0.4;
    let n = model.roots.len() as f32;
    let bh = (h / (n + ARROW * (n - 1.0))).min(w * 0.35);
    let ah = bh * ARROW;
    let bw = w.min(bh * 3.0);
    let x = (w - bw) / 2.0;
    let y0 = (h - (n * bh + (n - 1.0) * ah)) / 2.0;
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let y = y0 + i as f32 * (bh + ah);
        if i > 0 {
            let len = ah * 0.7;
            out.push(arrow(x + bw / 2.0, y - ah / 2.0, len, (bw * 0.2).min(len * 1.4), 90.0, i));
        }
        out.push(node_piece(model, node, rect(x, y, bw, bh), Geometry::RoundRect(0.16667), i));
    }
    out
}

/// Chevrons that nest into one another from left to right.
fn chevrons(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let n = model.roots.len() as f32;
    let ch = h.min(0.6 * w / n);
    let point = ch * 0.5;
    let gap = ch * 0.05;
    // w = (n - 1) * (cw - point + gap) + cw
    let cw = (w + (n - 1.0) * (point - gap)) / n;
    let step = cw - point + gap;
    let y = (h - ch) / 2.0;
    let a = (point / cw).clamp(0.0, 0.5);
    let shape = polygon(&[(0.0, 0.0), (1.0 - a, 0.0), (1.0, 0.5), (1.0 - a, 1.0), (0.0, 1.0), (a, 0.5)]);
    let mut out = Vec::new();
    for (i, &node) in model.roots.iter().enumerate() {
        let r = rect(i as f32 * step, y, cw, ch);
        // The text stays clear of the point and the notch.
        let tb = rect(r.x + point, r.y, (r.w - 2.0 * point).max(r.w * 0.2), r.h);
        out.push(Piece::new(r, shape.clone(), Role::Primary, i).labelled(Some(node), model.descendants(node), tb));
    }
    out
}

/// Ellipses on a circle, an arrow between neighbours (clockwise from the top).
fn cycle(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let n = model.roots.len();
    let m = w.min(h);
    let (cx, cy) = (w / 2.0, h / 2.0);
    if n == 1 {
        let s = m * 0.5;
        return vec![node_piece(
            model,
            model.roots[0],
            rect(cx - s / 2.0, cy - s / 2.0, s, s),
            Geometry::Ellipse,
            0,
        )];
    }
    let nf = n as f32;
    let s = ring_node(m, n, 0.28);
    let radius = m / 2.0 - s / 2.0;
    let at = |deg: f32| {
        let (sin, cos) = deg.to_radians().sin_cos();
        (cx + radius * sin, cy - radius * cos)
    };
    let mut out = Vec::new();
    for i in 0..n {
        let mid = 360.0 * (i as f32 + 0.5) / nf;
        let (x, y) = at(mid);
        let len = (s * 0.3).min(radius * 2.0 * (std::f32::consts::PI / nf).sin() * 0.25);
        out.push(arrow(x, y, len, len * 0.8, mid, i));
    }
    for (i, &node) in model.roots.iter().enumerate() {
        let (x, y) = at(360.0 * i as f32 / nf);
        out.push(node_piece(model, node, rect(x - s / 2.0, y - s / 2.0, s, s), Geometry::Ellipse, i));
    }
    out
}

/// The first node in the middle, joined by lines to its children and to the
/// other top-level nodes.
fn radial(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let m = w.min(h);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let centre = model.roots[0];
    // The centre's children, then any further top-level nodes.
    let around: Vec<usize> = model.nodes[centre].children.iter().chain(&model.roots[1..]).copied().collect();
    let hub_geometry = Geometry::Ellipse;
    if around.is_empty() {
        let s = m * 0.5;
        return vec![node_piece(model, centre, rect(cx - s / 2.0, cy - s / 2.0, s, s), hub_geometry, 0)];
    }
    let k = around.len() as f32;
    let s = ring_node(m, around.len(), 0.26);
    let radius = m / 2.0 - s / 2.0;
    let hub = (2.0 * (radius - s / 2.0) * 0.72).min(m * 0.36);
    let mut out = Vec::new();
    let positions: Vec<(f32, f32)> = (0..around.len())
        .map(|j| {
            let (sin, cos) = (360.0 * j as f32 / k).to_radians().sin_cos();
            (cx + radius * sin, cy - radius * cos)
        })
        .collect();
    for &(x, y) in &positions {
        out.push(link(cx, cy, x, y));
    }
    // The children are the nodes around the hub, not a list inside it.
    let mut hub_piece = node_piece(model, centre, rect(cx - hub / 2.0, cy - hub / 2.0, hub, hub), hub_geometry, 0);
    if let Some(l) = &mut hub_piece.label {
        l.bullets.clear();
    }
    out.push(hub_piece);
    for (j, (&node, &(x, y))) in around.iter().zip(&positions).enumerate() {
        out.push(
            Piece::new(rect(x - s / 2.0, y - s / 2.0, s, s), Geometry::Ellipse, Role::Primary, j + 1).labelled(
                Some(node),
                model.descendants(node),
                text_box(rect(x - s / 2.0, y - s / 2.0, s, s), &Geometry::Ellipse),
            ),
        );
    }
    out
}

/// A placed node of a hierarchy: its level and its horizontal slot.
struct Placed {
    node: usize,
    level: usize,
    x: f32,
    /// Indices into the placed list: assistants first, then the regular children.
    assistants: Vec<usize>,
    regulars: Vec<usize>,
}

/// Places `node` and everything under it. Leaves take consecutive slots; a
/// parent sits above the middle of its children. Assistants go one level
/// below the parent and its other children one level lower still.
fn place(model: &Model, node: usize, level: usize, slot: &mut f32, out: &mut Vec<Placed>) -> usize {
    let idx = out.len();
    out.push(Placed {
        node,
        level,
        x: 0.0,
        assistants: Vec::new(),
        regulars: Vec::new(),
    });
    let children = &model.nodes[node].children;
    let has_assistants = children.iter().any(|&c| model.nodes[c].assistant);
    for &c in children {
        let is_assistant = model.nodes[c].assistant;
        let child_level = if is_assistant || !has_assistants { level + 1 } else { level + 2 };
        let ci = place(model, c, child_level, slot, out);
        if is_assistant {
            out[idx].assistants.push(ci);
        } else {
            out[idx].regulars.push(ci);
        }
    }
    let kids: Vec<usize> = out[idx].assistants.iter().chain(&out[idx].regulars).copied().collect();
    out[idx].x = match (kids.first(), kids.last()) {
        (Some(&a), Some(&b)) => (out[a].x + out[b].x) / 2.0,
        _ => {
            let x = *slot;
            *slot += 1.0;
            x
        }
    };
    idx
}

/// An organisation chart: boxes by level, orthogonal lines to the children.
fn hierarchy(model: &Model, w: f32, h: f32) -> Vec<Piece> {
    let mut placed = Vec::new();
    let mut slot = 0.0f32;
    for &r in &model.roots {
        place(model, r, 0, &mut slot, &mut placed);
    }
    let leaves = slot.max(1.0);
    let levels = placed.iter().map(|p| p.level).max().unwrap_or(0) + 1;
    let bw = (w / (leaves + 0.3 * (leaves - 1.0))).min(w * 0.4);
    let pitch = bw * 1.3;
    let lv = levels as f32;
    let bh = (h / (lv + 0.6 * (lv - 1.0))).min(bw * 0.7);
    let gap_y = if levels > 1 { ((h - lv * bh) / (lv - 1.0)).min(bh) } else { 0.0 };
    let x0 = (w - ((leaves - 1.0) * pitch + bw)) / 2.0;
    let y0 = (h - (lv * bh + (lv - 1.0) * gap_y)) / 2.0;
    let left = |p: &Placed| x0 + p.x * pitch;
    let top = |level: usize| y0 + level as f32 * (bh + gap_y);

    let mut out = Vec::new();
    for p in &placed {
        let (cx, bottom) = (left(p) + bw / 2.0, top(p.level) + bh);
        for group in [&p.assistants, &p.regulars] {
            let Some(&first) = group.first() else { continue };
            let child_top = top(placed[first].level);
            let bar_y = child_top - (child_top - bottom) / 2.0;
            let xs: Vec<f32> = group.iter().map(|&c| left(&placed[c]) + bw / 2.0).collect();
            out.push(link(cx, bottom, cx, bar_y));
            let (lo, hi) = (xs.iter().copied().fold(cx, f32::min), xs.iter().copied().fold(cx, f32::max));
            if hi > lo {
                out.push(link(lo, bar_y, hi, bar_y));
            }
            for x in xs {
                out.push(link(x, bar_y, x, child_top));
            }
        }
    }
    for p in &placed {
        let r = rect(left(p), top(p.level), bw, bh);
        out.push(Piece::new(r, Geometry::Rect, Role::Primary, p.level).labelled(Some(p.node), Vec::new(), r));
    }
    out
}

// ---------------------------------------------------------------- text fit

/// Smallest and largest font size (points) the fit may choose.
const MIN_PT: f32 = 5.0;
const MAX_PT: f32 = 65.0;

/// Hanging indent of a bulleted paragraph, in ems of its first run: the
/// bullet and the gap after it grow with the text.
const BULLET_EM: f32 = 0.9;

fn first_size(p: &Paragraph) -> f32 {
    p.runs.iter().find(|r| !r.line_break).map(|r| r.size).unwrap_or(p.end_size)
}

/// Sets the hanging indent of every bulleted paragraph for the body's final
/// `font_scale`. Call it after the scale is chosen.
pub fn finish_bullets(body: &mut TextBody) {
    let scale = body.font_scale;
    for p in body.paragraphs.iter_mut().filter(|p| p.bullet.is_some()) {
        let em = BULLET_EM * first_size(p) * scale * EMU_PER_PT;
        p.margin_left = em;
        p.indent = -em;
    }
}

/// Greedy line breaking at spaces, the way the PDF writer does it.
struct Wrap {
    first_w: f32,
    rest_w: f32,
    lines: usize,
    line_w: f32,
    word_w: f32,
    /// Width of the space before the current word.
    space: f32,
    /// A word was wider than a whole line and had to be split.
    broken: bool,
}

impl Wrap {
    fn avail(&self) -> f32 {
        if self.lines == 1 {
            self.first_w
        } else {
            self.rest_w
        }
    }

    fn end_word(&mut self) {
        if self.word_w > 0.0 {
            if self.line_w == 0.0 {
                self.line_w = self.word_w;
            } else if self.line_w + self.space + self.word_w <= self.avail() {
                self.line_w += self.space + self.word_w;
            } else {
                self.lines += 1;
                self.line_w = self.word_w;
            }
            let avail = self.avail();
            if self.line_w > avail && avail > 0.0 {
                self.broken = true;
                let extra = (self.line_w / avail).ceil() as usize - 1;
                self.lines += extra;
                self.line_w -= extra as f32 * avail;
            }
        }
        self.word_w = 0.0;
        self.space = 0.0;
    }

    fn new_line(&mut self) {
        self.end_word();
        self.lines += 1;
        self.line_w = 0.0;
    }
}

/// Height in points of `body` laid out at `scale` in `avail_w` points, and
/// whether a word had to be broken because it is wider than the line.
fn text_height(body: &TextBody, avail_w: f32, scale: f32) -> (f32, bool) {
    let mut total = 0.0;
    let mut broken = false;
    for (pi, p) in body.paragraphs.iter().enumerate() {
        let marl = if p.bullet.is_some() {
            BULLET_EM * first_size(p) * scale
        } else {
            p.margin_left / EMU_PER_PT
        };
        let mut w = Wrap {
            first_w: avail_w - if p.bullet.is_some() { marl } else { marl + p.indent / EMU_PER_PT },
            rest_w: avail_w - marl,
            lines: 1,
            line_w: 0.0,
            word_w: 0.0,
            space: 0.0,
            broken: false,
        };
        let mut max_size = 0.0f32;
        for r in &p.runs {
            if r.line_break {
                w.new_line();
                continue;
            }
            let size = r.size * scale;
            max_size = max_size.max(size);
            for c in r.text.chars() {
                match c {
                    '\n' | '\u{b}' | '\u{2028}' => w.new_line(),
                    ' ' => {
                        w.end_word();
                        w.space = crate::pdf::builtin_advance(r, ' ') * size;
                    }
                    _ => w.word_w += crate::pdf::builtin_advance(r, c) * size,
                }
            }
        }
        w.end_word();
        broken |= w.broken;
        if max_size == 0.0 {
            max_size = p.end_size * scale;
        }
        let line_h = match p.line_spacing_pt {
            Some(pt) => pt * scale,
            None => max_size * 1.2 * p.line_spacing * (1.0 - body.line_reduction),
        };
        let before = if pi == 0 { 0.0 } else { p.space_before * scale };
        let after = if pi + 1 < body.paragraphs.len() {
            p.space_after * scale
        } else {
            0.0
        };
        total += before + w.lines as f32 * line_h + after;
    }
    (total, broken)
}

/// The font scale (1.0 = the sizes in `body`) at which `body` fills a box of
/// `w` x `h` EMU: the largest up to `max_scale` that fits, never below
/// [`MIN_PT`]. PowerPoint sizes SmartArt text this way.
pub fn fit_scale(body: &TextBody, w: f32, h: f32, max_scale: f32) -> f32 {
    let avail_w = (w - body.insets[0] - body.insets[2]) / EMU_PER_PT;
    let avail_h = (h - body.insets[1] - body.insets[3]) / EMU_PER_PT;
    let base = body
        .paragraphs
        .iter()
        .flat_map(|p| p.runs.iter().map(|r| r.size))
        .fold(0.0, f32::max)
        .max(1.0);
    let floor = (MIN_PT / base).min(max_scale);
    let fits = |s: f32| {
        let (height, broken) = text_height(body, avail_w, s);
        height <= avail_h && !broken
    };
    if avail_w <= 0.0 || avail_h <= 0.0 || !fits(floor) {
        return floor;
    }
    if fits(max_scale) {
        return max_scale;
    }
    let (mut lo, mut hi) = (floor, max_scale);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The largest scale worth trying for text whose biggest run is `base` points
/// (`explicit`: the author chose the size, so the text only ever shrinks).
pub fn max_scale(base: f32, explicit: bool) -> f32 {
    if explicit {
        1.0
    } else {
        (MAX_PT / base.max(1.0)).max(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Align, Anchor, Family as FontFamily, Rgb, Run};
    use crate::xml;

    const W: f32 = 6_096_000.0;
    const H: f32 = 4_064_000.0;

    /// A data model from `(text, parent, assistant)` rows: `parent` is the
    /// row index of the parent, `None` for a child of the document node.
    fn model(rows: &[(&str, Option<usize>, bool)], layout: &str) -> Model {
        let mut pts = format!(r#"<dgm:pt modelId="doc" type="doc"><dgm:prSet loTypeId="{layout}"/></dgm:pt>"#);
        let mut cxns = String::new();
        for (i, (text, parent, asst)) in rows.iter().enumerate() {
            let ty = if *asst { r#" type="asst""# } else { "" };
            pts += &format!(r#"<dgm:pt modelId="n{i}"{ty}><dgm:t><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></dgm:t></dgm:pt>"#);
            let src = parent.map(|p| format!("n{p}")).unwrap_or_else(|| "doc".into());
            cxns += &format!(r#"<dgm:cxn modelId="c{i}" srcId="{src}" destId="n{i}" srcOrd="{i}" destOrd="0"/>"#);
            // Transition points and presentation connections are not nodes.
            pts += &format!(r#"<dgm:pt modelId="p{i}" type="parTrans" cxnId="c{i}"/>"#);
            cxns += &format!(r#"<dgm:cxn modelId="x{i}" type="presOf" srcId="n{i}" destId="p{i}" srcOrd="0" destOrd="0"/>"#);
        }
        let xml = format!(
            r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>{pts}</dgm:ptLst><dgm:cxnLst>{cxns}</dgm:cxnLst></dgm:dataModel>"#
        );
        Model::parse(&xml::parse(xml.as_bytes()).unwrap())
    }

    fn flat(texts: &[&str], layout: &str) -> Model {
        let rows: Vec<_> = texts.iter().map(|t| (*t, None, false)).collect();
        model(&rows, layout)
    }

    fn of(pieces: &[Piece], role: Role) -> Vec<&Piece> {
        pieces.iter().filter(|p| p.role == role).collect()
    }

    fn inside(r: &Rect) -> bool {
        let e = 1.0;
        r.x >= -e && r.y >= -e && r.x + r.w <= W + e && r.y + r.h <= H + e
    }

    fn overlap(a: &Rect, b: &Rect) -> bool {
        a.x < b.x + b.w - 1.0 && b.x < a.x + a.w - 1.0 && a.y < b.y + b.h - 1.0 && b.y < a.y + a.h - 1.0
    }

    fn no_overlaps(pieces: &[&Piece]) {
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                assert!(!overlap(&a.rect, &b.rect), "{:?} overlaps {:?}", a.rect, b.rect);
            }
        }
    }

    fn heading(m: &Model, p: &Piece) -> String {
        m.nodes[p.label.as_ref().unwrap().heading.unwrap()].text.clone()
    }

    #[test]
    fn model_reads_the_tree_in_connection_order() {
        let m = model(
            &[
                ("a", None, false),
                ("a1", Some(0), false),
                ("b", None, false),
                ("a2", Some(0), false),
            ],
            "urn:x/layout/default",
        );
        assert_eq!(m.roots.len(), 2);
        let a = &m.nodes[m.roots[0]];
        assert_eq!(a.text, "a");
        let kids: Vec<_> = a.children.iter().map(|&c| m.nodes[c].text.as_str()).collect();
        assert_eq!(kids, ["a1", "a2"]);
        assert_eq!(m.nodes[m.roots[1]].text, "b");
        assert_eq!(m.layout_id.as_deref(), Some("urn:x/layout/default"));
        // Transition points are not nodes.
        assert_eq!(m.nodes.len(), 4);
    }

    #[test]
    fn model_survives_cycles_and_runaway_depth() {
        // n0 -> n1 -> n0: each node is visited once.
        let xml = r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
            <dgm:pt modelId="doc" type="doc"/><dgm:pt modelId="n0"/><dgm:pt modelId="n1"/></dgm:ptLst>
            <dgm:cxnLst><dgm:cxn srcId="doc" destId="n0" srcOrd="0"/><dgm:cxn srcId="n0" destId="n1" srcOrd="0"/>
            <dgm:cxn srcId="n1" destId="n0" srcOrd="0"/></dgm:cxnLst></dgm:dataModel>"#;
        let m = Model::parse(&xml::parse(xml.as_bytes()).unwrap());
        assert_eq!(m.nodes.len(), 2);

        // A chain deeper than MAX_DEPTH is cut and says so.
        let rows: Vec<(&str, Option<usize>, bool)> = (0..30).map(|i| ("x", if i == 0 { None } else { Some(i - 1) }, false)).collect();
        let m = model(&rows, "");
        assert!(m.truncated);
        assert_eq!(m.nodes.len(), MAX_DEPTH + 1);

        // So is a flat list longer than MAX_NODES.
        let rows: Vec<(&str, Option<usize>, bool)> = (0..MAX_NODES + 50).map(|_| ("x", None, false)).collect();
        let m = model(&rows, "");
        assert!(m.truncated);
        assert_eq!(m.nodes.len(), MAX_NODES);
    }

    #[test]
    fn layout_names_pick_families() {
        let id = |n: &str| format!("urn:microsoft.com/office/officeart/2005/8/layout/{n}");
        for (name, want) in [
            ("default", Family::BlockGrid),
            ("vList5", Family::VerticalList),
            ("vList2", Family::VerticalList),
            ("lProcess2", Family::VerticalList),
            ("hList1", Family::HorizontalList),
            ("process1", Family::Process),
            ("arrow5#2", Family::Process),
            ("vProcess5", Family::VerticalProcess),
            ("chevron1", Family::Chevrons),
            ("hChevron3", Family::Chevrons),
            ("cycle2", Family::Cycle),
            ("radial3", Family::Radial),
            ("orgChart1", Family::Hierarchy),
            ("hierarchy4", Family::Hierarchy),
            ("matrix1", Family::Matrix),
            ("pyramid1", Family::VerticalList),
        ] {
            assert_eq!(family_of(&id(name)), want, "{name}");
        }
        assert_eq!(family_of(""), Family::VerticalList);
    }

    #[test]
    fn block_grid_fits_every_box_without_overlap() {
        let m = flat(&["A", "B", "C", "D", "E"], "default");
        let p = layout(&m, Family::BlockGrid, W, H);
        let boxes = of(&p, Role::Primary);
        assert_eq!(boxes.len(), 5);
        assert!(boxes.iter().all(|b| inside(&b.rect)));
        no_overlaps(&boxes);
        // Reading order: rows top to bottom, left to right.
        let order: Vec<_> = boxes.iter().map(|b| heading(&m, b)).collect();
        assert_eq!(order, ["A", "B", "C", "D", "E"]);
        assert!(boxes[0].rect.y < boxes[4].rect.y);
    }

    #[test]
    fn vertical_list_stacks_bars_with_their_children_below() {
        let m = model(
            &[("a", None, false), ("b", Some(0), false), ("c", Some(0), false), ("x", None, false)],
            "vList2",
        );
        let p = layout(&m, Family::VerticalList, W, H);
        let bars = of(&p, Role::Primary);
        let lists = of(&p, Role::Secondary);
        assert_eq!((bars.len(), lists.len()), (2, 1));
        assert!(bars[0].rect.y + bars[0].rect.h <= lists[0].rect.y + 1.0);
        assert!(lists[0].rect.y + lists[0].rect.h <= bars[1].rect.y + 1.0);
        let bullets = &lists[0].label.as_ref().unwrap().bullets;
        let text: Vec<_> = bullets.iter().map(|&b| m.nodes[b].text.as_str()).collect();
        assert_eq!(text, ["b", "c"]);
        assert!(p.iter().all(|q| inside(&q.rect)));
        no_overlaps(&p.iter().collect::<Vec<_>>());
    }

    #[test]
    fn a_lone_bar_keeps_its_shape() {
        let p = layout(&flat(&["only"], "vList"), Family::VerticalList, W, H);
        assert_eq!(p.len(), 1);
        assert!(p[0].rect.h <= p[0].rect.w * 0.21, "{:?}", p[0].rect);
    }

    #[test]
    fn horizontal_list_makes_a_column_per_node() {
        let m = model(
            &[
                ("a", None, false),
                ("a1", Some(0), false),
                ("b", None, false),
                ("b1", Some(2), false),
            ],
            "hList1",
        );
        let p = layout(&m, Family::HorizontalList, W, H);
        let heads = of(&p, Role::Primary);
        let bodies = of(&p, Role::Secondary);
        assert_eq!((heads.len(), bodies.len()), (2, 2));
        assert!(heads[0].rect.x < heads[1].rect.x);
        assert_eq!(heads[0].rect.x, bodies[0].rect.x);
        assert!(heads[0].rect.y + heads[0].rect.h <= bodies[0].rect.y + 1.0);
        no_overlaps(&p.iter().collect::<Vec<_>>());
    }

    #[test]
    fn process_joins_boxes_with_arrows_left_to_right() {
        let m = flat(&["one", "two", "three"], "process1");
        let p = layout(&m, Family::Process, W, H);
        let boxes = of(&p, Role::Primary);
        let arrows = of(&p, Role::Arrow);
        assert_eq!((boxes.len(), arrows.len()), (3, 2));
        assert!(boxes.windows(2).all(|w| w[0].rect.x + w[0].rect.w < w[1].rect.x));
        for (i, a) in arrows.iter().enumerate() {
            let (l, r) = (&boxes[i].rect, &boxes[i + 1].rect);
            let mid = a.rect.x + a.rect.w / 2.0;
            assert!(mid > l.x + l.w && mid < r.x, "arrow {i} sits between its boxes");
            assert_eq!(a.rotation, 0.0);
        }
        assert!(p.iter().all(|q| inside(&q.rect)));
        // Boxes stay in the frame and the first starts at its edge.
        assert!(boxes[0].rect.x.abs() < 1.0 && (boxes[2].rect.x + boxes[2].rect.w - W).abs() < 1.0);
    }

    #[test]
    fn vertical_process_points_down() {
        let p = layout(&flat(&["one", "two"], "vProcess5"), Family::VerticalProcess, W, H);
        let boxes = of(&p, Role::Primary);
        let arrows = of(&p, Role::Arrow);
        assert_eq!((boxes.len(), arrows.len()), (2, 1));
        assert!(boxes[0].rect.y + boxes[0].rect.h < boxes[1].rect.y);
        assert_eq!(arrows[0].rotation, 90.0);
    }

    #[test]
    fn chevrons_nest_and_keep_their_text_clear_of_the_points() {
        let m = flat(&["a", "b", "c"], "chevron1");
        let p = layout(&m, Family::Chevrons, W, H);
        assert_eq!(p.len(), 3);
        assert!(p.windows(2).all(|w| w[0].rect.x < w[1].rect.x));
        // Neighbours overlap in x (they nest) but their text boxes do not.
        assert!(p[0].rect.x + p[0].rect.w > p[1].rect.x);
        let (t0, t1) = (p[0].label.as_ref().unwrap().rect, p[1].label.as_ref().unwrap().rect);
        assert!(t0.x + t0.w <= t1.x);
        assert!(p.iter().all(|q| inside(&q.rect)));
        assert!(matches!(&p[0].geometry, Geometry::Path(v) if v.len() == 7));
    }

    #[test]
    fn cycle_spreads_nodes_on_a_circle_with_arrows_between() {
        let m = flat(&["a", "b", "c", "d", "e"], "cycle2");
        let p = layout(&m, Family::Cycle, W, H);
        let nodes = of(&p, Role::Primary);
        let arrows = of(&p, Role::Arrow);
        assert_eq!((nodes.len(), arrows.len()), (5, 5));
        let (cx, cy) = (W / 2.0, H / 2.0);
        let dist = |r: &Rect| ((r.x + r.w / 2.0 - cx).powi(2) + (r.y + r.h / 2.0 - cy).powi(2)).sqrt();
        let d0 = dist(&nodes[0].rect);
        assert!(nodes.iter().all(|n| (dist(&n.rect) - d0).abs() < 1.0));
        // The first node is at the top, the second to its right.
        assert!(nodes[0].rect.y < cy && nodes[1].rect.x > nodes[0].rect.x);
        assert!(nodes.iter().all(|n| matches!(n.geometry, Geometry::Ellipse) && inside(&n.rect)));
        no_overlaps(&nodes);
    }

    #[test]
    fn radial_puts_the_first_node_in_the_middle() {
        let m = model(
            &[
                ("hub", None, false),
                ("a", Some(0), false),
                ("b", Some(0), false),
                ("c", Some(0), false),
            ],
            "radial3",
        );
        let p = layout(&m, Family::Radial, W, H);
        let nodes = of(&p, Role::Primary);
        assert_eq!(nodes.len(), 4);
        assert_eq!(heading(&m, nodes[0]), "hub");
        let hub = nodes[0].rect;
        assert!((hub.x + hub.w / 2.0 - W / 2.0).abs() < 1.0 && (hub.y + hub.h / 2.0 - H / 2.0).abs() < 1.0);
        assert_eq!(of(&p, Role::Link).len(), 3);
        no_overlaps(&nodes);
        assert!(nodes.iter().all(|n| inside(&n.rect)));
    }

    #[test]
    fn matrix_fills_a_two_by_two_grid() {
        let p = layout(&flat(&["a", "b", "c", "d"], "matrix1"), Family::Matrix, W, H);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].rect.y, p[1].rect.y);
        assert!(p[2].rect.y > p[0].rect.y + p[0].rect.h - 1.0);
        assert!(p[1].rect.x > p[0].rect.x + p[0].rect.w - 1.0);
        no_overlaps(&p.iter().collect::<Vec<_>>());
        assert!(p.iter().all(|q| inside(&q.rect)));
    }

    #[test]
    fn hierarchy_puts_children_below_their_parent_and_joins_them() {
        let m = model(
            &[
                ("boss", None, false),
                ("l", Some(0), false),
                ("r", Some(0), false),
                ("l1", Some(1), false),
                ("l2", Some(1), false),
            ],
            "orgChart1",
        );
        let p = layout(&m, Family::Hierarchy, W, H);
        let boxes = of(&p, Role::Primary);
        assert_eq!(boxes.len(), 5);
        let by = |t: &str| boxes.iter().find(|b| heading(&m, b) == t).unwrap().rect;
        let (boss, l, r, l1, l2) = (by("boss"), by("l"), by("r"), by("l1"), by("l2"));
        assert!(boss.y + boss.h < l.y && l.y == r.y && l.y + l.h < l1.y);
        // A parent is centred over its children.
        let mid = |a: &Rect, b: &Rect| (a.x + a.w / 2.0 + b.x + b.w / 2.0) / 2.0;
        assert!((boss.x + boss.w / 2.0 - mid(&l, &r)).abs() < 1.0);
        assert!((l.x + l.w / 2.0 - mid(&l1, &l2)).abs() < 1.0);
        no_overlaps(&boxes);
        assert!(p.iter().all(|q| inside(&q.rect)));
        // Lines come first, so the boxes are drawn over them.
        assert!(p.iter().position(|q| q.role == Role::Primary).unwrap() >= of(&p, Role::Link).len());
        // Every line is axis-aligned.
        assert!(of(&p, Role::Link).iter().all(|q| q.rect.w == 0.0 || q.rect.h == 0.0));
    }

    #[test]
    fn hierarchy_puts_assistants_between_the_parent_and_its_other_children() {
        let m = model(
            &[("boss", None, false), ("helper", Some(0), true), ("report", Some(0), false)],
            "orgChart1",
        );
        let p = layout(&m, Family::Hierarchy, W, H);
        let boxes = of(&p, Role::Primary);
        let by = |t: &str| boxes.iter().find(|b| heading(&m, b) == t).unwrap().rect;
        let (boss, helper, report) = (by("boss"), by("helper"), by("report"));
        assert!(boss.y < helper.y && helper.y < report.y, "three levels");
        no_overlaps(&boxes);
    }

    #[test]
    fn empty_or_degenerate_input_lays_out_nothing() {
        let m = flat(&["a"], "x");
        assert!(layout(&Model::default(), Family::Process, W, H).is_empty());
        assert!(layout(&m, Family::Process, 0.0, H).is_empty());
        assert!(layout(&m, Family::Process, f32::NAN, H).is_empty());
    }

    #[test]
    fn every_family_handles_one_node_and_many() {
        for family in [
            Family::BlockGrid,
            Family::VerticalList,
            Family::HorizontalList,
            Family::Process,
            Family::VerticalProcess,
            Family::Chevrons,
            Family::Cycle,
            Family::Radial,
            Family::Hierarchy,
            Family::Matrix,
        ] {
            for n in [1usize, 2, 7, 40] {
                let texts: Vec<String> = (0..n).map(|i| format!("n{i}")).collect();
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                let p = layout(&flat(&refs, ""), family, W, H);
                assert!(!p.is_empty(), "{family:?} {n}");
                for q in &p {
                    let r = q.rect;
                    assert!(
                        [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()) && r.w >= 0.0 && r.h >= 0.0,
                        "{family:?} {n}: {r:?}"
                    );
                }
            }
        }
    }

    fn body(text: &str, size: f32) -> TextBody {
        TextBody {
            insets: [91440.0, 45720.0, 91440.0, 45720.0],
            rotation: 0.0,
            anchor: Anchor::Middle,
            wrap: true,
            font_scale: 1.0,
            line_reduction: 0.0,
            paragraphs: vec![Paragraph {
                align: Align::Center,
                margin_left: 0.0,
                indent: 0.0,
                bullet: None,
                line_spacing: 1.0,
                line_spacing_pt: None,
                space_before: 0.0,
                space_after: 0.0,
                runs: vec![Run {
                    text: text.into(),
                    size,
                    bold: false,
                    italic: false,
                    underline: false,
                    color: Rgb::BLACK,
                    family: FontFamily::Sans,
                    face: "Calibri".into(),
                    line_break: false,
                }],
                end_size: size,
            }],
        }
    }

    #[test]
    fn text_is_sized_to_fill_its_box() {
        let b = body("A short label", 18.0);
        let (w, h) = (2_000_000.0, 1_000_000.0);
        let s = fit_scale(&b, w, h, 3.0);
        // It fits at the chosen scale and would not fit a little larger.
        let (avail_w, avail_h) = ((w - 182_880.0) / EMU_PER_PT, (h - 91_440.0) / EMU_PER_PT);
        let (height, broken) = text_height(&b, avail_w, s);
        assert!(height <= avail_h && !broken);
        if s < 3.0 {
            let (height, broken) = text_height(&b, avail_w, s * 1.05);
            assert!(height > avail_h || broken, "scale {s} could grow");
        }
        // A longer text in the same box gets a smaller scale.
        let long = body("A much longer label that has to wrap over several lines to fit", 18.0);
        assert!(fit_scale(&long, w, h, 3.0) < s);
    }

    #[test]
    fn text_never_grows_past_the_limit_and_never_vanishes() {
        let b = body("x", 18.0);
        assert_eq!(fit_scale(&b, 8_000_000.0, 8_000_000.0, max_scale(18.0, false)), MAX_PT / 18.0);
        assert_eq!(fit_scale(&b, 8_000_000.0, 8_000_000.0, max_scale(18.0, true)), 1.0);
        let tiny = fit_scale(&body("a very long text indeed", 18.0), 300_000.0, 200_000.0, 3.6);
        assert!((tiny * 18.0 - MIN_PT).abs() < 0.01, "floor at {MIN_PT}pt, got {}", tiny * 18.0);
    }
}
