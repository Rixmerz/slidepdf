//! Outlines of the DrawingML preset shapes that need more than a polygon
//! from a fixed list of fractions: arrows, callouts, stars, flow-chart
//! symbols, math signs, ribbons and the rest of the `prstGeom` catalogue.
//!
//! Each arm follows the guide formulas of ECMA-376 `presetShapeDefinitions`
//! (`avLst` adjust values, `gdLst` guides, `pathLst` path), evaluated in the
//! shape's own units so the adjust values keep their meaning on non-square
//! boxes, and then scaled to the unit box the IR uses. The long arms keep the
//! spec's guide names and formulas (pruned to the guides their path uses),
//! so that they can be read side by side with the definition.
//!
//! The IR holds one path and one fill rule per shape, while a preset has
//! several paths with their own fill and stroke. What is kept of them:
//!
//! - the filled (`norm`) paths are the outline;
//! - strokes that only add detail (a callout's leader, a cube's edges) are
//!   appended retraced ([`Pen::back`]), so they cost no fill area;
//! - the shaded overlay paths (`darken`, `lighten`) are dropped when they
//!   lie inside the body, and filled in the body's colour, wound the same way
//!   ([`Pen::flip`]), where they are part of it (curved arrows, ribbons);
//! - action-button icons are holes in the button (even-odd fill).

use crate::ir::{Geometry, PathCmd, Rect};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// A path under construction in shape units (0..w, 0..h).
struct Pen {
    w: f32,
    h: f32,
    cmds: Vec<PathCmd>,
    cur: (f32, f32),
    start: (f32, f32),
    /// Where the current subpath starts in `cmds`.
    sub: usize,
}

impl Pen {
    fn new(w: f32, h: f32) -> Self {
        Pen {
            w,
            h,
            cmds: Vec::new(),
            cur: (0.0, 0.0),
            start: (0.0, 0.0),
            sub: 0,
        }
    }

    fn m(&mut self, x: f32, y: f32) {
        self.sub = self.cmds.len();
        self.cmds.push(PathCmd::Move(x / self.w, y / self.h));
        self.cur = (x, y);
        self.start = (x, y);
    }

    fn l(&mut self, x: f32, y: f32) {
        self.cmds.push(PathCmd::Line(x / self.w, y / self.h));
        self.cur = (x, y);
    }

    fn cubic(&mut self, a: (f32, f32), b: (f32, f32), p: (f32, f32)) {
        let (w, h) = (self.w, self.h);
        self.cmds.push(PathCmd::Cubic(a.0 / w, a.1 / h, b.0 / w, b.1 / h, p.0 / w, p.1 / h));
        self.cur = p;
    }

    /// Quadratic Bézier to `p` through control point `q`.
    fn quad(&mut self, q: (f32, f32), p: (f32, f32)) {
        let c = self.cur;
        let third = |a: f32, b: f32| a + 2.0 / 3.0 * (b - a);
        self.cubic((third(c.0, q.0), third(c.1, q.1)), (third(p.0, q.0), third(p.1, q.1)), p);
    }

    fn close(&mut self) {
        self.cmds.push(PathCmd::Close);
        self.cur = self.start;
    }

    /// Retraces the open subpath back to its start. A stroke-only detail line
    /// then encloses no area, so filling the whole path (the IR has one fill
    /// rule and one path per shape) neither paints nor cancels anything.
    fn back(&mut self) {
        let sub = &self.cmds[self.sub..];
        let mut from = Vec::new();
        for c in sub {
            match *c {
                PathCmd::Move(x, y) | PathCmd::Line(x, y) | PathCmd::Cubic(_, _, _, _, x, y) => from.push((x, y)),
                PathCmd::Close => return,
            }
        }
        let mut back = Vec::new();
        for i in (1..sub.len()).rev() {
            let to = from[i - 1];
            back.push(match sub[i] {
                PathCmd::Cubic(a, b, c, d, _, _) => PathCmd::Cubic(c, d, a, b, to.0, to.1),
                _ => PathCmd::Line(to.0, to.1),
            });
        }
        self.cmds.extend(back);
        self.cur = self.start;
    }

    /// Winds the current subpath the other way round, so that overlay
    /// pieces drawn on top of the body add to it under the non-zero rule
    /// instead of cancelling it.
    fn flip(&mut self) {
        let seg: Vec<PathCmd> = self.cmds.drain(self.sub..).collect();
        let closed = seg.last() == Some(&PathCmd::Close);
        let mut pts = Vec::new();
        for c in &seg {
            match *c {
                PathCmd::Move(x, y) | PathCmd::Line(x, y) | PathCmd::Cubic(_, _, _, _, x, y) => pts.push((x, y)),
                PathCmd::Close => {}
            }
        }
        let Some(&last) = pts.last() else { return };
        self.cmds.push(PathCmd::Move(last.0, last.1));
        for i in (1..seg.len() - usize::from(closed)).rev() {
            let to = pts[i - 1];
            self.cmds.push(match seg[i] {
                PathCmd::Cubic(a, b, c, d, _, _) => PathCmd::Cubic(c, d, a, b, to.0, to.1),
                _ => PathCmd::Line(to.0, to.1),
            });
        }
        if closed {
            self.cmds.push(PathCmd::Close);
        }
        self.start = last;
        self.cur = if closed { last } else { pts[0] };
    }

    /// Closed polygon.
    fn poly(&mut self, pts: &[(f32, f32)]) {
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 {
                self.m(x, y);
            } else {
                self.l(x, y);
            }
        }
        self.close();
    }

    /// Open straight stroke (detail lines of bevels, cubes, buttons).
    fn seg(&mut self, a: (f32, f32), b: (f32, f32)) {
        self.m(a.0, a.1);
        self.l(b.0, b.1);
    }

    /// `arcTo`: an elliptical arc that starts at the current point, which sits
    /// at the visual angle `st` (degrees, clockwise from +x) of an ellipse
    /// with radii `wr` x `hr`, and sweeps `sw` degrees.
    fn arc(&mut self, wr: f32, hr: f32, st: f32, sw: f32) {
        self.arc_in(1.0, 1.0, wr, hr, st, sw);
    }

    /// `arcTo` in a path whose own coordinate space is scaled by `sx` x `sy`
    /// to the shape: the radii are in path units, and so is the meaning of
    /// the visual angles.
    fn arc_in(&mut self, sx: f32, sy: f32, wr: f32, hr: f32, st: f32, sw: f32) {
        let t0 = ellipse_param(wr, hr, st);
        let sweep = sw.to_radians();
        let mut dt = ellipse_param(wr, hr, st + sw) - t0;
        while dt - sweep > PI {
            dt -= TAU;
        }
        while dt - sweep < -PI {
            dt += TAU;
        }
        let (wr, hr) = (wr * sx, hr * sy);
        let (cx, cy) = (self.cur.0 - wr * t0.cos(), self.cur.1 - hr * t0.sin());
        let n = ((dt.abs() / FRAC_PI_2).ceil() as usize).max(1);
        let step = dt / n as f32;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        for i in 0..n {
            let (a, b) = (t0 + i as f32 * step, t0 + (i + 1) as f32 * step);
            let at = |t: f32, d: f32| (cx + wr * (t.cos() - d * t.sin()), cy + hr * (t.sin() + d * t.cos()));
            self.cubic(at(a, k), at(b, -k), at(b, 0.0));
        }
    }

    fn path(self) -> Geometry {
        Geometry::Path(self.cmds)
    }
}

/// Parametric angle of the ellipse point seen at visual angle `ang`.
fn ellipse_param(wr: f32, hr: f32, ang: f32) -> f32 {
    let a = ang.to_radians();
    (wr * a.sin()).atan2(hr * a.cos())
}

/// The point of the ellipse centred on `c` seen at visual angle `ang`.
fn ellipse_point(c: (f32, f32), wr: f32, hr: f32, ang: f32) -> (f32, f32) {
    let t = ellipse_param(wr, hr, ang);
    (c.0 + wr * t.cos(), c.1 + hr * t.sin())
}

/// `pin lo v hi`, in the spec's order (so `lo > hi` does not panic).
fn pin3(lo: f32, v: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// `pin 0 v max`.
fn pin(v: f32, max: f32) -> f32 {
    pin3(0.0, v, max)
}

// The guide-formula operators of DrawingML. Angles are 60000ths of a degree.

/// `n / d`, and 0 rather than infinity when `d` is 0 (the spec leaves it open).
fn div(n: f32, d: f32) -> f32 {
    if d == 0.0 {
        0.0
    } else {
        n / d
    }
}

fn at2(x: f32, y: f32) -> f32 {
    y.atan2(x).to_degrees() * 60000.0
}

fn cat2(x: f32, y: f32, z: f32) -> f32 {
    x * z.atan2(y).cos()
}

fn sat2(x: f32, y: f32, z: f32) -> f32 {
    x * z.atan2(y).sin()
}

fn cos60(x: f32, a: f32) -> f32 {
    x * (a / 60000.0).to_radians().cos()
}

fn sin60(x: f32, a: f32) -> f32 {
    x * (a / 60000.0).to_radians().sin()
}

fn tan60(x: f32, a: f32) -> f32 {
    x * (a / 60000.0).to_radians().tan()
}

/// The outline of preset `prst` in a box of `r`, or `None` for names this
/// module does not draw. `raw(name, default)` reads an adjust value in the
/// file's own units (`val 16667` is 16667; angles are 60000ths of a degree).
pub(crate) fn outline(prst: &str, raw: &dyn Fn(&str, f32) -> f32, r: Rect) -> Option<Geometry> {
    let (w, h) = (r.w.max(1.0), r.h.max(1.0));
    let ss = w.min(h);
    // Adjust as a fraction (100000 = 1.0).
    let adj = |name: &str, def: f32| raw(name, def) / 100000.0;
    // Adjust as an angle in degrees.
    let ang = |name: &str, def: f32| raw(name, def) / 60000.0;
    let mut p = Pen::new(w, h);
    match prst {
        "snip1Rect" => {
            let d = ss * pin(adj("adj", 16667.0), 0.5);
            p.poly(&[(0.0, 0.0), (w - d, 0.0), (w, d), (w, h), (0.0, h)]);
        }
        "snip2SameRect" => {
            let (t, b) = (ss * pin(adj("adj1", 16667.0), 0.5), ss * pin(adj("adj2", 0.0), 0.5));
            p.poly(&[
                (t, 0.0),
                (w - t, 0.0),
                (w, t),
                (w, h - b),
                (w - b, h),
                (b, h),
                (0.0, h - b),
                (0.0, t),
            ]);
        }
        "snip2DiagRect" => {
            let (a, b) = (ss * pin(adj("adj1", 0.0), 0.5), ss * pin(adj("adj2", 16667.0), 0.5));
            p.poly(&[
                (a, 0.0),
                (w - b, 0.0),
                (w, b),
                (w, h - a),
                (w - a, h),
                (b, h),
                (0.0, h - b),
                (0.0, a),
            ]);
        }
        "snipRoundRect" => {
            let (a, b) = (ss * pin(adj("adj1", 16667.0), 0.5), ss * pin(adj("adj2", 16667.0), 0.5));
            p.m(a, 0.0);
            p.l(w - b, 0.0);
            p.l(w, b);
            p.l(w, h);
            p.l(0.0, h);
            p.l(0.0, a);
            p.arc(a, a, 180.0, 90.0);
            p.close();
        }
        "foldedCorner" => {
            let d2 = ss * pin(adj("adj", 16667.0), 0.5);
            let (x1, y2) = (w - d2, h - d2);
            let (x2, y1) = (x1 + d2 / 5.0, y2 + d2 / 5.0);
            p.poly(&[(0.0, 0.0), (w, 0.0), (w, y2), (x1, h), (0.0, h)]);
            // The turned-up flap.
            p.m(x1, h);
            p.l(x2, y1);
            p.l(w, y2);
            p.back();
        }
        "bevel" => {
            let a = ss * pin(adj("adj", 12500.0), 0.5);
            let (x2, y2) = (w - a, h - a);
            p.poly(&[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]);
            p.poly(&[(a, a), (x2, a), (x2, y2), (a, y2)]);
            p.seg((0.0, 0.0), (a, a));
            p.seg((0.0, h), (a, y2));
            p.seg((w, 0.0), (x2, a));
            p.seg((w, h), (x2, y2));
        }
        "cube" => {
            let y1 = ss * pin(adj("adj", 25000.0), 1.0);
            let (y4, x4) = (h - y1, w - y1);
            p.poly(&[(0.0, y1), (y1, 0.0), (w, 0.0), (w, y4), (x4, h), (0.0, h)]);
            p.m(0.0, y1);
            p.l(x4, y1);
            p.l(w, 0.0);
            p.back();
            p.seg((x4, y1), (x4, h));
        }
        "pieWedge" => {
            p.m(0.0, h);
            p.arc(w, h, 180.0, 90.0);
            p.l(w, h);
            p.close();
        }
        "chord" | "arc" | "blockArc" => {
            let (def_st, def_en) = match prst {
                "chord" => (2700000.0, 16200000.0),
                "arc" => (16200000.0, 0.0),
                _ => (10800000.0, 0.0),
            };
            let st = ang("adj1", def_st).clamp(0.0, 359.99998);
            let en = ang("adj2", def_en).clamp(0.0, 359.99998);
            let mut sw = en - st;
            if sw <= 0.0 {
                sw += 360.0;
            }
            let c = (w / 2.0, h / 2.0);
            let s = ellipse_point(c, w / 2.0, h / 2.0, st);
            p.m(s.0, s.1);
            match prst {
                "chord" => {
                    p.arc(w / 2.0, h / 2.0, st, sw);
                    p.close();
                }
                "arc" => {
                    p.arc(w / 2.0, h / 2.0, st, sw);
                    return Some(Geometry::Open(p.cmds));
                }
                _ => {
                    let d = ss * pin(adj("adj3", 25000.0), 0.5);
                    let (iw, ih) = (w / 2.0 - d, h / 2.0 - d);
                    p.arc(w / 2.0, h / 2.0, st, sw);
                    let e = ellipse_point(c, iw, ih, en);
                    p.l(e.0, e.1);
                    p.arc(iw, ih, en, -sw);
                    p.close();
                }
            }
        }
        "bentArrow" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), 0.5);
            let (th, aw2, ah) = (ss * a1, ss * a2, ss * a3);
            let dh2 = aw2 - th / 2.0;
            let bs = (w - ah).min(h - dh2);
            let bd = ss * pin(adj("adj4", 43750.0), bs / ss);
            let bd2 = (bd - th).max(0.0);
            let (x3, x4, y3) = (th + bd2, w - ah, dh2 + th);
            p.m(0.0, h);
            p.l(0.0, dh2 + bd);
            p.arc(bd, bd, 180.0, 90.0);
            p.l(x4, dh2);
            p.l(x4, 0.0);
            p.l(w, aw2);
            p.l(x4, y3 + dh2);
            p.l(x4, y3);
            p.l(x3, y3);
            p.arc(bd2, bd2, 270.0, -90.0);
            p.l(th, h);
            p.close();
        }
        "bentUpArrow" => {
            let a1 = pin(adj("adj1", 25000.0), 0.5);
            let a2 = pin(adj("adj2", 25000.0), 0.5);
            let y1 = ss * pin(adj("adj3", 25000.0), 0.5);
            let (x1, x3) = (w - ss * a2 * 2.0, w - ss * a2);
            let (x2, x4) = (x3 - ss * a1 / 2.0, x3 + ss * a1 / 2.0);
            let y2 = h - ss * a1;
            p.poly(&[
                (0.0, y2),
                (x2, y2),
                (x2, y1),
                (x1, y1),
                (x3, 0.0),
                (w, y1),
                (x4, y1),
                (x4, h),
                (0.0, h),
            ]);
        }
        "uturnArrow" => {
            let a2 = pin(adj("adj2", 25000.0), 0.25);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), (1.0 - a1 * ss / h) * h / ss);
            let a5 = pin3((a3 + a1) * ss / h, adj("adj5", 75000.0), 1.0);
            let (th, aw2) = (ss * a1, ss * a2);
            let dh2 = aw2 - th / 2.0;
            let (y5, ah) = (h * a5, ss * a3);
            let y4 = y5 - ah;
            let x9 = w - dh2;
            let bs = (x9 / 2.0).min(y4);
            let bd = ss * pin(adj("adj4", 43750.0), bs / ss);
            let bd2 = (bd - th).max(0.0);
            let (x3, x8) = (th + bd2, w - aw2);
            let x6 = x8 - aw2;
            let x7 = x6 + dh2;
            p.m(0.0, h);
            p.l(0.0, bd);
            p.arc(bd, bd, 180.0, 90.0);
            p.l(x9 - bd, 0.0);
            p.arc(bd, bd, 270.0, 90.0);
            p.l(x9, y4);
            p.l(w, y4);
            p.l(x8, y5);
            p.l(x6, y4);
            p.l(x7, y4);
            p.l(x7, x3);
            p.arc(bd2, bd2, 0.0, -90.0);
            p.l(x3, th);
            p.arc(bd2, bd2, 270.0, -90.0);
            p.l(th, h);
            p.close();
        }
        "leftUpArrow" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5);
            let max1 = a2 * 2.0;
            let a1 = pin(adj("adj1", 25000.0), max1);
            let a3 = pin(adj("adj3", 25000.0), 1.0 - max1);
            let x1 = ss * a3;
            let (x2, y2) = (w - ss * a2 * 2.0, h - ss * a2 * 2.0);
            let (x4, y4) = (w - ss * a2, h - ss * a2);
            let dx3 = ss * a1 / 2.0;
            p.poly(&[
                (0.0, y4),
                (x1, y2),
                (x1, y4 - dx3),
                (x4 - dx3, y4 - dx3),
                (x4 - dx3, x1),
                (x2, x1),
                (x4, 0.0),
                (w, x1),
                (x4 + dx3, x1),
                (x4 + dx3, y4 + dx3),
                (x1, y4 + dx3),
                (x1, h),
            ]);
        }
        "leftRightUpArrow" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5);
            let max1 = a2 * 2.0;
            let a1 = pin(adj("adj1", 25000.0), max1);
            let x1 = ss * pin(adj("adj3", 25000.0), (1.0 - max1) / 2.0);
            let dx2 = ss * a2;
            let (x2, x5) = (w / 2.0 - dx2, w / 2.0 + dx2);
            let dx3 = ss * a1 / 2.0;
            let (x3, x4, x6) = (w / 2.0 - dx3, w / 2.0 + dx3, w - x1);
            let y2 = h - ss * a2 * 2.0;
            let y4 = h - dx2;
            let (y3, y5) = (y4 - dx3, y4 + dx3);
            p.poly(&[
                (0.0, y4),
                (x1, y2),
                (x1, y3),
                (x3, y3),
                (x3, x1),
                (x2, x1),
                (w / 2.0, 0.0),
                (x5, x1),
                (x4, x1),
                (x4, y3),
                (x6, y3),
                (x6, y2),
                (w, y4),
                (x6, h),
                (x6, y5),
                (x1, y5),
                (x1, h),
            ]);
        }
        "quadArrow" => {
            let a2 = pin(adj("adj2", 22500.0), 0.5);
            let max1 = a2 * 2.0;
            let a1 = pin(adj("adj1", 22500.0), max1);
            let x1 = ss * pin(adj("adj3", 22500.0), (1.0 - max1) / 2.0);
            let (hc, vc) = (w / 2.0, h / 2.0);
            let dx2 = ss * a2;
            let (x2, x5, y2, y5) = (hc - dx2, hc + dx2, vc - dx2, vc + dx2);
            let dx3 = ss * a1 / 2.0;
            let (x3, x4, y3, y4) = (hc - dx3, hc + dx3, vc - dx3, vc + dx3);
            let (x6, y6) = (w - x1, h - x1);
            p.poly(&[
                (0.0, vc),
                (x1, y2),
                (x1, y3),
                (x3, y3),
                (x3, x1),
                (x2, x1),
                (hc, 0.0),
                (x5, x1),
                (x4, x1),
                (x4, y3),
                (x6, y3),
                (x6, y2),
                (w, vc),
                (x6, y5),
                (x6, y4),
                (x4, y4),
                (x4, y6),
                (x5, y6),
                (hc, h),
                (x2, y6),
                (x3, y6),
                (x3, y4),
                (x1, y4),
                (x1, y5),
            ]);
        }
        "upDownArrow" => {
            let a1 = pin(adj("adj1", 50000.0), 1.0);
            let y2 = ss * pin(adj("adj2", 50000.0), 0.5 * h / ss);
            let y3 = h - y2;
            let (x1, x2) = (w / 2.0 - w * a1 / 2.0, w / 2.0 + w * a1 / 2.0);
            p.poly(&[
                (0.0, y2),
                (w / 2.0, 0.0),
                (w, y2),
                (x2, y2),
                (x2, y3),
                (w, y3),
                (w / 2.0, h),
                (0.0, y3),
                (x1, y3),
                (x1, y2),
            ]);
        }
        "leftArrowCallout" | "rightArrowCallout" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5 * h / ss);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), w / ss);
            let a4 = pin(adj("adj4", 64977.0), 1.0 - a3 * ss / w);
            let (dy1, dy2) = (ss * a2, ss * a1 / 2.0);
            let (y1, y2, y3, y4) = (h / 2.0 - dy1, h / 2.0 - dy2, h / 2.0 + dy2, h / 2.0 + dy1);
            let vc = h / 2.0;
            if prst == "leftArrowCallout" {
                let (x1, x2) = (ss * a3, w - w * a4);
                p.poly(&[
                    (0.0, vc),
                    (x1, y1),
                    (x1, y2),
                    (x2, y2),
                    (x2, 0.0),
                    (w, 0.0),
                    (w, h),
                    (x2, h),
                    (x2, y3),
                    (x1, y3),
                    (x1, y4),
                ]);
            } else {
                let (x2, x3) = (w * a4, w - ss * a3);
                p.poly(&[
                    (0.0, 0.0),
                    (x2, 0.0),
                    (x2, y2),
                    (x3, y2),
                    (x3, y1),
                    (w, vc),
                    (x3, y4),
                    (x3, y3),
                    (x2, y3),
                    (x2, h),
                    (0.0, h),
                ]);
            }
        }
        "upArrowCallout" | "downArrowCallout" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5 * w / ss);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), h / ss);
            let a4 = pin(adj("adj4", 64977.0), 1.0 - a3 * ss / h);
            let (dx1, dx2, hc) = (ss * a2, ss * a1 / 2.0, w / 2.0);
            let (x1, x2, x3, x4) = (hc - dx1, hc - dx2, hc + dx2, hc + dx1);
            if prst == "upArrowCallout" {
                let (y1, y2) = (ss * a3, h - h * a4);
                p.poly(&[
                    (0.0, y2),
                    (x2, y2),
                    (x2, y1),
                    (x1, y1),
                    (hc, 0.0),
                    (x4, y1),
                    (x3, y1),
                    (x3, y2),
                    (w, y2),
                    (w, h),
                    (0.0, h),
                ]);
            } else {
                let (y3, y2) = (h - ss * a3, h * a4);
                p.poly(&[
                    (0.0, 0.0),
                    (w, 0.0),
                    (w, y2),
                    (x3, y2),
                    (x3, y3),
                    (x4, y3),
                    (hc, h),
                    (x1, y3),
                    (x2, y3),
                    (x2, y2),
                    (0.0, y2),
                ]);
            }
        }
        "leftRightArrowCallout" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5 * h / ss);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), 0.5 * w / ss);
            let a4 = pin(adj("adj4", 48123.0), 1.0 - a3 * ss / (w / 2.0));
            let (vc, hc) = (h / 2.0, w / 2.0);
            let (dy1, dy2) = (ss * a2, ss * a1 / 2.0);
            let (y1, y2, y3, y4) = (vc - dy1, vc - dy2, vc + dy2, vc + dy1);
            let (x1, dx2) = (ss * a3, w * a4 / 2.0);
            let (x4, x2, x3) = (w - x1, hc - dx2, hc + dx2);
            p.poly(&[
                (0.0, vc),
                (x1, y1),
                (x1, y2),
                (x2, y2),
                (x2, 0.0),
                (x3, 0.0),
                (x3, y2),
                (x4, y2),
                (x4, y1),
                (w, vc),
                (x4, y4),
                (x4, y3),
                (x3, y3),
                (x3, h),
                (x2, h),
                (x2, y3),
                (x1, y3),
                (x1, y4),
            ]);
        }
        "upDownArrowCallout" => {
            let a2 = pin(adj("adj2", 25000.0), 0.5 * w / ss);
            let a1 = pin(adj("adj1", 25000.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 25000.0), 0.5 * h / ss);
            let a4 = pin(adj("adj4", 48123.0), 1.0 - a3 * ss / (h / 2.0));
            let (hc, vc) = (w / 2.0, h / 2.0);
            let (dx1, dx2) = (ss * a2, ss * a1 / 2.0);
            let (x1, x2, x3, x4) = (hc - dx1, hc - dx2, hc + dx2, hc + dx1);
            let (y1, dy2) = (ss * a3, h * a4 / 2.0);
            let (y4, y2, y3) = (h - y1, vc - dy2, vc + dy2);
            p.poly(&[
                (0.0, y2),
                (x2, y2),
                (x2, y1),
                (x1, y1),
                (hc, 0.0),
                (x4, y1),
                (x3, y1),
                (x3, y2),
                (w, y2),
                (w, y3),
                (x3, y3),
                (x3, y4),
                (x4, y4),
                (hc, h),
                (x1, y4),
                (x2, y4),
                (x2, y3),
                (0.0, y3),
            ]);
        }
        "quadArrowCallout" => {
            let a2 = pin(adj("adj2", 18515.0), 0.5);
            let a1 = pin(adj("adj1", 18515.0), a2 * 2.0);
            let a3 = pin(adj("adj3", 18515.0), 0.5 - a2);
            let a4 = pin3(a1, adj("adj4", 48123.0), 1.0 - a3 * 2.0);
            let (hc, vc) = (w / 2.0, h / 2.0);
            let (dx2, dx3, ah) = (ss * a2, ss * a1 / 2.0, ss * a3);
            let (dx1, dy1) = (w * a4 / 2.0, h * a4 / 2.0);
            let (x8, y8) = (w - ah, h - ah);
            let (x2, x7, x3, x6, x4, x5) = (hc - dx1, hc + dx1, hc - dx2, hc + dx2, hc - dx3, hc + dx3);
            let (y2, y7, y3, y6, y4, y5) = (vc - dy1, vc + dy1, vc - dx2, vc + dx2, vc - dx3, vc + dx3);
            p.poly(&[
                (0.0, vc),
                (ah, y3),
                (ah, y4),
                (x2, y4),
                (x2, y2),
                (x4, y2),
                (x4, ah),
                (x3, ah),
                (hc, 0.0),
                (x6, ah),
                (x5, ah),
                (x5, y2),
                (x7, y2),
                (x7, y4),
                (x8, y4),
                (x8, y3),
                (w, vc),
                (x8, y6),
                (x8, y5),
                (x7, y5),
                (x7, y7),
                (x5, y7),
                (x5, y8),
                (x6, y8),
                (hc, h),
                (x3, y8),
                (x4, y8),
                (x4, y7),
                (x2, y7),
                (x2, y5),
                (ah, y5),
                (ah, y6),
            ]);
        }
        "lineInv" => {
            p.m(0.0, h);
            p.l(w, 0.0);
            return Some(Geometry::Open(p.cmds));
        }
        "accentBorderCallout1" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 112500.0);
            let adj4 = raw("adj4", -38333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.back();
        }
        "accentBorderCallout2" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 112500.0);
            let adj6 = raw("adj6", -46667.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.back();
        }
        "accentBorderCallout3" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 100000.0);
            let adj6 = raw("adj6", -16667.0);
            let adj7 = raw("adj7", 112963.0);
            let adj8 = raw("adj8", -8333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            let y4 = h * adj7 / 100000.0;
            let x4 = w * adj8 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.l(x4, y4);
            p.back();
        }
        "accentCallout1" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 112500.0);
            let adj4 = raw("adj4", -38333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.back();
        }
        "accentCallout2" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 112500.0);
            let adj6 = raw("adj6", -46667.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.back();
        }
        "accentCallout3" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 100000.0);
            let adj6 = raw("adj6", -16667.0);
            let adj7 = raw("adj7", 112963.0);
            let adj8 = raw("adj8", -8333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            let y4 = h * adj7 / 100000.0;
            let x4 = w * adj8 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(x1, h);
            p.back();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.l(x4, y4);
            p.back();
        }
        "borderCallout1" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 112500.0);
            let adj4 = raw("adj4", -38333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.back();
        }
        "borderCallout2" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 112500.0);
            let adj6 = raw("adj6", -46667.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.back();
        }
        "borderCallout3" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 100000.0);
            let adj6 = raw("adj6", -16667.0);
            let adj7 = raw("adj7", 112963.0);
            let adj8 = raw("adj8", -8333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            let y4 = h * adj7 / 100000.0;
            let x4 = w * adj8 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.l(x4, y4);
            p.back();
        }
        "bracePair" => {
            let adj = raw("adj", 8333.0);
            let a = pin3(0.0, adj, 25000.0);
            let x1 = ss * a / 100000.0;
            let x2 = ss * a / 50000.0;
            let x3 = w - x2;
            let x4 = w - x1;
            let y2 = (h / 2.0) - x1;
            let y3 = (h / 2.0) + x1;
            let y4 = h - x1;
            p.m(x2, h);
            p.arc(x1, x1, 90.0, 90.0);
            p.l(x1, y3);
            p.arc(x1, x1, 0.0, -90.0);
            p.arc(x1, x1, 90.0, -90.0);
            p.l(x1, x1);
            p.arc(x1, x1, 180.0, 90.0);
            p.l(x3, 0.0);
            p.arc(x1, x1, 270.0, 90.0);
            p.l(x4, y2);
            p.arc(x1, x1, 180.0, -90.0);
            p.arc(x1, x1, 270.0, -90.0);
            p.l(x4, y4);
            p.arc(x1, x1, 0.0, 90.0);
            p.close();
            p.m(x2, h);
            p.arc(x1, x1, 90.0, 90.0);
            p.l(x1, y3);
            p.arc(x1, x1, 0.0, -90.0);
            p.arc(x1, x1, 90.0, -90.0);
            p.l(x1, x1);
            p.arc(x1, x1, 180.0, 90.0);
            p.back();
            p.m(x3, 0.0);
            p.arc(x1, x1, 270.0, 90.0);
            p.l(x4, y2);
            p.arc(x1, x1, 180.0, -90.0);
            p.arc(x1, x1, 270.0, -90.0);
            p.l(x4, y4);
            p.arc(x1, x1, 0.0, 90.0);
            p.back();
        }
        "bracketPair" => {
            let adj = raw("adj", 16667.0);
            let a = pin3(0.0, adj, 50000.0);
            let x1 = ss * a / 100000.0;
            let x2 = w - x1;
            let y2 = h - x1;
            p.m(0.0, x1);
            p.arc(x1, x1, 180.0, 90.0);
            p.l(x2, 0.0);
            p.arc(x1, x1, 270.0, 90.0);
            p.l(w, y2);
            p.arc(x1, x1, 0.0, 90.0);
            p.l(x1, h);
            p.arc(x1, x1, 90.0, 90.0);
            p.close();
            p.m(x1, h);
            p.arc(x1, x1, 90.0, 90.0);
            p.l(0.0, x1);
            p.arc(x1, x1, 180.0, 90.0);
            p.back();
            p.m(x2, 0.0);
            p.arc(x1, x1, 270.0, 90.0);
            p.l(w, y2);
            p.arc(x1, x1, 0.0, 90.0);
            p.back();
        }
        "callout1" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 112500.0);
            let adj4 = raw("adj4", -38333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.back();
        }
        "callout2" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 112500.0);
            let adj6 = raw("adj6", -46667.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.back();
        }
        "callout3" => {
            let adj1 = raw("adj1", 18750.0);
            let adj2 = raw("adj2", -8333.0);
            let adj3 = raw("adj3", 18750.0);
            let adj4 = raw("adj4", -16667.0);
            let adj5 = raw("adj5", 100000.0);
            let adj6 = raw("adj6", -16667.0);
            let adj7 = raw("adj7", 112963.0);
            let adj8 = raw("adj8", -8333.0);
            let y1 = h * adj1 / 100000.0;
            let x1 = w * adj2 / 100000.0;
            let y2 = h * adj3 / 100000.0;
            let x2 = w * adj4 / 100000.0;
            let y3 = h * adj5 / 100000.0;
            let x3 = w * adj6 / 100000.0;
            let y4 = h * adj7 / 100000.0;
            let x4 = w * adj8 / 100000.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, y1);
            p.l(x2, y2);
            p.l(x3, y3);
            p.l(x4, y4);
            p.back();
        }
        "chartPlus" => {
            p.m(5.0 * w / 10.0, 0.0);
            p.l(5.0 * w / 10.0, h);
            p.back();
            p.m(0.0, 5.0 * h / 10.0);
            p.l(w, 5.0 * h / 10.0);
            p.back();
            p.m(0.0, 0.0);
            p.l(0.0, h);
            p.l(w, h);
            p.l(w, 0.0);
            p.close();
        }
        "chartStar" => {
            p.m(0.0, 0.0);
            p.l(w, h);
            p.back();
            p.m(0.0, h);
            p.l(w, 0.0);
            p.back();
            p.m(5.0 * w / 10.0, 0.0);
            p.l(5.0 * w / 10.0, h);
            p.back();
            p.m(0.0, 0.0);
            p.l(0.0, h);
            p.l(w, h);
            p.l(w, 0.0);
            p.close();
        }
        "chartX" => {
            p.m(0.0, 0.0);
            p.l(w, h);
            p.back();
            p.m(0.0, h);
            p.l(w, 0.0);
            p.back();
            p.m(0.0, 0.0);
            p.l(0.0, h);
            p.l(w, h);
            p.l(w, 0.0);
            p.close();
        }
        "circularArrow" => {
            let adj1 = raw("adj1", 12500.0);
            let adj2 = raw("adj2", 1142319.0);
            let adj3 = raw("adj3", 20457680.0);
            let adj4 = raw("adj4", 10800000.0);
            let adj5 = raw("adj5", 12500.0);
            let a5 = pin3(0.0, adj5, 25000.0);
            let max_adj1 = a5 * 2.0;
            let a1 = pin3(0.0, adj1, max_adj1);
            let en_ang = pin3(1.0, adj3, 21600000.0);
            let st_ang = pin3(0.0, adj4, 21600000.0);
            let th = ss * a1 / 100000.0;
            let thh = ss * a5 / 100000.0;
            let th2 = th / 2.0;
            let rw1 = (w / 2.0) + th2 - thh;
            let rh1 = (h / 2.0) + th2 - thh;
            let rw2 = rw1 - th;
            let rh2 = rh1 - th;
            let rw3 = rw2 + th2;
            let rh3 = rh2 + th2;
            let wt_h = sin60(rw3, en_ang);
            let ht_h = cos60(rh3, en_ang);
            let dx_h = cat2(rw3, ht_h, wt_h);
            let dy_h = sat2(rh3, ht_h, wt_h);
            let x_h = (w / 2.0) + dx_h;
            let y_h = (h / 2.0) + dy_h;
            let r_i = rw2.min(rh2);
            let u1 = dx_h * dx_h;
            let u2 = dy_h * dy_h;
            let u3 = r_i * r_i;
            let u4 = u1 - u3;
            let u5 = u2 - u3;
            let u6 = div(u4 * u5, u1);
            let u7 = div(u6, u2);
            let u8 = 1.0 - u7;
            let u9 = u8.max(0.0).sqrt();
            let u10 = div(u4, dx_h);
            let u11 = div(u10, dy_h);
            let u12 = div(1.0 + u9, u11);
            let u13 = at2(1.0, u12);
            let u14 = u13 + 21600000.0;
            let u15 = if u13 > 0.0 { u13 } else { u14 };
            let u16 = u15 - en_ang;
            let u17 = u16 + 21600000.0;
            let u18 = if u16 > 0.0 { u16 } else { u17 };
            let u19 = u18 - 10800000.0;
            let u20 = u18 - 21600000.0;
            let u21 = if u19 > 0.0 { u20 } else { u18 };
            let max_ang = u21.abs();
            let a_ang = pin3(0.0, adj2, max_ang);
            let pt_ang = en_ang + a_ang;
            let wt_a = sin60(rw3, pt_ang);
            let ht_a = cos60(rh3, pt_ang);
            let dx_a = cat2(rw3, ht_a, wt_a);
            let dy_a = sat2(rh3, ht_a, wt_a);
            let x_a = (w / 2.0) + dx_a;
            let y_a = (h / 2.0) + dy_a;
            let wt_e = sin60(rw1, st_ang);
            let ht_e = cos60(rh1, st_ang);
            let dx_e = cat2(rw1, ht_e, wt_e);
            let dy_e = sat2(rh1, ht_e, wt_e);
            let x_e = (w / 2.0) + dx_e;
            let y_e = (h / 2.0) + dy_e;
            let dx_g = cos60(thh, pt_ang);
            let dy_g = sin60(thh, pt_ang);
            let x_g = x_h + dx_g;
            let y_g = y_h + dy_g;
            let dx_b = cos60(thh, pt_ang);
            let dy_b = sin60(thh, pt_ang);
            let x_b = x_h - dx_b;
            let y_b = y_h - dy_b;
            let sx1 = x_b - (w / 2.0);
            let sy1 = y_b - (h / 2.0);
            let sx2 = x_g - (w / 2.0);
            let sy2 = y_g - (h / 2.0);
            let r_o = rw1.min(rh1);
            let x1_o = div(sx1 * r_o, rw1);
            let y1_o = div(sy1 * r_o, rh1);
            let x2_o = div(sx2 * r_o, rw1);
            let y2_o = div(sy2 * r_o, rh1);
            let dx_o = x2_o - x1_o;
            let dy_o = y2_o - y1_o;
            let d_o = dx_o.hypot(dy_o);
            let q1 = x1_o * y2_o;
            let q2 = x2_o * y1_o;
            let v_do = q1 - q2;
            let q3 = r_o * r_o;
            let q4 = d_o * d_o;
            let q5 = q3 * q4;
            let q6 = v_do * v_do;
            let q7 = q5 - q6;
            let q8 = q7.max(0.0);
            let sdel_o = q8.max(0.0).sqrt();
            let ndy_o = -dy_o;
            let sdy_o = if ndy_o > 0.0 { -1.0 } else { 1.0 };
            let q9 = sdy_o * dx_o;
            let q10 = q9 * sdel_o;
            let q11 = v_do * dy_o;
            let dx_f1 = div(q11 + q10, q4);
            let q12 = q11 - q10;
            let dx_f2 = div(q12, q4);
            let ady_o = dy_o.abs();
            let q13 = ady_o * sdel_o;
            let q14 = v_do * dx_o / -1.0;
            let dy_f1 = div(q14 + q13, q4);
            let q15 = q14 - q13;
            let dy_f2 = div(q15, q4);
            let q16 = x2_o - dx_f1;
            let q17 = x2_o - dx_f2;
            let q18 = y2_o - dy_f1;
            let q19 = y2_o - dy_f2;
            let q20 = q16.hypot(q18);
            let q21 = q17.hypot(q19);
            let q22 = q21 - q20;
            let dx_f = if q22 > 0.0 { dx_f1 } else { dx_f2 };
            let dy_f = if q22 > 0.0 { dy_f1 } else { dy_f2 };
            let sdx_f = div(dx_f * rw1, r_o);
            let sdy_f = div(dy_f * rh1, r_o);
            let x_f = (w / 2.0) + sdx_f;
            let y_f = (h / 2.0) + sdy_f;
            let x1_i = div(sx1 * r_i, rw2);
            let y1_i = div(sy1 * r_i, rh2);
            let x2_i = div(sx2 * r_i, rw2);
            let y2_i = div(sy2 * r_i, rh2);
            let dx_i = x2_i - x1_i;
            let dy_i = y2_i - y1_i;
            let d_i = dx_i.hypot(dy_i);
            let v1 = x1_i * y2_i;
            let v2 = x2_i * y1_i;
            let di = v1 - v2;
            let v3 = r_i * r_i;
            let v4 = d_i * d_i;
            let v5 = v3 * v4;
            let v6 = di * di;
            let v7 = v5 - v6;
            let v8 = v7.max(0.0);
            let sdel_i = v8.max(0.0).sqrt();
            let v9 = sdy_o * dx_i;
            let v10 = v9 * sdel_i;
            let v11 = di * dy_i;
            let dx_c1 = div(v11 + v10, v4);
            let v12 = v11 - v10;
            let dx_c2 = div(v12, v4);
            let ady_i = dy_i.abs();
            let v13 = ady_i * sdel_i;
            let v14 = di * dx_i / -1.0;
            let dy_c1 = div(v14 + v13, v4);
            let v15 = v14 - v13;
            let dy_c2 = div(v15, v4);
            let v16 = x1_i - dx_c1;
            let v17 = x1_i - dx_c2;
            let v18 = y1_i - dy_c1;
            let v19 = y1_i - dy_c2;
            let v20 = v16.hypot(v18);
            let v21 = v17.hypot(v19);
            let v22 = v21 - v20;
            let dx_c = if v22 > 0.0 { dx_c1 } else { dx_c2 };
            let dy_c = if v22 > 0.0 { dy_c1 } else { dy_c2 };
            let sdx_c = div(dx_c * rw2, r_i);
            let sdy_c = div(dy_c * rh2, r_i);
            let x_c = (w / 2.0) + sdx_c;
            let y_c = (h / 2.0) + sdy_c;
            let ist0 = at2(sdx_c, sdy_c);
            let ist1 = ist0 + 21600000.0;
            let ist_ang = if ist0 > 0.0 { ist0 } else { ist1 };
            let isw1 = st_ang - ist_ang;
            let isw2 = isw1 - 21600000.0;
            let isw_ang = if isw1 > 0.0 { isw2 } else { isw1 };
            let p1 = x_f - x_c;
            let p2 = y_f - y_c;
            let p3 = p1.hypot(p2);
            let p4 = p3 / 2.0;
            let p5 = p4 - thh;
            let x_gp = if p5 > 0.0 { x_f } else { x_g };
            let y_gp = if p5 > 0.0 { y_f } else { y_g };
            let x_bp = if p5 > 0.0 { x_c } else { x_b };
            let y_bp = if p5 > 0.0 { y_c } else { y_b };
            let en0 = at2(sdx_f, sdy_f);
            let en1 = en0 + 21600000.0;
            let en2 = if en0 > 0.0 { en0 } else { en1 };
            let sw0 = en2 - st_ang;
            let sw1 = sw0 + 21600000.0;
            let sw_ang = if sw0 > 0.0 { sw0 } else { sw1 };
            p.m(x_e, y_e);
            p.arc(rw1, rh1, st_ang / 60000.0, sw_ang / 60000.0);
            p.l(x_gp, y_gp);
            p.l(x_a, y_a);
            p.l(x_bp, y_bp);
            p.l(x_c, y_c);
            p.arc(rw2, rh2, ist_ang / 60000.0, isw_ang / 60000.0);
            p.close();
        }
        "cloud" => {
            p.m(3900.0 * w / 43200.0, 14370.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, -11429249.0 / 60000.0, 7426832.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7267.0, -8646143.0 / 60000.0, 5396714.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4365.0, 5945.0, -8748475.0 / 60000.0, 5983381.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -7859164.0 / 60000.0, 7034504.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7273.0, -4722533.0 / 60000.0, 6541615.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6775.0, 9220.0, -2776035.0 / 60000.0, 7816140.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5785.0, 7867.0, 37501.0 / 60000.0, 6842000.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 1347096.0 / 60000.0, 6910353.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 7720.0, 10543.0, 3974558.0 / 60000.0, 4542661.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4360.0, 5918.0, -16496525.0 / 60000.0, 8804134.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4345.0, 5945.0, -14809710.0 / 60000.0, 9151131.0 / 60000.0);
            p.close();
            p.m(4693.0 * w / 43200.0, 26177.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4345.0, 5945.0, 5204520.0 / 60000.0, 1585770.0 / 60000.0);
            p.back();
            p.m(6928.0 * w / 43200.0, 34899.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4360.0, 5918.0, 4416628.0 / 60000.0, 686848.0 / 60000.0);
            p.back();
            p.m(16478.0 * w / 43200.0, 39090.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 8257449.0 / 60000.0, 844866.0 / 60000.0);
            p.back();
            p.m(28827.0 * w / 43200.0, 34751.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 387196.0 / 60000.0, 959901.0 / 60000.0);
            p.back();
            p.m(34129.0 * w / 43200.0, 22954.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5785.0, 7867.0, -4217541.0 / 60000.0, 4255042.0 / 60000.0);
            p.back();
            p.m(41798.0 * w / 43200.0, 15354.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7273.0, 1819082.0 / 60000.0, 1665090.0 / 60000.0);
            p.back();
            p.m(38324.0 * w / 43200.0, 5426.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -824660.0 / 60000.0, 891534.0 / 60000.0);
            p.back();
            p.m(29078.0 * w / 43200.0, 3952.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -8950887.0 / 60000.0, 1091722.0 / 60000.0);
            p.back();
            p.m(22141.0 * w / 43200.0, 4720.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4365.0, 5945.0, -9809656.0 / 60000.0, 1061181.0 / 60000.0);
            p.back();
            p.m(14000.0 * w / 43200.0, 5192.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, -4002417.0 / 60000.0, 739161.0 / 60000.0);
            p.back();
            p.m(4127.0 * w / 43200.0, 15789.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, 9459261.0 / 60000.0, 711490.0 / 60000.0);
            p.back();
        }
        "cloudCallout" => {
            let adj1 = raw("adj1", -20833.0);
            let adj2 = raw("adj2", 62500.0);
            let dx_pos = w * adj1 / 100000.0;
            let dy_pos = h * adj2 / 100000.0;
            let x_pos = (w / 2.0) + dx_pos;
            let y_pos = (h / 2.0) + dy_pos;
            let ht = cat2(h / 2.0, dx_pos, dy_pos);
            let wt = sat2(w / 2.0, dx_pos, dy_pos);
            let g2 = cat2(w / 2.0, ht, wt);
            let g3 = sat2(h / 2.0, ht, wt);
            let g4 = (w / 2.0) + g2;
            let g5 = (h / 2.0) + g3;
            let g6 = g4 - x_pos;
            let g7 = g5 - y_pos;
            let g8 = g6.hypot(g7);
            let g9 = ss * 6600.0 / 21600.0;
            let g10 = g8 - g9;
            let g11 = g10 / 3.0;
            let g12 = ss * 1800.0 / 21600.0;
            let g13 = g11 + g12;
            let g14 = div(g13 * g6, g8);
            let g15 = div(g13 * g7, g8);
            let g16 = g14 + x_pos;
            let g17 = g15 + y_pos;
            let g18 = ss * 4800.0 / 21600.0;
            let g19 = g11 * 2.0;
            let g20 = g18 + g19;
            let g21 = div(g20 * g6, g8);
            let g22 = div(g20 * g7, g8);
            let g23 = g21 + x_pos;
            let g24 = g22 + y_pos;
            let g25 = ss * 1200.0 / 21600.0;
            let g26 = ss * 600.0 / 21600.0;
            let x23 = x_pos + g26;
            let x24 = g16 + g25;
            let x25 = g23 + g12;
            p.m(3900.0 * w / 43200.0, 14370.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, -11429249.0 / 60000.0, 7426832.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7267.0, -8646143.0 / 60000.0, 5396714.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4365.0, 5945.0, -8748475.0 / 60000.0, 5983381.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -7859164.0 / 60000.0, 7034504.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7273.0, -4722533.0 / 60000.0, 6541615.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6775.0, 9220.0, -2776035.0 / 60000.0, 7816140.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5785.0, 7867.0, 37501.0 / 60000.0, 6842000.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 1347096.0 / 60000.0, 6910353.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 7720.0, 10543.0, 3974558.0 / 60000.0, 4542661.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4360.0, 5918.0, -16496525.0 / 60000.0, 8804134.0 / 60000.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4345.0, 5945.0, -14809710.0 / 60000.0, 9151131.0 / 60000.0);
            p.close();
            p.m(x23, y_pos);
            p.arc(g26, g26, 0.0, 360.0);
            p.close();
            p.m(x24, g17);
            p.arc(g25, g25, 0.0, 360.0);
            p.close();
            p.m(x25, g24);
            p.arc(g12, g12, 0.0, 360.0);
            p.close();
            p.m(4693.0 * w / 43200.0, 26177.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4345.0, 5945.0, 5204520.0 / 60000.0, 1585770.0 / 60000.0);
            p.back();
            p.m(6928.0 * w / 43200.0, 34899.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4360.0, 5918.0, 4416628.0 / 60000.0, 686848.0 / 60000.0);
            p.back();
            p.m(16478.0 * w / 43200.0, 39090.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 8257449.0 / 60000.0, 844866.0 / 60000.0);
            p.back();
            p.m(28827.0 * w / 43200.0, 34751.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6752.0, 9215.0, 387196.0 / 60000.0, 959901.0 / 60000.0);
            p.back();
            p.m(34129.0 * w / 43200.0, 22954.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5785.0, 7867.0, -4217541.0 / 60000.0, 4255042.0 / 60000.0);
            p.back();
            p.m(41798.0 * w / 43200.0, 15354.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 5333.0, 7273.0, 1819082.0 / 60000.0, 1665090.0 / 60000.0);
            p.back();
            p.m(38324.0 * w / 43200.0, 5426.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -824660.0 / 60000.0, 891534.0 / 60000.0);
            p.back();
            p.m(29078.0 * w / 43200.0, 3952.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4857.0, 6595.0, -8950887.0 / 60000.0, 1091722.0 / 60000.0);
            p.back();
            p.m(22141.0 * w / 43200.0, 4720.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 4365.0, 5945.0, -9809656.0 / 60000.0, 1061181.0 / 60000.0);
            p.back();
            p.m(14000.0 * w / 43200.0, 5192.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, -4002417.0 / 60000.0, 739161.0 / 60000.0);
            p.back();
            p.m(4127.0 * w / 43200.0, 15789.0 * h / 43200.0);
            p.arc_in(w / 43200.0, h / 43200.0, 6753.0, 9190.0, 9459261.0 / 60000.0, 711490.0 / 60000.0);
            p.back();
        }
        "cornerTabs" => {
            let md = w.hypot(h);
            let dx = 1.0 * md / 20.0;
            let y1 = h - dx;
            let x1 = w - dx;
            p.m(0.0, 0.0);
            p.l(dx, 0.0);
            p.l(0.0, dx);
            p.close();
            p.m(0.0, y1);
            p.l(dx, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(w, 0.0);
            p.l(w, dx);
            p.close();
            p.m(w, y1);
            p.l(w, h);
            p.l(x1, h);
            p.close();
        }
        "curvedDownArrow" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 25000.0);
            let max_adj2 = div(50000.0 * w, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let a1 = pin3(0.0, adj1, 100000.0);
            let th = ss * a1 / 100000.0;
            let aw = ss * a2 / 100000.0;
            let q1 = (th + aw) / 4.0;
            let w_r = (w / 2.0) - q1;
            let q7 = w_r * 2.0;
            let q8 = q7 * q7;
            let q9 = th * th;
            let q10 = q8 - q9;
            let q11 = q10.max(0.0).sqrt();
            let idy = div(q11 * h, q7);
            let ah = ss * adj3 / 100000.0;
            let x3 = w_r + th;
            let q2 = h * h;
            let q3 = ah * ah;
            let q4 = q2 - q3;
            let q5 = q4.max(0.0).sqrt();
            let dx = div(q5 * w_r, h);
            let x5 = w_r + dx;
            let x7 = x3 + dx;
            let q6 = aw - th;
            let dh = q6 / 2.0;
            let x4 = x5 - dh;
            let x8 = x7 + dh;
            let aw2 = aw / 2.0;
            let x6 = w - aw2;
            let y1 = h - ah;
            let sw_ang = at2(ah, dx);
            let msw_ang = -sw_ang;
            let iy = h - idy;
            let ix = (w_r + x3) / 2.0;
            let q12 = th / 2.0;
            let dang2 = at2(idy, q12);
            let st_ang = 16200000.0 + sw_ang;
            let st_ang2 = 16200000.0 - dang2;
            let sw_ang2 = dang2 - 5400000.0;
            let sw_ang3 = 5400000.0 + dang2;
            p.m(x6, h);
            p.l(x4, y1);
            p.l(x5, y1);
            p.arc(w_r, h, st_ang / 60000.0, msw_ang / 60000.0);
            p.l(x3, 0.0);
            p.arc(w_r, h, 270.0, sw_ang / 60000.0);
            p.l(x8, y1);
            p.close();
            p.m(ix, iy);
            p.arc(w_r, h, st_ang2 / 60000.0, sw_ang2 / 60000.0);
            p.l(0.0, h);
            p.arc(w_r, h, 180.0, sw_ang3 / 60000.0);
            p.close();
            p.m(ix, iy);
            p.arc(w_r, h, st_ang2 / 60000.0, sw_ang2 / 60000.0);
            p.l(0.0, h);
            p.arc(w_r, h, 180.0, 90.0);
            p.l(x3, 0.0);
            p.arc(w_r, h, 270.0, sw_ang / 60000.0);
            p.l(x8, y1);
            p.l(x6, h);
            p.l(x4, y1);
            p.l(x5, y1);
            p.arc(w_r, h, st_ang / 60000.0, msw_ang / 60000.0);
            p.back();
        }
        "curvedLeftArrow" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 25000.0);
            let max_adj2 = div(50000.0 * h, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let a1 = pin3(0.0, adj1, a2);
            let th = ss * a1 / 100000.0;
            let aw = ss * a2 / 100000.0;
            let q1 = (th + aw) / 4.0;
            let h_r = (h / 2.0) - q1;
            let q7 = h_r * 2.0;
            let q8 = q7 * q7;
            let q9 = th * th;
            let q10 = q8 - q9;
            let q11 = q10.max(0.0).sqrt();
            let idx = div(q11 * w, q7);
            let max_adj3 = div(100000.0 * idx, ss);
            let a3 = pin3(0.0, adj3, max_adj3);
            let ah = ss * a3 / 100000.0;
            let y3 = h_r + th;
            let q2 = w * w;
            let q3 = ah * ah;
            let q4 = q2 - q3;
            let q5 = q4.max(0.0).sqrt();
            let dy = div(q5 * h_r, w);
            let y5 = h_r + dy;
            let y7 = y3 + dy;
            let q6 = aw - th;
            let dh = q6 / 2.0;
            let y4 = y5 - dh;
            let y8 = y7 + dh;
            let aw2 = aw / 2.0;
            let y6 = h - aw2;
            let x1 = 0.0 + ah;
            let sw_ang = at2(ah, dy);
            let q12 = th / 2.0;
            let dang2 = at2(idx, q12);
            let sw_ang2 = dang2 - sw_ang;
            let sw_ang3 = sw_ang + dang2;
            let st_ang3 = -dang2;
            p.m(0.0, y6);
            p.l(x1, y4);
            p.l(x1, y5);
            p.arc(w, h_r, sw_ang / 60000.0, sw_ang2 / 60000.0);
            p.arc(w, h_r, st_ang3 / 60000.0, sw_ang3 / 60000.0);
            p.l(x1, y8);
            p.close();
            p.m(w, y3);
            p.arc(w, h_r, 0.0, -90.0);
            p.l(0.0, 0.0);
            p.arc(w, h_r, 270.0, 90.0);
            p.close();
            p.m(w, y3);
            p.arc(w, h_r, 0.0, -90.0);
            p.l(0.0, 0.0);
            p.arc(w, h_r, 270.0, 90.0);
            p.l(w, y3);
            p.arc(w, h_r, 0.0, sw_ang / 60000.0);
            p.l(x1, y8);
            p.l(0.0, y6);
            p.l(x1, y4);
            p.l(x1, y5);
            p.arc(w, h_r, sw_ang / 60000.0, sw_ang2 / 60000.0);
            p.back();
        }
        "curvedRightArrow" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 25000.0);
            let max_adj2 = div(50000.0 * h, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let a1 = pin3(0.0, adj1, a2);
            let th = ss * a1 / 100000.0;
            let aw = ss * a2 / 100000.0;
            let q1 = (th + aw) / 4.0;
            let h_r = (h / 2.0) - q1;
            let q7 = h_r * 2.0;
            let q8 = q7 * q7;
            let q9 = th * th;
            let q10 = q8 - q9;
            let q11 = q10.max(0.0).sqrt();
            let idx = div(q11 * w, q7);
            let max_adj3 = div(100000.0 * idx, ss);
            let a3 = pin3(0.0, adj3, max_adj3);
            let ah = ss * a3 / 100000.0;
            let y3 = h_r + th;
            let q2 = w * w;
            let q3 = ah * ah;
            let q4 = q2 - q3;
            let q5 = q4.max(0.0).sqrt();
            let dy = div(q5 * h_r, w);
            let y5 = h_r + dy;
            let y7 = y3 + dy;
            let q6 = aw - th;
            let dh = q6 / 2.0;
            let y4 = y5 - dh;
            let y8 = y7 + dh;
            let aw2 = aw / 2.0;
            let y6 = h - aw2;
            let x1 = w - ah;
            let sw_ang = at2(ah, dy);
            let st_ang = 10800000.0 - sw_ang;
            let msw_ang = -sw_ang;
            let q12 = th / 2.0;
            let dang2 = at2(idx, q12);
            let sw_ang2 = dang2 - 5400000.0;
            let sw_ang3 = 5400000.0 + dang2;
            let st_ang3 = 10800000.0 - dang2;
            p.m(0.0, h_r);
            p.arc(w, h_r, 180.0, msw_ang / 60000.0);
            p.l(x1, y4);
            p.l(w, y6);
            p.l(x1, y8);
            p.l(x1, y7);
            p.arc(w, h_r, st_ang / 60000.0, sw_ang / 60000.0);
            p.close();
            p.m(w, th);
            p.arc(w, h_r, 270.0, sw_ang2 / 60000.0);
            p.arc(w, h_r, st_ang3 / 60000.0, sw_ang3 / 60000.0);
            p.close();
            p.m(0.0, h_r);
            p.arc(w, h_r, 180.0, msw_ang / 60000.0);
            p.l(x1, y4);
            p.l(w, y6);
            p.l(x1, y8);
            p.l(x1, y7);
            p.arc(w, h_r, st_ang / 60000.0, sw_ang / 60000.0);
            p.l(0.0, h_r);
            p.arc(w, h_r, 180.0, 90.0);
            p.l(w, th);
            p.arc(w, h_r, 270.0, sw_ang2 / 60000.0);
            p.back();
        }
        "curvedUpArrow" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 25000.0);
            let max_adj2 = div(50000.0 * w, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let a1 = pin3(0.0, adj1, 100000.0);
            let th = ss * a1 / 100000.0;
            let aw = ss * a2 / 100000.0;
            let q1 = (th + aw) / 4.0;
            let w_r = (w / 2.0) - q1;
            let q7 = w_r * 2.0;
            let q8 = q7 * q7;
            let q9 = th * th;
            let q10 = q8 - q9;
            let q11 = q10.max(0.0).sqrt();
            let idy = div(q11 * h, q7);
            let ah = ss * adj3 / 100000.0;
            let x3 = w_r + th;
            let q2 = h * h;
            let q3 = ah * ah;
            let q4 = q2 - q3;
            let q5 = q4.max(0.0).sqrt();
            let dx = div(q5 * w_r, h);
            let x5 = w_r + dx;
            let x7 = x3 + dx;
            let q6 = aw - th;
            let dh = q6 / 2.0;
            let x4 = x5 - dh;
            let x8 = x7 + dh;
            let aw2 = aw / 2.0;
            let x6 = w - aw2;
            let y1 = 0.0 + ah;
            let sw_ang = at2(ah, dx);
            let iy = 0.0 + idy;
            let ix = (w_r + x3) / 2.0;
            let q12 = th / 2.0;
            let dang2 = at2(idy, q12);
            let sw_ang2 = dang2 - sw_ang;
            let st_ang3 = 5400000.0 - sw_ang;
            let sw_ang3 = sw_ang + dang2;
            let st_ang2 = 5400000.0 - dang2;
            p.m(x6, 0.0);
            p.l(x8, y1);
            p.l(x7, y1);
            p.arc(w_r, h, st_ang3 / 60000.0, sw_ang3 / 60000.0);
            p.arc(w_r, h, st_ang2 / 60000.0, sw_ang2 / 60000.0);
            p.l(x4, y1);
            p.close();
            p.m(w_r, h);
            p.arc(w_r, h, 90.0, 90.0);
            p.l(th, 0.0);
            p.arc(w_r, h, 180.0, -90.0);
            p.close();
            p.m(ix, iy);
            p.arc(w_r, h, st_ang2 / 60000.0, sw_ang2 / 60000.0);
            p.l(x4, y1);
            p.l(x6, 0.0);
            p.l(x8, y1);
            p.l(x7, y1);
            p.arc(w_r, h, st_ang3 / 60000.0, sw_ang / 60000.0);
            p.l(w_r, h);
            p.arc(w_r, h, 90.0, 90.0);
            p.l(th, 0.0);
            p.arc(w_r, h, 180.0, -90.0);
            p.back();
        }
        "doubleWave" => {
            let adj1 = raw("adj1", 6250.0);
            let adj2 = raw("adj2", 0.0);
            let a1 = pin3(0.0, adj1, 12500.0);
            let a2 = pin3(-10000.0, adj2, 10000.0);
            let y1 = h * a1 / 100000.0;
            let dy2 = y1 * 10.0 / 3.0;
            let y2 = y1 - dy2;
            let y3 = y1 + dy2;
            let y4 = h - y1;
            let y5 = y4 - dy2;
            let y6 = y4 + dy2;
            let of2 = w * a2 / 50000.0;
            let dx2 = if of2 > 0.0 { 0.0 } else { of2 };
            let x2 = 0.0 - dx2;
            let dx8 = if of2 > 0.0 { of2 } else { 0.0 };
            let x8 = w - dx8;
            let dx3 = (dx2 + x8) / 6.0;
            let x3 = x2 + dx3;
            let dx4 = (dx2 + x8) / 3.0;
            let x4 = x2 + dx4;
            let x5 = (x2 + x8) / 2.0;
            let x6 = x5 + dx3;
            let x7 = (x6 + x8) / 2.0;
            let x9 = 0.0 + dx8;
            let x15 = w + dx2;
            let x10 = x9 + dx3;
            let x11 = x9 + dx4;
            let x12 = (x9 + x15) / 2.0;
            let x13 = x12 + dx3;
            let x14 = (x13 + x15) / 2.0;
            p.m(x2, y1);
            p.cubic((x3, y2), (x4, y3), (x5, y1));
            p.cubic((x6, y2), (x7, y3), (x8, y1));
            p.l(x15, y4);
            p.cubic((x14, y6), (x13, y5), (x12, y4));
            p.cubic((x11, y6), (x10, y5), (x9, y4));
            p.close();
        }
        "ellipseRibbon" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 12500.0);
            let a1 = pin3(0.0, adj1, 100000.0);
            let a2 = pin3(25000.0, adj2, 75000.0);
            let q10 = 100000.0 - a1;
            let q11 = q10 / 2.0;
            let q12 = a1 - q11;
            let min_adj3 = 0.0_f32.max(q12);
            let a3 = pin3(min_adj3, adj3, a1);
            let dx2 = w * a2 / 200000.0;
            let x2 = (w / 2.0) - dx2;
            let x3 = x2 + (w / 8.0);
            let x4 = w - x3;
            let x5 = w - x2;
            let x6 = w - (w / 8.0);
            let dy1 = h * a3 / 100000.0;
            let f1 = div(4.0 * dy1, w);
            let q1 = div(x3 * x3, w);
            let q2 = x3 - q1;
            let y1 = f1 * q2;
            let cx1 = x3 / 2.0;
            let cy1 = f1 * cx1;
            let cx2 = w - cx1;
            let q1 = h * a1 / 100000.0;
            let dy3 = q1 - dy1;
            let q3 = div(x2 * x2, w);
            let q4 = x2 - q3;
            let q5 = f1 * q4;
            let y3 = q5 + dy3;
            let q6 = dy1 + dy3 - y3;
            let q7 = q6 + dy1;
            let cy3 = q7 + dy3;
            let rh = h - q1;
            let q8 = dy1 * 14.0 / 16.0;
            let y2 = (q8 + rh) / 2.0;
            let y5 = q5 + rh;
            let y6 = y3 + rh;
            let cx4 = x2 / 2.0;
            let q9 = f1 * cx4;
            let cy4 = q9 + rh;
            let cx5 = w - cx4;
            let cy6 = cy3 + rh;
            let y7 = y1 + dy3;
            let cy7 = q1 + q1 - y7;
            p.m(0.0, 0.0);
            p.quad((cx1, cy1), (x3, y1));
            p.l(x2, y3);
            p.quad((w / 2.0, cy3), (x5, y3));
            p.l(x4, y1);
            p.quad((cx2, cy1), (w, 0.0));
            p.l(x6, y2);
            p.l(w, rh);
            p.quad((cx5, cy4), (x5, y5));
            p.l(x5, y6);
            p.quad((w / 2.0, cy6), (x2, y6));
            p.l(x2, y5);
            p.quad((cx4, cy4), (0.0, rh));
            p.l(w / 8.0, y2);
            p.close();
            p.m(x3, y7);
            p.l(x3, y1);
            p.l(x2, y3);
            p.quad((w / 2.0, cy3), (x5, y3));
            p.l(x4, y1);
            p.l(x4, y7);
            p.quad((w / 2.0, cy7), (x3, y7));
            p.close();
            p.flip();
            p.m(x2, y5);
            p.l(x2, y3);
            p.back();
            p.m(x5, y3);
            p.l(x5, y5);
            p.back();
            p.m(x3, y1);
            p.l(x3, y7);
            p.back();
            p.m(x4, y7);
            p.l(x4, y1);
            p.back();
        }
        "ellipseRibbon2" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 12500.0);
            let a1 = pin3(0.0, adj1, 100000.0);
            let a2 = pin3(25000.0, adj2, 75000.0);
            let q10 = 100000.0 - a1;
            let q11 = q10 / 2.0;
            let q12 = a1 - q11;
            let min_adj3 = 0.0_f32.max(q12);
            let a3 = pin3(min_adj3, adj3, a1);
            let dx2 = w * a2 / 200000.0;
            let x2 = (w / 2.0) - dx2;
            let x3 = x2 + (w / 8.0);
            let x4 = w - x3;
            let x5 = w - x2;
            let x6 = w - (w / 8.0);
            let dy1 = h * a3 / 100000.0;
            let f1 = div(4.0 * dy1, w);
            let q1 = div(x3 * x3, w);
            let q2 = x3 - q1;
            let u1 = f1 * q2;
            let y1 = h - u1;
            let cx1 = x3 / 2.0;
            let cu1 = f1 * cx1;
            let cy1 = h - cu1;
            let cx2 = w - cx1;
            let q1 = h * a1 / 100000.0;
            let dy3 = q1 - dy1;
            let q3 = div(x2 * x2, w);
            let q4 = x2 - q3;
            let q5 = f1 * q4;
            let u3 = q5 + dy3;
            let y3 = h - u3;
            let q6 = dy1 + dy3 - u3;
            let q7 = q6 + dy1;
            let cu3 = q7 + dy3;
            let cy3 = h - cu3;
            let rh = h - q1;
            let q8 = dy1 * 14.0 / 16.0;
            let u2 = (q8 + rh) / 2.0;
            let y2 = h - u2;
            let u5 = q5 + rh;
            let y5 = h - u5;
            let u6 = u3 + rh;
            let y6 = h - u6;
            let cx4 = x2 / 2.0;
            let q9 = f1 * cx4;
            let cu4 = q9 + rh;
            let cy4 = h - cu4;
            let cx5 = w - cx4;
            let cu6 = cu3 + rh;
            let cy6 = h - cu6;
            let u7 = u1 + dy3;
            let y7 = h - u7;
            let cu7 = q1 + q1 - u7;
            let cy7 = h - cu7;
            p.m(0.0, h);
            p.quad((cx1, cy1), (x3, y1));
            p.l(x2, y3);
            p.quad((w / 2.0, cy3), (x5, y3));
            p.l(x4, y1);
            p.quad((cx2, cy1), (w, h));
            p.l(x6, y2);
            p.l(w, q1);
            p.quad((cx5, cy4), (x5, y5));
            p.l(x5, y6);
            p.quad((w / 2.0, cy6), (x2, y6));
            p.l(x2, y5);
            p.quad((cx4, cy4), (0.0, q1));
            p.l(w / 8.0, y2);
            p.close();
            p.m(x3, y7);
            p.l(x3, y1);
            p.l(x2, y3);
            p.quad((w / 2.0, cy3), (x5, y3));
            p.l(x4, y1);
            p.l(x4, y7);
            p.quad((w / 2.0, cy7), (x3, y7));
            p.close();
            p.flip();
            p.m(x2, y3);
            p.l(x2, y5);
            p.back();
            p.m(x5, y5);
            p.l(x5, y3);
            p.back();
            p.m(x3, y7);
            p.l(x3, y1);
            p.back();
            p.m(x4, y1);
            p.l(x4, y7);
            p.back();
        }
        "flowChartCollate" => {
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w / 2.0, h / 2.0);
            p.l(w, h);
            p.l(0.0, h);
            p.l(w / 2.0, h / 2.0);
            p.close();
        }
        "flowChartDisplay" => {
            p.m(0.0, 3.0 * h / 6.0);
            p.l(w / 6.0, 0.0);
            p.l(5.0 * w / 6.0, 0.0);
            p.arc_in(w / 6.0, h / 6.0, 1.0, 3.0, 270.0, 180.0);
            p.l(w / 6.0, h);
            p.close();
        }
        "flowChartInternalStorage" => {
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(w / 8.0, 0.0);
            p.l(w / 8.0, h);
            p.back();
            p.m(0.0, h / 8.0);
            p.l(w, h / 8.0);
            p.back();
        }
        "flowChartMultidocument" => {
            p.m(0.0, 20782.0 * h / 21600.0);
            p.cubic(
                (9298.0 * w / 21600.0, 23542.0 * h / 21600.0),
                (9298.0 * w / 21600.0, 18022.0 * h / 21600.0),
                (18595.0 * w / 21600.0, 18022.0 * h / 21600.0),
            );
            p.l(18595.0 * w / 21600.0, 3675.0 * h / 21600.0);
            p.l(0.0, 3675.0 * h / 21600.0);
            p.close();
            p.m(1532.0 * w / 21600.0, 3675.0 * h / 21600.0);
            p.l(1532.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(20000.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(20000.0 * w / 21600.0, 16252.0 * h / 21600.0);
            p.cubic(
                (19298.0 * w / 21600.0, 16252.0 * h / 21600.0),
                (18595.0 * w / 21600.0, 16352.0 * h / 21600.0),
                (18595.0 * w / 21600.0, 16352.0 * h / 21600.0),
            );
            p.l(18595.0 * w / 21600.0, 3675.0 * h / 21600.0);
            p.close();
            p.m(2972.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(2972.0 * w / 21600.0, 0.0);
            p.l(w, 0.0);
            p.l(w, 14392.0 * h / 21600.0);
            p.cubic(
                (20800.0 * w / 21600.0, 14392.0 * h / 21600.0),
                (20000.0 * w / 21600.0, 14467.0 * h / 21600.0),
                (20000.0 * w / 21600.0, 14467.0 * h / 21600.0),
            );
            p.l(20000.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.close();
            p.m(1532.0 * w / 21600.0, 3675.0 * h / 21600.0);
            p.l(1532.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(20000.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(20000.0 * w / 21600.0, 16252.0 * h / 21600.0);
            p.cubic(
                (19298.0 * w / 21600.0, 16252.0 * h / 21600.0),
                (18595.0 * w / 21600.0, 16352.0 * h / 21600.0),
                (18595.0 * w / 21600.0, 16352.0 * h / 21600.0),
            );
            p.back();
            p.m(2972.0 * w / 21600.0, 1815.0 * h / 21600.0);
            p.l(2972.0 * w / 21600.0, 0.0);
            p.l(w, 0.0);
            p.l(w, 14392.0 * h / 21600.0);
            p.cubic(
                (20800.0 * w / 21600.0, 14392.0 * h / 21600.0),
                (20000.0 * w / 21600.0, 14467.0 * h / 21600.0),
                (20000.0 * w / 21600.0, 14467.0 * h / 21600.0),
            );
            p.back();
        }
        "flowChartOfflineStorage" => {
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w / 2.0, h);
            p.close();
            p.m(2.0 * w / 5.0, 4.0 * h / 5.0);
            p.l(3.0 * w / 5.0, 4.0 * h / 5.0);
            p.back();
        }
        "flowChartOnlineStorage" => {
            p.m(w / 6.0, 0.0);
            p.l(w, 0.0);
            p.arc_in(w / 6.0, h / 6.0, 1.0, 3.0, 270.0, -180.0);
            p.l(w / 6.0, h);
            p.arc_in(w / 6.0, h / 6.0, 1.0, 3.0, 90.0, 180.0);
            p.close();
        }
        "flowChartPredefinedProcess" => {
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(w / 8.0, 0.0);
            p.l(w / 8.0, h);
            p.back();
            p.m(7.0 * w / 8.0, 0.0);
            p.l(7.0 * w / 8.0, h);
            p.back();
        }
        "flowChartSort" => {
            p.m(0.0, h / 2.0);
            p.l(w / 2.0, 0.0);
            p.l(w, h / 2.0);
            p.l(w / 2.0, h);
            p.close();
            p.m(0.0, h / 2.0);
            p.l(w, h / 2.0);
            p.back();
        }
        "gear9" => {
            let adj1 = raw("adj1", 10000.0);
            let adj2 = raw("adj2", 1763.0);
            let a1 = pin3(0.0, adj1, 20000.0);
            let a2 = pin3(0.0, adj2, 2679.0);
            let th = ss * a1 / 100000.0;
            let l_fd = ss * a2 / 100000.0;
            let th2 = th / 2.0;
            let l2 = l_fd / 2.0;
            let l3 = th2 + l2;
            let rh = (h / 2.0) - th;
            let rw = (w / 2.0) - th;
            let dr = rw - rh;
            let maxr = if dr > 0.0 { rh } else { rw };
            let ha = at2(maxr, l3);
            let a_a1 = 18600000.0 - ha;
            let a_d1 = 18600000.0 + ha;
            let ta11 = cos60(rw, a_a1);
            let ta12 = sin60(rh, a_a1);
            let b_a1 = at2(ta11, ta12);
            let cta1 = cos60(rh, b_a1);
            let sta1 = sin60(rw, b_a1);
            let ma1 = cta1.hypot(sta1);
            let na1 = div(rw * rh, ma1);
            let dxa1 = cos60(na1, b_a1);
            let dya1 = sin60(na1, b_a1);
            let x_a1 = (w / 2.0) + dxa1;
            let y_a1 = (h / 2.0) + dya1;
            let td11 = cos60(rw, a_d1);
            let td12 = sin60(rh, a_d1);
            let b_d1 = at2(td11, td12);
            let ctd1 = cos60(rh, b_d1);
            let std1 = sin60(rw, b_d1);
            let md1 = ctd1.hypot(std1);
            let nd1 = div(rw * rh, md1);
            let dxd1 = cos60(nd1, b_d1);
            let dyd1 = sin60(nd1, b_d1);
            let x_d1 = (w / 2.0) + dxd1;
            let y_d1 = (h / 2.0) + dyd1;
            let x_ad1 = x_a1 - x_d1;
            let y_ad1 = y_a1 - y_d1;
            let a1 = at2(y_ad1, x_ad1);
            let dx_f1 = sin60(l_fd, a1);
            let dy_f1 = cos60(l_fd, a1);
            let x_f1 = x_d1 + dx_f1;
            let y_f1 = y_d1 + dy_f1;
            let x_e1 = x_a1 - dx_f1;
            let y_e1 = y_a1 - dy_f1;
            let y_c1t = sin60(th, a1);
            let x_c1t = cos60(th, a1);
            let y_c1 = y_f1 + y_c1t;
            let x_c1 = x_f1 - x_c1t;
            let y_b1 = y_e1 + y_c1t;
            let x_b1 = x_e1 - x_c1t;
            let a_a2 = 21000000.0 - ha;
            let a_d2 = 21000000.0 + ha;
            let ta21 = cos60(rw, a_a2);
            let ta22 = sin60(rh, a_a2);
            let b_a2 = at2(ta21, ta22);
            let cta2 = cos60(rh, b_a2);
            let sta2 = sin60(rw, b_a2);
            let ma2 = cta2.hypot(sta2);
            let na2 = div(rw * rh, ma2);
            let dxa2 = cos60(na2, b_a2);
            let dya2 = sin60(na2, b_a2);
            let x_a2 = (w / 2.0) + dxa2;
            let y_a2 = (h / 2.0) + dya2;
            let td21 = cos60(rw, a_d2);
            let td22 = sin60(rh, a_d2);
            let b_d2 = at2(td21, td22);
            let ctd2 = cos60(rh, b_d2);
            let std2 = sin60(rw, b_d2);
            let md2 = ctd2.hypot(std2);
            let nd2 = div(rw * rh, md2);
            let dxd2 = cos60(nd2, b_d2);
            let dyd2 = sin60(nd2, b_d2);
            let x_d2 = (w / 2.0) + dxd2;
            let y_d2 = (h / 2.0) + dyd2;
            let x_ad2 = x_a2 - x_d2;
            let y_ad2 = y_a2 - y_d2;
            let a2 = at2(y_ad2, x_ad2);
            let dx_f2 = sin60(l_fd, a2);
            let dy_f2 = cos60(l_fd, a2);
            let x_f2 = x_d2 + dx_f2;
            let y_f2 = y_d2 + dy_f2;
            let x_e2 = x_a2 - dx_f2;
            let y_e2 = y_a2 - dy_f2;
            let y_c2t = sin60(th, a2);
            let x_c2t = cos60(th, a2);
            let y_c2 = y_f2 + y_c2t;
            let x_c2 = x_f2 - x_c2t;
            let y_b2 = y_e2 + y_c2t;
            let x_b2 = x_e2 - x_c2t;
            let sw_ang1 = b_a2 - b_d1;
            let a_a3 = 1800000.0 - ha;
            let a_d3 = 1800000.0 + ha;
            let ta31 = cos60(rw, a_a3);
            let ta32 = sin60(rh, a_a3);
            let b_a3 = at2(ta31, ta32);
            let cta3 = cos60(rh, b_a3);
            let sta3 = sin60(rw, b_a3);
            let ma3 = cta3.hypot(sta3);
            let na3 = div(rw * rh, ma3);
            let dxa3 = cos60(na3, b_a3);
            let dya3 = sin60(na3, b_a3);
            let x_a3 = (w / 2.0) + dxa3;
            let y_a3 = (h / 2.0) + dya3;
            let td31 = cos60(rw, a_d3);
            let td32 = sin60(rh, a_d3);
            let b_d3 = at2(td31, td32);
            let ctd3 = cos60(rh, b_d3);
            let std3 = sin60(rw, b_d3);
            let md3 = ctd3.hypot(std3);
            let nd3 = div(rw * rh, md3);
            let dxd3 = cos60(nd3, b_d3);
            let dyd3 = sin60(nd3, b_d3);
            let x_d3 = (w / 2.0) + dxd3;
            let y_d3 = (h / 2.0) + dyd3;
            let x_ad3 = x_a3 - x_d3;
            let y_ad3 = y_a3 - y_d3;
            let a3 = at2(y_ad3, x_ad3);
            let dx_f3 = sin60(l_fd, a3);
            let dy_f3 = cos60(l_fd, a3);
            let x_f3 = x_d3 + dx_f3;
            let y_f3 = y_d3 + dy_f3;
            let x_e3 = x_a3 - dx_f3;
            let y_e3 = y_a3 - dy_f3;
            let y_c3t = sin60(th, a3);
            let x_c3t = cos60(th, a3);
            let y_c3 = y_f3 + y_c3t;
            let x_c3 = x_f3 - x_c3t;
            let y_b3 = y_e3 + y_c3t;
            let x_b3 = x_e3 - x_c3t;
            let sw_ang2 = b_a3 - b_d2;
            let a_a4 = 4200000.0 - ha;
            let a_d4 = 4200000.0 + ha;
            let ta41 = cos60(rw, a_a4);
            let ta42 = sin60(rh, a_a4);
            let b_a4 = at2(ta41, ta42);
            let cta4 = cos60(rh, b_a4);
            let sta4 = sin60(rw, b_a4);
            let ma4 = cta4.hypot(sta4);
            let na4 = div(rw * rh, ma4);
            let dxa4 = cos60(na4, b_a4);
            let dya4 = sin60(na4, b_a4);
            let x_a4 = (w / 2.0) + dxa4;
            let y_a4 = (h / 2.0) + dya4;
            let td41 = cos60(rw, a_d4);
            let td42 = sin60(rh, a_d4);
            let b_d4 = at2(td41, td42);
            let ctd4 = cos60(rh, b_d4);
            let std4 = sin60(rw, b_d4);
            let md4 = ctd4.hypot(std4);
            let nd4 = div(rw * rh, md4);
            let dxd4 = cos60(nd4, b_d4);
            let dyd4 = sin60(nd4, b_d4);
            let x_d4 = (w / 2.0) + dxd4;
            let y_d4 = (h / 2.0) + dyd4;
            let x_ad4 = x_a4 - x_d4;
            let y_ad4 = y_a4 - y_d4;
            let a4 = at2(y_ad4, x_ad4);
            let dx_f4 = sin60(l_fd, a4);
            let dy_f4 = cos60(l_fd, a4);
            let x_f4 = x_d4 + dx_f4;
            let y_f4 = y_d4 + dy_f4;
            let x_e4 = x_a4 - dx_f4;
            let y_e4 = y_a4 - dy_f4;
            let y_c4t = sin60(th, a4);
            let x_c4t = cos60(th, a4);
            let y_c4 = y_f4 + y_c4t;
            let x_c4 = x_f4 - x_c4t;
            let y_b4 = y_e4 + y_c4t;
            let x_b4 = x_e4 - x_c4t;
            let sw_ang3 = b_a4 - b_d3;
            let a_a5 = 6600000.0 - ha;
            let a_d5 = 6600000.0 + ha;
            let ta51 = cos60(rw, a_a5);
            let ta52 = sin60(rh, a_a5);
            let b_a5 = at2(ta51, ta52);
            let td51 = cos60(rw, a_d5);
            let td52 = sin60(rh, a_d5);
            let b_d5 = at2(td51, td52);
            let x_d5 = w - x_a4;
            let x_c5 = w - x_b4;
            let x_b5 = w - x_c4;
            let sw_ang4 = b_a5 - b_d4;
            let a_d6 = 9000000.0 + ha;
            let td61 = cos60(rw, a_d6);
            let td62 = sin60(rh, a_d6);
            let b_d6 = at2(td61, td62);
            let x_d6 = w - x_a3;
            let x_c6 = w - x_b3;
            let x_b6 = w - x_c3;
            let a_d7 = 11400000.0 + ha;
            let td71 = cos60(rw, a_d7);
            let td72 = sin60(rh, a_d7);
            let b_d7 = at2(td71, td72);
            let x_d7 = w - x_a2;
            let x_c7 = w - x_b2;
            let x_b7 = w - x_c2;
            let a_d8 = 13800000.0 + ha;
            let td81 = cos60(rw, a_d8);
            let td82 = sin60(rh, a_d8);
            let b_d8 = at2(td81, td82);
            let x_d8 = w - x_a1;
            let x_c8 = w - x_b1;
            let x_b8 = w - x_c1;
            let a_a9 = 16200000.0 - ha;
            let a_d9 = 16200000.0 + ha;
            let td91 = cos60(rw, a_d9);
            let td92 = sin60(rh, a_d9);
            let b_d9 = at2(td91, td92);
            let ctd9 = cos60(rh, b_d9);
            let std9 = sin60(rw, b_d9);
            let md9 = ctd9.hypot(std9);
            let nd9 = div(rw * rh, md9);
            let dxd9 = cos60(nd9, b_d9);
            let dyd9 = sin60(nd9, b_d9);
            let x_d9 = (w / 2.0) + dxd9;
            let y_d9 = (h / 2.0) + dyd9;
            let ta91 = cos60(rw, a_a9);
            let ta92 = sin60(rh, a_a9);
            let b_a9 = at2(ta91, ta92);
            let x_a9 = (w / 2.0) - dxd9;
            let x_f9 = x_d9 - l_fd;
            let x_e9 = x_a9 + l_fd;
            let y_c9 = y_d9 - th;
            let sw_ang5 = b_a9 - b_d8;
            p.m(x_a1, y_a1);
            p.l(x_b1, y_b1);
            p.l(x_c1, y_c1);
            p.l(x_d1, y_d1);
            p.arc(rw, rh, b_d1 / 60000.0, sw_ang1 / 60000.0);
            p.l(x_b2, y_b2);
            p.l(x_c2, y_c2);
            p.l(x_d2, y_d2);
            p.arc(rw, rh, b_d2 / 60000.0, sw_ang2 / 60000.0);
            p.l(x_b3, y_b3);
            p.l(x_c3, y_c3);
            p.l(x_d3, y_d3);
            p.arc(rw, rh, b_d3 / 60000.0, sw_ang3 / 60000.0);
            p.l(x_b4, y_b4);
            p.l(x_c4, y_c4);
            p.l(x_d4, y_d4);
            p.arc(rw, rh, b_d4 / 60000.0, sw_ang4 / 60000.0);
            p.l(x_b5, y_c4);
            p.l(x_c5, y_b4);
            p.l(x_d5, y_a4);
            p.arc(rw, rh, b_d5 / 60000.0, sw_ang3 / 60000.0);
            p.l(x_b6, y_c3);
            p.l(x_c6, y_b3);
            p.l(x_d6, y_a3);
            p.arc(rw, rh, b_d6 / 60000.0, sw_ang2 / 60000.0);
            p.l(x_b7, y_c2);
            p.l(x_c7, y_b2);
            p.l(x_d7, y_a2);
            p.arc(rw, rh, b_d7 / 60000.0, sw_ang1 / 60000.0);
            p.l(x_b8, y_c1);
            p.l(x_c8, y_b1);
            p.l(x_d8, y_a1);
            p.arc(rw, rh, b_d8 / 60000.0, sw_ang5 / 60000.0);
            p.l(x_e9, y_c9);
            p.l(x_f9, y_c9);
            p.l(x_d9, y_d9);
            p.arc(rw, rh, b_d9 / 60000.0, sw_ang5 / 60000.0);
            p.close();
        }
        "horizontalScroll" => {
            let adj = raw("adj", 12500.0);
            let a = pin3(0.0, adj, 25000.0);
            let ch = ss * a / 100000.0;
            let ch2 = ch / 2.0;
            let ch4 = ch / 4.0;
            let y3 = ch + ch2;
            let y4 = ch + ch;
            let y6 = h - ch;
            let y7 = h - ch2;
            let y5 = y6 - ch2;
            let x3 = w - ch;
            let x4 = w - ch2;
            p.m(w, ch2);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.l(x4, ch2);
            p.arc(ch4, ch4, 0.0, 180.0);
            p.l(x3, ch);
            p.l(ch2, ch);
            p.arc(ch2, ch2, 270.0, -90.0);
            p.l(0.0, y7);
            p.arc(ch2, ch2, 180.0, -180.0);
            p.l(ch, y6);
            p.l(x4, y6);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.close();
            p.m(ch2, y4);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.arc(ch4, ch4, 0.0, -180.0);
            p.close();
            p.m(ch2, y4);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.arc(ch4, ch4, 0.0, -180.0);
            p.close();
            p.m(x4, ch);
            p.arc(ch2, ch2, 90.0, -270.0);
            p.arc(ch4, ch4, 180.0, -180.0);
            p.close();
            p.m(0.0, y3);
            p.arc(ch2, ch2, 180.0, 90.0);
            p.l(x3, ch);
            p.l(x3, ch2);
            p.arc(ch2, ch2, 180.0, 180.0);
            p.l(w, y5);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.l(ch, y6);
            p.l(ch, y7);
            p.arc(ch2, ch2, 0.0, 180.0);
            p.close();
            p.flip();
            p.m(x3, ch);
            p.l(x4, ch);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.back();
            p.m(x4, ch);
            p.l(x4, ch2);
            p.arc(ch4, ch4, 0.0, 180.0);
            p.back();
            p.m(ch2, y4);
            p.l(ch2, y3);
            p.arc(ch4, ch4, 180.0, 180.0);
            p.arc(ch2, ch2, 0.0, 180.0);
            p.back();
            p.m(ch, y3);
            p.l(ch, y6);
            p.back();
        }
        "irregularSeal1" => {
            p.m(10800.0 * w / 21600.0, 5800.0 * h / 21600.0);
            p.l(14522.0 * w / 21600.0, 0.0);
            p.l(14155.0 * w / 21600.0, 5325.0 * h / 21600.0);
            p.l(18380.0 * w / 21600.0, 4457.0 * h / 21600.0);
            p.l(16702.0 * w / 21600.0, 7315.0 * h / 21600.0);
            p.l(21097.0 * w / 21600.0, 8137.0 * h / 21600.0);
            p.l(17607.0 * w / 21600.0, 10475.0 * h / 21600.0);
            p.l(w, 13290.0 * h / 21600.0);
            p.l(16837.0 * w / 21600.0, 12942.0 * h / 21600.0);
            p.l(18145.0 * w / 21600.0, 18095.0 * h / 21600.0);
            p.l(14020.0 * w / 21600.0, 14457.0 * h / 21600.0);
            p.l(13247.0 * w / 21600.0, 19737.0 * h / 21600.0);
            p.l(10532.0 * w / 21600.0, 14935.0 * h / 21600.0);
            p.l(8485.0 * w / 21600.0, h);
            p.l(7715.0 * w / 21600.0, 15627.0 * h / 21600.0);
            p.l(4762.0 * w / 21600.0, 17617.0 * h / 21600.0);
            p.l(5667.0 * w / 21600.0, 13937.0 * h / 21600.0);
            p.l(135.0 * w / 21600.0, 14587.0 * h / 21600.0);
            p.l(3722.0 * w / 21600.0, 11775.0 * h / 21600.0);
            p.l(0.0, 8615.0 * h / 21600.0);
            p.l(4627.0 * w / 21600.0, 7617.0 * h / 21600.0);
            p.l(370.0 * w / 21600.0, 2295.0 * h / 21600.0);
            p.l(7312.0 * w / 21600.0, 6320.0 * h / 21600.0);
            p.l(8352.0 * w / 21600.0, 2295.0 * h / 21600.0);
            p.close();
        }
        "irregularSeal2" => {
            p.m(11462.0 * w / 21600.0, 4342.0 * h / 21600.0);
            p.l(14790.0 * w / 21600.0, 0.0);
            p.l(14525.0 * w / 21600.0, 5777.0 * h / 21600.0);
            p.l(18007.0 * w / 21600.0, 3172.0 * h / 21600.0);
            p.l(16380.0 * w / 21600.0, 6532.0 * h / 21600.0);
            p.l(w, 6645.0 * h / 21600.0);
            p.l(16985.0 * w / 21600.0, 9402.0 * h / 21600.0);
            p.l(18270.0 * w / 21600.0, 11290.0 * h / 21600.0);
            p.l(16380.0 * w / 21600.0, 12310.0 * h / 21600.0);
            p.l(18877.0 * w / 21600.0, 15632.0 * h / 21600.0);
            p.l(14640.0 * w / 21600.0, 14350.0 * h / 21600.0);
            p.l(14942.0 * w / 21600.0, 17370.0 * h / 21600.0);
            p.l(12180.0 * w / 21600.0, 15935.0 * h / 21600.0);
            p.l(11612.0 * w / 21600.0, 18842.0 * h / 21600.0);
            p.l(9872.0 * w / 21600.0, 17370.0 * h / 21600.0);
            p.l(8700.0 * w / 21600.0, 19712.0 * h / 21600.0);
            p.l(7527.0 * w / 21600.0, 18125.0 * h / 21600.0);
            p.l(4917.0 * w / 21600.0, h);
            p.l(4805.0 * w / 21600.0, 18240.0 * h / 21600.0);
            p.l(1285.0 * w / 21600.0, 17825.0 * h / 21600.0);
            p.l(3330.0 * w / 21600.0, 15370.0 * h / 21600.0);
            p.l(0.0, 12877.0 * h / 21600.0);
            p.l(3935.0 * w / 21600.0, 11592.0 * h / 21600.0);
            p.l(1172.0 * w / 21600.0, 8270.0 * h / 21600.0);
            p.l(5372.0 * w / 21600.0, 7817.0 * h / 21600.0);
            p.l(4502.0 * w / 21600.0, 3625.0 * h / 21600.0);
            p.l(8550.0 * w / 21600.0, 6382.0 * h / 21600.0);
            p.l(9722.0 * w / 21600.0, 1887.0 * h / 21600.0);
            p.close();
        }
        "leftCircularArrow" => {
            let adj1 = raw("adj1", 12500.0);
            let adj2 = raw("adj2", -1142319.0);
            let adj3 = raw("adj3", 1142319.0);
            let adj4 = raw("adj4", 10800000.0);
            let adj5 = raw("adj5", 12500.0);
            let a5 = pin3(0.0, adj5, 25000.0);
            let max_adj1 = a5 * 2.0;
            let a1 = pin3(0.0, adj1, max_adj1);
            let en_ang = pin3(1.0, adj3, 21600000.0);
            let st_ang = pin3(0.0, adj4, 21600000.0);
            let th = ss * a1 / 100000.0;
            let thh = ss * a5 / 100000.0;
            let th2 = th / 2.0;
            let rw1 = (w / 2.0) + th2 - thh;
            let rh1 = (h / 2.0) + th2 - thh;
            let rw2 = rw1 - th;
            let rh2 = rh1 - th;
            let rw3 = rw2 + th2;
            let rh3 = rh2 + th2;
            let wt_h = sin60(rw3, en_ang);
            let ht_h = cos60(rh3, en_ang);
            let dx_h = cat2(rw3, ht_h, wt_h);
            let dy_h = sat2(rh3, ht_h, wt_h);
            let x_h = (w / 2.0) + dx_h;
            let y_h = (h / 2.0) + dy_h;
            let r_i = rw2.min(rh2);
            let u1 = dx_h * dx_h;
            let u2 = dy_h * dy_h;
            let u3 = r_i * r_i;
            let u4 = u1 - u3;
            let u5 = u2 - u3;
            let u6 = div(u4 * u5, u1);
            let u7 = div(u6, u2);
            let u8 = 1.0 - u7;
            let u9 = u8.max(0.0).sqrt();
            let u10 = div(u4, dx_h);
            let u11 = div(u10, dy_h);
            let u12 = div(1.0 + u9, u11);
            let u13 = at2(1.0, u12);
            let u14 = u13 + 21600000.0;
            let u15 = if u13 > 0.0 { u13 } else { u14 };
            let u16 = u15 - en_ang;
            let u17 = u16 + 21600000.0;
            let u18 = if u16 > 0.0 { u16 } else { u17 };
            let u19 = u18 - 10800000.0;
            let u20 = u18 - 21600000.0;
            let u21 = if u19 > 0.0 { u20 } else { u18 };
            let u22 = u21.abs();
            let min_ang = -u22;
            let u23 = adj2.abs();
            let a2 = -u23;
            let a_ang = pin3(min_ang, a2, 0.0);
            let pt_ang = en_ang + a_ang;
            let wt_a = sin60(rw3, pt_ang);
            let ht_a = cos60(rh3, pt_ang);
            let dx_a = cat2(rw3, ht_a, wt_a);
            let dy_a = sat2(rh3, ht_a, wt_a);
            let x_a = (w / 2.0) + dx_a;
            let y_a = (h / 2.0) + dy_a;
            let wt_e = sin60(rw1, st_ang);
            let ht_e = cos60(rh1, st_ang);
            let dx_e = cat2(rw1, ht_e, wt_e);
            let dy_e = sat2(rh1, ht_e, wt_e);
            let x_e = (w / 2.0) + dx_e;
            let y_e = (h / 2.0) + dy_e;
            let wt_d = sin60(rw2, st_ang);
            let ht_d = cos60(rh2, st_ang);
            let dx_d = cat2(rw2, ht_d, wt_d);
            let dy_d = sat2(rh2, ht_d, wt_d);
            let x_d = (w / 2.0) + dx_d;
            let y_d = (h / 2.0) + dy_d;
            let dx_g = cos60(thh, pt_ang);
            let dy_g = sin60(thh, pt_ang);
            let x_g = x_h + dx_g;
            let y_g = y_h + dy_g;
            let dx_b = cos60(thh, pt_ang);
            let dy_b = sin60(thh, pt_ang);
            let x_b = x_h - dx_b;
            let y_b = y_h - dy_b;
            let sx1 = x_b - (w / 2.0);
            let sy1 = y_b - (h / 2.0);
            let sx2 = x_g - (w / 2.0);
            let sy2 = y_g - (h / 2.0);
            let r_o = rw1.min(rh1);
            let x1_o = div(sx1 * r_o, rw1);
            let y1_o = div(sy1 * r_o, rh1);
            let x2_o = div(sx2 * r_o, rw1);
            let y2_o = div(sy2 * r_o, rh1);
            let dx_o = x2_o - x1_o;
            let dy_o = y2_o - y1_o;
            let d_o = dx_o.hypot(dy_o);
            let q1 = x1_o * y2_o;
            let q2 = x2_o * y1_o;
            let v_do = q1 - q2;
            let q3 = r_o * r_o;
            let q4 = d_o * d_o;
            let q5 = q3 * q4;
            let q6 = v_do * v_do;
            let q7 = q5 - q6;
            let q8 = q7.max(0.0);
            let sdel_o = q8.max(0.0).sqrt();
            let ndy_o = -dy_o;
            let sdy_o = if ndy_o > 0.0 { -1.0 } else { 1.0 };
            let q9 = sdy_o * dx_o;
            let q10 = q9 * sdel_o;
            let q11 = v_do * dy_o;
            let dx_f1 = div(q11 + q10, q4);
            let q12 = q11 - q10;
            let dx_f2 = div(q12, q4);
            let ady_o = dy_o.abs();
            let q13 = ady_o * sdel_o;
            let q14 = v_do * dx_o / -1.0;
            let dy_f1 = div(q14 + q13, q4);
            let q15 = q14 - q13;
            let dy_f2 = div(q15, q4);
            let q16 = x2_o - dx_f1;
            let q17 = x2_o - dx_f2;
            let q18 = y2_o - dy_f1;
            let q19 = y2_o - dy_f2;
            let q20 = q16.hypot(q18);
            let q21 = q17.hypot(q19);
            let q22 = q21 - q20;
            let dx_f = if q22 > 0.0 { dx_f1 } else { dx_f2 };
            let dy_f = if q22 > 0.0 { dy_f1 } else { dy_f2 };
            let sdx_f = div(dx_f * rw1, r_o);
            let sdy_f = div(dy_f * rh1, r_o);
            let x_f = (w / 2.0) + sdx_f;
            let y_f = (h / 2.0) + sdy_f;
            let x1_i = div(sx1 * r_i, rw2);
            let y1_i = div(sy1 * r_i, rh2);
            let x2_i = div(sx2 * r_i, rw2);
            let y2_i = div(sy2 * r_i, rh2);
            let dx_i = x2_i - x1_i;
            let dy_i = y2_i - y1_i;
            let d_i = dx_i.hypot(dy_i);
            let v1 = x1_i * y2_i;
            let v2 = x2_i * y1_i;
            let di = v1 - v2;
            let v3 = r_i * r_i;
            let v4 = d_i * d_i;
            let v5 = v3 * v4;
            let v6 = di * di;
            let v7 = v5 - v6;
            let v8 = v7.max(0.0);
            let sdel_i = v8.max(0.0).sqrt();
            let v9 = sdy_o * dx_i;
            let v10 = v9 * sdel_i;
            let v11 = di * dy_i;
            let dx_c1 = div(v11 + v10, v4);
            let v12 = v11 - v10;
            let dx_c2 = div(v12, v4);
            let ady_i = dy_i.abs();
            let v13 = ady_i * sdel_i;
            let v14 = di * dx_i / -1.0;
            let dy_c1 = div(v14 + v13, v4);
            let v15 = v14 - v13;
            let dy_c2 = div(v15, v4);
            let v16 = x1_i - dx_c1;
            let v17 = x1_i - dx_c2;
            let v18 = y1_i - dy_c1;
            let v19 = y1_i - dy_c2;
            let v20 = v16.hypot(v18);
            let v21 = v17.hypot(v19);
            let v22 = v21 - v20;
            let dx_c = if v22 > 0.0 { dx_c1 } else { dx_c2 };
            let dy_c = if v22 > 0.0 { dy_c1 } else { dy_c2 };
            let sdx_c = div(dx_c * rw2, r_i);
            let sdy_c = div(dy_c * rh2, r_i);
            let x_c = (w / 2.0) + sdx_c;
            let y_c = (h / 2.0) + sdy_c;
            let ist0 = at2(sdx_c, sdy_c);
            let ist1 = ist0 + 21600000.0;
            let ist_ang0 = if ist0 > 0.0 { ist0 } else { ist1 };
            let isw1 = st_ang - ist_ang0;
            let isw2 = isw1 + 21600000.0;
            let isw_ang0 = if isw1 > 0.0 { isw1 } else { isw2 };
            let ist_ang = ist_ang0 + isw_ang0;
            let isw_ang = -isw_ang0;
            let p1 = x_f - x_c;
            let p2 = y_f - y_c;
            let p3 = p1.hypot(p2);
            let p4 = p3 / 2.0;
            let p5 = p4 - thh;
            let x_gp = if p5 > 0.0 { x_f } else { x_g };
            let y_gp = if p5 > 0.0 { y_f } else { y_g };
            let x_bp = if p5 > 0.0 { x_c } else { x_b };
            let y_bp = if p5 > 0.0 { y_c } else { y_b };
            let en0 = at2(sdx_f, sdy_f);
            let en1 = en0 + 21600000.0;
            let en2 = if en0 > 0.0 { en0 } else { en1 };
            let sw0 = en2 - st_ang;
            let sw1 = sw0 - 21600000.0;
            let sw_ang = if sw0 > 0.0 { sw1 } else { sw0 };
            let st_ang0 = st_ang + sw_ang;
            let sw_ang0 = -sw_ang;
            p.m(x_e, y_e);
            p.l(x_d, y_d);
            p.arc(rw2, rh2, ist_ang / 60000.0, isw_ang / 60000.0);
            p.l(x_bp, y_bp);
            p.l(x_a, y_a);
            p.l(x_gp, y_gp);
            p.l(x_f, y_f);
            p.arc(rw1, rh1, st_ang0 / 60000.0, sw_ang0 / 60000.0);
            p.close();
        }
        "leftRightCircularArrow" => {
            let adj1 = raw("adj1", 12500.0);
            let adj2 = raw("adj2", 1142319.0);
            let adj3 = raw("adj3", 20457680.0);
            let adj4 = raw("adj4", 11942319.0);
            let adj5 = raw("adj5", 12500.0);
            let a5 = pin3(0.0, adj5, 25000.0);
            let max_adj1 = a5 * 2.0;
            let a1 = pin3(0.0, adj1, max_adj1);
            let en_ang = pin3(1.0, adj3, 21600000.0);
            let st_ang = pin3(0.0, adj4, 21600000.0);
            let th = ss * a1 / 100000.0;
            let thh = ss * a5 / 100000.0;
            let th2 = th / 2.0;
            let rw1 = (w / 2.0) + th2 - thh;
            let rh1 = (h / 2.0) + th2 - thh;
            let rw2 = rw1 - th;
            let rh2 = rh1 - th;
            let rw3 = rw2 + th2;
            let rh3 = rh2 + th2;
            let wt_h = sin60(rw3, en_ang);
            let ht_h = cos60(rh3, en_ang);
            let dx_h = cat2(rw3, ht_h, wt_h);
            let dy_h = sat2(rh3, ht_h, wt_h);
            let x_h = (w / 2.0) + dx_h;
            let y_h = (h / 2.0) + dy_h;
            let r_i = rw2.min(rh2);
            let u1 = dx_h * dx_h;
            let u2 = dy_h * dy_h;
            let u3 = r_i * r_i;
            let u4 = u1 - u3;
            let u5 = u2 - u3;
            let u6 = div(u4 * u5, u1);
            let u7 = div(u6, u2);
            let u8 = 1.0 - u7;
            let u9 = u8.max(0.0).sqrt();
            let u10 = div(u4, dx_h);
            let u11 = div(u10, dy_h);
            let u12 = div(1.0 + u9, u11);
            let u13 = at2(1.0, u12);
            let u14 = u13 + 21600000.0;
            let u15 = if u13 > 0.0 { u13 } else { u14 };
            let u16 = u15 - en_ang;
            let u17 = u16 + 21600000.0;
            let u18 = if u16 > 0.0 { u16 } else { u17 };
            let u19 = u18 - 10800000.0;
            let u20 = u18 - 21600000.0;
            let u21 = if u19 > 0.0 { u20 } else { u18 };
            let max_ang = u21.abs();
            let a_ang = pin3(0.0, adj2, max_ang);
            let pt_ang = en_ang + a_ang;
            let wt_a = sin60(rw3, pt_ang);
            let ht_a = cos60(rh3, pt_ang);
            let dx_a = cat2(rw3, ht_a, wt_a);
            let dy_a = sat2(rh3, ht_a, wt_a);
            let x_a = (w / 2.0) + dx_a;
            let y_a = (h / 2.0) + dy_a;
            let dx_g = cos60(thh, pt_ang);
            let dy_g = sin60(thh, pt_ang);
            let x_g = x_h + dx_g;
            let y_g = y_h + dy_g;
            let dx_b = cos60(thh, pt_ang);
            let dy_b = sin60(thh, pt_ang);
            let x_b = x_h - dx_b;
            let y_b = y_h - dy_b;
            let sx1 = x_b - (w / 2.0);
            let sy1 = y_b - (h / 2.0);
            let sx2 = x_g - (w / 2.0);
            let sy2 = y_g - (h / 2.0);
            let r_o = rw1.min(rh1);
            let x1_o = div(sx1 * r_o, rw1);
            let y1_o = div(sy1 * r_o, rh1);
            let x2_o = div(sx2 * r_o, rw1);
            let y2_o = div(sy2 * r_o, rh1);
            let dx_o = x2_o - x1_o;
            let dy_o = y2_o - y1_o;
            let d_o = dx_o.hypot(dy_o);
            let q1 = x1_o * y2_o;
            let q2 = x2_o * y1_o;
            let v_do = q1 - q2;
            let q3 = r_o * r_o;
            let q4 = d_o * d_o;
            let q5 = q3 * q4;
            let q6 = v_do * v_do;
            let q7 = q5 - q6;
            let q8 = q7.max(0.0);
            let sdel_o = q8.max(0.0).sqrt();
            let ndy_o = -dy_o;
            let sdy_o = if ndy_o > 0.0 { -1.0 } else { 1.0 };
            let q9 = sdy_o * dx_o;
            let q10 = q9 * sdel_o;
            let q11 = v_do * dy_o;
            let dx_f1 = div(q11 + q10, q4);
            let q12 = q11 - q10;
            let dx_f2 = div(q12, q4);
            let ady_o = dy_o.abs();
            let q13 = ady_o * sdel_o;
            let q14 = v_do * dx_o / -1.0;
            let dy_f1 = div(q14 + q13, q4);
            let q15 = q14 - q13;
            let dy_f2 = div(q15, q4);
            let q16 = x2_o - dx_f1;
            let q17 = x2_o - dx_f2;
            let q18 = y2_o - dy_f1;
            let q19 = y2_o - dy_f2;
            let q20 = q16.hypot(q18);
            let q21 = q17.hypot(q19);
            let q22 = q21 - q20;
            let dx_f = if q22 > 0.0 { dx_f1 } else { dx_f2 };
            let dy_f = if q22 > 0.0 { dy_f1 } else { dy_f2 };
            let sdx_f = div(dx_f * rw1, r_o);
            let sdy_f = div(dy_f * rh1, r_o);
            let x_f = (w / 2.0) + sdx_f;
            let y_f = (h / 2.0) + sdy_f;
            let x1_i = div(sx1 * r_i, rw2);
            let y1_i = div(sy1 * r_i, rh2);
            let x2_i = div(sx2 * r_i, rw2);
            let y2_i = div(sy2 * r_i, rh2);
            let dx_i = x2_i - x1_i;
            let dy_i = y2_i - y1_i;
            let d_i = dx_i.hypot(dy_i);
            let v1 = x1_i * y2_i;
            let v2 = x2_i * y1_i;
            let di = v1 - v2;
            let v3 = r_i * r_i;
            let v4 = d_i * d_i;
            let v5 = v3 * v4;
            let v6 = di * di;
            let v7 = v5 - v6;
            let v8 = v7.max(0.0);
            let sdel_i = v8.max(0.0).sqrt();
            let v9 = sdy_o * dx_i;
            let v10 = v9 * sdel_i;
            let v11 = di * dy_i;
            let dx_c1 = div(v11 + v10, v4);
            let v12 = v11 - v10;
            let dx_c2 = div(v12, v4);
            let ady_i = dy_i.abs();
            let v13 = ady_i * sdel_i;
            let v14 = di * dx_i / -1.0;
            let dy_c1 = div(v14 + v13, v4);
            let v15 = v14 - v13;
            let dy_c2 = div(v15, v4);
            let v16 = x1_i - dx_c1;
            let v17 = x1_i - dx_c2;
            let v18 = y1_i - dy_c1;
            let v19 = y1_i - dy_c2;
            let v20 = v16.hypot(v18);
            let v21 = v17.hypot(v19);
            let v22 = v21 - v20;
            let dx_c = if v22 > 0.0 { dx_c1 } else { dx_c2 };
            let dy_c = if v22 > 0.0 { dy_c1 } else { dy_c2 };
            let sdx_c = div(dx_c * rw2, r_i);
            let sdy_c = div(dy_c * rh2, r_i);
            let x_c = (w / 2.0) + sdx_c;
            let y_c = (h / 2.0) + sdy_c;
            let wt_i = sin60(rw3, st_ang);
            let ht_i = cos60(rh3, st_ang);
            let dx_i = cat2(rw3, ht_i, wt_i);
            let dy_i = sat2(rh3, ht_i, wt_i);
            let x_i = (w / 2.0) + dx_i;
            let y_i = (h / 2.0) + dy_i;
            let lpt_ang = st_ang - a_ang;
            let wt_l = sin60(rw3, lpt_ang);
            let ht_l = cos60(rh3, lpt_ang);
            let dx_l = cat2(rw3, ht_l, wt_l);
            let dy_l = sat2(rh3, ht_l, wt_l);
            let x_l = (w / 2.0) + dx_l;
            let y_l = (h / 2.0) + dy_l;
            let dx_k = cos60(thh, lpt_ang);
            let dy_k = sin60(thh, lpt_ang);
            let x_k = x_i + dx_k;
            let y_k = y_i + dy_k;
            let dx_j = cos60(thh, lpt_ang);
            let dy_j = sin60(thh, lpt_ang);
            let x_j = x_i - dx_j;
            let y_j = y_i - dy_j;
            let p1 = x_f - x_c;
            let p2 = y_f - y_c;
            let p3 = p1.hypot(p2);
            let p4 = p3 / 2.0;
            let p5 = p4 - thh;
            let x_gp = if p5 > 0.0 { x_f } else { x_g };
            let y_gp = if p5 > 0.0 { y_f } else { y_g };
            let x_bp = if p5 > 0.0 { x_c } else { x_b };
            let y_bp = if p5 > 0.0 { y_c } else { y_b };
            let en0 = at2(sdx_f, sdy_f);
            let en1 = en0 + 21600000.0;
            let en2 = if en0 > 0.0 { en0 } else { en1 };
            let od0 = en2 - en_ang;
            let od1 = od0 + 21600000.0;
            let od2 = if od0 > 0.0 { od0 } else { od1 };
            let st0 = st_ang - od2;
            let st1 = st0 + 21600000.0;
            let st2 = if st0 > 0.0 { st0 } else { st1 };
            let sw0 = en2 - st2;
            let sw1 = sw0 + 21600000.0;
            let sw_ang = if sw0 > 0.0 { sw0 } else { sw1 };
            let ist0 = at2(sdx_c, sdy_c);
            let ist1 = ist0 + 21600000.0;
            let ist_ang = if ist0 > 0.0 { ist0 } else { ist1 };
            let id0 = ist_ang - en_ang;
            let id1 = id0 - 21600000.0;
            let id2 = if id0 > 0.0 { id1 } else { id0 };
            let ien0 = st_ang - id2;
            let ien1 = ien0 - 21600000.0;
            let ien2 = if ien1 > 0.0 { ien1 } else { ien0 };
            let isw1 = ien2 - ist_ang;
            let isw2 = isw1 - 21600000.0;
            let isw_ang = if isw1 > 0.0 { isw2 } else { isw1 };
            let wt_e = sin60(rw1, st2);
            let ht_e = cos60(rh1, st2);
            let dx_e = cat2(rw1, ht_e, wt_e);
            let dy_e = sat2(rh1, ht_e, wt_e);
            let x_e = (w / 2.0) + dx_e;
            let y_e = (h / 2.0) + dy_e;
            let wt_d = sin60(rw2, ien2);
            let ht_d = cos60(rh2, ien2);
            let dx_d = cat2(rw2, ht_d, wt_d);
            let dy_d = sat2(rh2, ht_d, wt_d);
            let x_d = (w / 2.0) + dx_d;
            let y_d = (h / 2.0) + dy_d;
            let x_kp = if p5 > 0.0 { x_e } else { x_k };
            let y_kp = if p5 > 0.0 { y_e } else { y_k };
            let x_jp = if p5 > 0.0 { x_d } else { x_j };
            let y_jp = if p5 > 0.0 { y_d } else { y_j };
            p.m(x_l, y_l);
            p.l(x_kp, y_kp);
            p.l(x_e, y_e);
            p.arc(rw1, rh1, st2 / 60000.0, sw_ang / 60000.0);
            p.l(x_gp, y_gp);
            p.l(x_a, y_a);
            p.l(x_bp, y_bp);
            p.l(x_c, y_c);
            p.arc(rw2, rh2, ist_ang / 60000.0, isw_ang / 60000.0);
            p.l(x_jp, y_jp);
            p.close();
        }
        "leftRightRibbon" => {
            let adj1 = raw("adj1", 50000.0);
            let adj2 = raw("adj2", 50000.0);
            let adj3 = raw("adj3", 16667.0);
            let a3 = pin3(0.0, adj3, 33333.0);
            let max_adj1 = 100000.0 - a3;
            let a1 = pin3(0.0, adj1, max_adj1);
            let w1 = (w / 2.0) - (w / 32.0);
            let max_adj2 = div(100000.0 * w1, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let x1 = ss * a2 / 100000.0;
            let x4 = w - x1;
            let dy1 = h * a1 / 200000.0;
            let dy2 = h * a3 / -200000.0;
            let ly1 = (h / 2.0) + dy2 - dy1;
            let ry4 = (h / 2.0) + dy1 - dy2;
            let ly2 = ly1 + dy1;
            let ry3 = h - ly2;
            let ly4 = ly2 * 2.0;
            let ry1 = h - ly4;
            let ly3 = ly4 - ly1;
            let ry2 = h - ly3;
            let h_r = a3 * ss / 400000.0;
            let x2 = (w / 2.0) - (w / 32.0);
            let x3 = (w / 2.0) + (w / 32.0);
            let y1 = ly1 + h_r;
            let y2 = ry2 - h_r;
            p.m(0.0, ly2);
            p.l(x1, 0.0);
            p.l(x1, ly1);
            p.l(w / 2.0, ly1);
            p.arc(w / 32.0, h_r, 270.0, 180.0);
            p.arc(w / 32.0, h_r, 270.0, -180.0);
            p.l(x4, ry2);
            p.l(x4, ry1);
            p.l(w, ry3);
            p.l(x4, h);
            p.l(x4, ry4);
            p.l(w / 2.0, ry4);
            p.arc(w / 32.0, h_r, 90.0, 90.0);
            p.l(x2, ly3);
            p.l(x1, ly3);
            p.l(x1, ly4);
            p.close();
            p.m(x3, y1);
            p.arc(w / 32.0, h_r, 0.0, 90.0);
            p.arc(w / 32.0, h_r, 270.0, -180.0);
            p.l(x3, ry2);
            p.close();
            p.flip();
            p.m(x3, y1);
            p.l(x3, ry2);
            p.back();
            p.m(x2, y2);
            p.l(x2, ly3);
            p.back();
        }
        "mathDivide" => {
            let adj1 = raw("adj1", 23520.0);
            let adj2 = raw("adj2", 5880.0);
            let adj3 = raw("adj3", 11760.0);
            let a1 = pin3(1000.0, adj1, 36745.0);
            let ma1 = -a1;
            let ma3h = (73490.0 + ma1) / 4.0;
            let ma3w = div(36745.0 * w, h);
            let max_adj3 = ma3h.min(ma3w);
            let a3 = pin3(1000.0, adj3, max_adj3);
            let m4a3 = -4.0 * a3;
            let max_adj2 = 73490.0 + m4a3 - a1;
            let a2 = pin3(0.0, adj2, max_adj2);
            let dy1 = h * a1 / 200000.0;
            let yg = h * a2 / 100000.0;
            let rad = h * a3 / 100000.0;
            let dx1 = w * 73490.0 / 200000.0;
            let y3 = (h / 2.0) - dy1;
            let y4 = (h / 2.0) + dy1;
            let a = yg + rad;
            let y2 = y3 - a;
            let y1 = y2 - rad;
            let y5 = h - y1;
            let x1 = (w / 2.0) - dx1;
            let x3 = (w / 2.0) + dx1;
            p.m(w / 2.0, y1);
            p.arc(rad, rad, 270.0, 360.0);
            p.close();
            p.m(w / 2.0, y5);
            p.arc(rad, rad, 90.0, 360.0);
            p.close();
            p.m(x1, y3);
            p.l(x3, y3);
            p.l(x3, y4);
            p.l(x1, y4);
            p.close();
        }
        "mathEqual" => {
            let adj1 = raw("adj1", 23520.0);
            let adj2 = raw("adj2", 11760.0);
            let a1 = pin3(0.0, adj1, 36745.0);
            let v_2a1 = a1 * 2.0;
            let m_adj2 = 100000.0 - v_2a1;
            let a2 = pin3(0.0, adj2, m_adj2);
            let dy1 = h * a1 / 100000.0;
            let dy2 = h * a2 / 200000.0;
            let dx1 = w * 73490.0 / 200000.0;
            let y2 = (h / 2.0) - dy2;
            let y3 = (h / 2.0) + dy2;
            let y1 = y2 - dy1;
            let y4 = y3 + dy1;
            let x1 = (w / 2.0) - dx1;
            let x2 = (w / 2.0) + dx1;
            p.m(x1, y1);
            p.l(x2, y1);
            p.l(x2, y2);
            p.l(x1, y2);
            p.close();
            p.m(x1, y3);
            p.l(x2, y3);
            p.l(x2, y4);
            p.l(x1, y4);
            p.close();
        }
        "mathMinus" => {
            let adj1 = raw("adj1", 23520.0);
            let a1 = pin3(0.0, adj1, 100000.0);
            let dy1 = h * a1 / 200000.0;
            let dx1 = w * 73490.0 / 200000.0;
            let y1 = (h / 2.0) - dy1;
            let y2 = (h / 2.0) + dy1;
            let x1 = (w / 2.0) - dx1;
            let x2 = (w / 2.0) + dx1;
            p.m(x1, y1);
            p.l(x2, y1);
            p.l(x2, y2);
            p.l(x1, y2);
            p.close();
        }
        "mathMultiply" => {
            let adj1 = raw("adj1", 23520.0);
            let a1 = pin3(0.0, adj1, 51965.0);
            let th = ss * a1 / 100000.0;
            let a = at2(w, h);
            let sa = sin60(1.0, a);
            let ca = cos60(1.0, a);
            let ta = tan60(1.0, a);
            let dl = w.hypot(h);
            let rw = dl * 51965.0 / 100000.0;
            let l_m = dl - rw;
            let x_m = ca * l_m / 2.0;
            let y_m = sa * l_m / 2.0;
            let dx_am = sa * th / 2.0;
            let dy_am = ca * th / 2.0;
            let x_a = x_m - dx_am;
            let y_a = y_m + dy_am;
            let x_b = x_m + dx_am;
            let y_b = y_m - dy_am;
            let x_bc = (w / 2.0) - x_b;
            let y_bc = x_bc * ta;
            let y_c = y_bc + y_b;
            let x_d = w - x_b;
            let x_e = w - x_a;
            let y_fe = (h / 2.0) - y_a;
            let x_fe = div(y_fe, ta);
            let x_f = x_e - x_fe;
            let x_l = x_a + x_fe;
            let y_g = h - y_a;
            let y_h = h - y_b;
            let y_i = h - y_c;
            p.m(x_a, y_a);
            p.l(x_b, y_b);
            p.l(w / 2.0, y_c);
            p.l(x_d, y_b);
            p.l(x_e, y_a);
            p.l(x_f, h / 2.0);
            p.l(x_e, y_g);
            p.l(x_d, y_h);
            p.l(w / 2.0, y_i);
            p.l(x_b, y_h);
            p.l(x_a, y_g);
            p.l(x_l, h / 2.0);
            p.close();
        }
        "mathNotEqual" => {
            let adj1 = raw("adj1", 23520.0);
            let adj2 = raw("adj2", 6600000.0);
            let adj3 = raw("adj3", 11760.0);
            let a1 = pin3(0.0, adj1, 50000.0);
            let cr_ang = pin3(4200000.0, adj2, 6600000.0);
            let v_2a1 = a1 * 2.0;
            let max_adj3 = 100000.0 - v_2a1;
            let a3 = pin3(0.0, adj3, max_adj3);
            let dy1 = h * a1 / 100000.0;
            let dy2 = h * a3 / 200000.0;
            let dx1 = w * 73490.0 / 200000.0;
            let x1 = (w / 2.0) - dx1;
            let x8 = (w / 2.0) + dx1;
            let y2 = (h / 2.0) - dy2;
            let y3 = (h / 2.0) + dy2;
            let y1 = y2 - dy1;
            let y4 = y3 + dy1;
            let cadj2 = cr_ang - 5400000.0;
            let xadj2 = tan60(h / 2.0, cadj2);
            let len = xadj2.hypot(h / 2.0);
            let bhw = div(len * dy1, h / 2.0);
            let bhw2 = bhw / 2.0;
            let x7 = (w / 2.0) + xadj2 - bhw2;
            let dx67 = div(xadj2 * y1, h / 2.0);
            let x6 = x7 - dx67;
            let dx57 = div(xadj2 * y2, h / 2.0);
            let x5 = x7 - dx57;
            let dx47 = div(xadj2 * y3, h / 2.0);
            let x4 = x7 - dx47;
            let dx37 = div(xadj2 * y4, h / 2.0);
            let x3 = x7 - dx37;
            let rx7 = x7 + bhw;
            let rx6 = x6 + bhw;
            let rx5 = x5 + bhw;
            let rx4 = x4 + bhw;
            let rx3 = x3 + bhw;
            let dx7 = div(dy1 * (h / 2.0), len);
            let rxt = x7 + dx7;
            let lxt = rx7 - dx7;
            let rx = if cadj2 > 0.0 { rxt } else { rx7 };
            let lx = if cadj2 > 0.0 { x7 } else { lxt };
            let dy3 = div(dy1 * xadj2, len);
            let dy4 = -dy3;
            let ry = if cadj2 > 0.0 { dy3 } else { 0.0 };
            let ly = if cadj2 > 0.0 { 0.0 } else { dy4 };
            let dlx = w - rx;
            let drx = w - lx;
            let dly = h - ry;
            let dry = h - ly;
            p.m(x1, y1);
            p.l(x6, y1);
            p.l(lx, ly);
            p.l(rx, ry);
            p.l(rx6, y1);
            p.l(x8, y1);
            p.l(x8, y2);
            p.l(rx5, y2);
            p.l(rx4, y3);
            p.l(x8, y3);
            p.l(x8, y4);
            p.l(rx3, y4);
            p.l(drx, dry);
            p.l(dlx, dly);
            p.l(x3, y4);
            p.l(x1, y4);
            p.l(x1, y3);
            p.l(x4, y3);
            p.l(x5, y2);
            p.l(x1, y2);
            p.close();
        }
        "mathPlus" => {
            let adj1 = raw("adj1", 23520.0);
            let a1 = pin3(0.0, adj1, 73490.0);
            let dx1 = w * 73490.0 / 200000.0;
            let dy1 = h * 73490.0 / 200000.0;
            let dx2 = ss * a1 / 200000.0;
            let x1 = (w / 2.0) - dx1;
            let x2 = (w / 2.0) - dx2;
            let x3 = (w / 2.0) + dx2;
            let x4 = (w / 2.0) + dx1;
            let y1 = (h / 2.0) - dy1;
            let y2 = (h / 2.0) - dx2;
            let y3 = (h / 2.0) + dx2;
            let y4 = (h / 2.0) + dy1;
            p.m(x1, y2);
            p.l(x2, y2);
            p.l(x2, y1);
            p.l(x3, y1);
            p.l(x3, y2);
            p.l(x4, y2);
            p.l(x4, y3);
            p.l(x3, y3);
            p.l(x3, y4);
            p.l(x2, y4);
            p.l(x2, y3);
            p.l(x1, y3);
            p.close();
        }
        "nonIsoscelesTrapezoid" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 25000.0);
            let max_adj = div(50000.0 * w, ss);
            let a1 = pin3(0.0, adj1, max_adj);
            let a2 = pin3(0.0, adj2, max_adj);
            let x2 = ss * a1 / 100000.0;
            let dx3 = ss * a2 / 100000.0;
            let x3 = w - dx3;
            p.m(0.0, h);
            p.l(x2, 0.0);
            p.l(x3, 0.0);
            p.l(w, h);
            p.close();
        }
        "plaqueTabs" => {
            let md = w.hypot(h);
            let dx = 1.0 * md / 20.0;
            let y1 = h - dx;
            let x1 = w - dx;
            p.m(0.0, 0.0);
            p.l(dx, 0.0);
            p.arc(dx, dx, 0.0, 90.0);
            p.close();
            p.m(0.0, y1);
            p.arc(dx, dx, 270.0, 90.0);
            p.l(0.0, h);
            p.close();
            p.m(w, 0.0);
            p.l(w, dx);
            p.arc(dx, dx, 90.0, 90.0);
            p.close();
            p.m(x1, h);
            p.arc(dx, dx, 180.0, 90.0);
            p.l(w, h);
            p.close();
        }
        "ribbon" => {
            let adj1 = raw("adj1", 16667.0);
            let adj2 = raw("adj2", 50000.0);
            let a1 = pin3(0.0, adj1, 33333.0);
            let a2 = pin3(25000.0, adj2, 75000.0);
            let x10 = w - (w / 8.0);
            let dx2 = w * a2 / 200000.0;
            let x2 = (w / 2.0) - dx2;
            let x9 = (w / 2.0) + dx2;
            let x3 = x2 + (w / 32.0);
            let x8 = x9 - (w / 32.0);
            let x5 = x2 + (w / 8.0);
            let x6 = x9 - (w / 8.0);
            let x4 = x5 - (w / 32.0);
            let x7 = x6 + (w / 32.0);
            let y1 = h * a1 / 200000.0;
            let y2 = h * a1 / 100000.0;
            let y4 = h - y2;
            let y3 = y4 / 2.0;
            let h_r = h * a1 / 400000.0;
            let y5 = h - h_r;
            let y6 = y2 - h_r;
            p.m(0.0, 0.0);
            p.l(x4, 0.0);
            p.arc(w / 32.0, h_r, 270.0, 180.0);
            p.l(x3, y1);
            p.arc(w / 32.0, h_r, 270.0, -180.0);
            p.l(x8, y2);
            p.arc(w / 32.0, h_r, 90.0, -180.0);
            p.l(x7, y1);
            p.arc(w / 32.0, h_r, 90.0, 180.0);
            p.l(w, 0.0);
            p.l(x10, y3);
            p.l(w, y4);
            p.l(x9, y4);
            p.l(x9, y5);
            p.arc(w / 32.0, h_r, 0.0, 90.0);
            p.l(x3, h);
            p.arc(w / 32.0, h_r, 90.0, 90.0);
            p.l(x2, y4);
            p.l(0.0, y4);
            p.l(w / 8.0, y3);
            p.close();
            p.m(x5, h_r);
            p.arc(w / 32.0, h_r, 0.0, 90.0);
            p.l(x3, y1);
            p.arc(w / 32.0, h_r, 270.0, -180.0);
            p.l(x5, y2);
            p.close();
            p.flip();
            p.m(x6, h_r);
            p.arc(w / 32.0, h_r, 180.0, -90.0);
            p.l(x8, y1);
            p.arc(w / 32.0, h_r, 270.0, 180.0);
            p.l(x6, y2);
            p.close();
            p.m(x5, h_r);
            p.l(x5, y2);
            p.back();
            p.m(x6, y2);
            p.l(x6, h_r);
            p.back();
            p.m(x2, y4);
            p.l(x2, y6);
            p.back();
            p.m(x9, y6);
            p.l(x9, y4);
            p.back();
        }
        "ribbon2" => {
            let adj1 = raw("adj1", 16667.0);
            let adj2 = raw("adj2", 50000.0);
            let a1 = pin3(0.0, adj1, 33333.0);
            let a2 = pin3(25000.0, adj2, 75000.0);
            let x10 = w - (w / 8.0);
            let dx2 = w * a2 / 200000.0;
            let x2 = (w / 2.0) - dx2;
            let x9 = (w / 2.0) + dx2;
            let x3 = x2 + (w / 32.0);
            let x8 = x9 - (w / 32.0);
            let x5 = x2 + (w / 8.0);
            let x6 = x9 - (w / 8.0);
            let x4 = x5 - (w / 32.0);
            let x7 = x6 + (w / 32.0);
            let dy1 = h * a1 / 200000.0;
            let y1 = h - dy1;
            let dy2 = h * a1 / 100000.0;
            let y2 = h - dy2;
            let y4 = 0.0 + dy2;
            let y3 = (y4 + h) / 2.0;
            let h_r = h * a1 / 400000.0;
            let y6 = h - h_r;
            let y7 = y1 - h_r;
            p.m(0.0, h);
            p.l(x4, h);
            p.arc(w / 32.0, h_r, 90.0, -180.0);
            p.l(x3, y1);
            p.arc(w / 32.0, h_r, 90.0, 180.0);
            p.l(x8, y2);
            p.arc(w / 32.0, h_r, 270.0, 180.0);
            p.l(x7, y1);
            p.arc(w / 32.0, h_r, 270.0, -180.0);
            p.l(w, h);
            p.l(x10, y3);
            p.l(w, y4);
            p.l(x9, y4);
            p.l(x9, h_r);
            p.arc(w / 32.0, h_r, 0.0, -90.0);
            p.l(x3, 0.0);
            p.arc(w / 32.0, h_r, 270.0, -90.0);
            p.l(x2, y4);
            p.l(0.0, y4);
            p.l(w / 8.0, y3);
            p.close();
            p.m(x5, y6);
            p.arc(w / 32.0, h_r, 0.0, -90.0);
            p.l(x3, y1);
            p.arc(w / 32.0, h_r, 90.0, 180.0);
            p.l(x5, y2);
            p.close();
            p.flip();
            p.m(x6, y6);
            p.arc(w / 32.0, h_r, 180.0, 90.0);
            p.l(x8, y1);
            p.arc(w / 32.0, h_r, 90.0, -180.0);
            p.l(x6, y2);
            p.close();
            p.m(x5, y2);
            p.l(x5, y6);
            p.back();
            p.m(x6, y6);
            p.l(x6, y2);
            p.back();
            p.m(x2, y7);
            p.l(x2, y4);
            p.back();
            p.m(x9, y4);
            p.l(x9, y7);
            p.back();
        }
        "squareTabs" => {
            let md = w.hypot(h);
            let dx = 1.0 * md / 20.0;
            let y1 = h - dx;
            let x1 = w - dx;
            p.m(0.0, 0.0);
            p.l(dx, 0.0);
            p.l(dx, dx);
            p.l(0.0, dx);
            p.close();
            p.m(0.0, y1);
            p.l(dx, y1);
            p.l(dx, h);
            p.l(0.0, h);
            p.close();
            p.m(x1, 0.0);
            p.l(w, 0.0);
            p.l(w, dx);
            p.l(x1, dx);
            p.close();
            p.m(x1, y1);
            p.l(w, y1);
            p.l(w, h);
            p.l(x1, h);
            p.close();
        }
        "swooshArrow" => {
            let adj1 = raw("adj1", 25000.0);
            let adj2 = raw("adj2", 16667.0);
            let a1 = pin3(1.0, adj1, 75000.0);
            let max_adj2 = div(70000.0 * w, ss);
            let a2 = pin3(0.0, adj2, max_adj2);
            let ad1 = h * a1 / 100000.0;
            let ad2 = ss * a2 / 100000.0;
            let x_b = w - ad2;
            let y_b = 0.0 + (ss / 8.0);
            let alfa = 5400000.0 / 14.0;
            let dx0 = tan60(ss / 8.0, alfa);
            let x_c = x_b - dx0;
            let dx1 = tan60(ad1, alfa);
            let y_f = y_b + ad1;
            let x_f = x_b + dx1;
            let x_e = x_f + dx0;
            let y_e = y_f + (ss / 8.0);
            let dy2 = y_e - 0.0;
            let dy22 = dy2 / 2.0;
            let dy3 = h / 20.0;
            let y_d = 0.0 + dy22 - dy3;
            let dy4 = h / 6.0;
            let y_p1 = (h / 6.0) + dy4;
            let x_p1 = w / 6.0;
            let dy5 = (h / 6.0) / 2.0;
            let y_p2 = y_f + dy5;
            let x_p2 = w / 4.0;
            p.m(0.0, h);
            p.quad((x_p1, y_p1), (x_b, y_b));
            p.l(x_c, 0.0);
            p.l(w, y_d);
            p.l(x_e, y_e);
            p.l(x_f, y_f);
            p.quad((x_p2, y_p2), (0.0, h));
            p.close();
        }
        "verticalScroll" => {
            let adj = raw("adj", 12500.0);
            let a = pin3(0.0, adj, 25000.0);
            let ch = ss * a / 100000.0;
            let ch2 = ch / 2.0;
            let ch4 = ch / 4.0;
            let x3 = ch + ch2;
            let x4 = ch + ch;
            let x6 = w - ch;
            let x7 = w - ch2;
            let y3 = h - ch;
            let y4 = h - ch2;
            p.m(ch2, h);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.l(ch2, y4);
            p.arc(ch4, ch4, 90.0, -180.0);
            p.l(ch, y3);
            p.l(ch, ch2);
            p.arc(ch2, ch2, 180.0, 90.0);
            p.l(x7, 0.0);
            p.arc(ch2, ch2, 270.0, 180.0);
            p.l(x6, ch);
            p.l(x6, y4);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.close();
            p.m(x4, ch2);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.arc(ch4, ch4, 90.0, 180.0);
            p.close();
            p.m(x4, ch2);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.arc(ch4, ch4, 90.0, 180.0);
            p.close();
            p.m(ch, y4);
            p.arc(ch2, ch2, 0.0, 270.0);
            p.arc(ch4, ch4, 270.0, 180.0);
            p.close();
            p.m(ch, y3);
            p.l(ch, ch2);
            p.arc(ch2, ch2, 180.0, 90.0);
            p.l(x7, 0.0);
            p.arc(ch2, ch2, 270.0, 180.0);
            p.l(x6, ch);
            p.l(x6, y4);
            p.arc(ch2, ch2, 0.0, 90.0);
            p.l(ch2, h);
            p.arc(ch2, ch2, 90.0, 180.0);
            p.close();
            p.m(x3, 0.0);
            p.arc(ch2, ch2, 270.0, 180.0);
            p.arc(ch4, ch4, 90.0, 180.0);
            p.l(x4, ch2);
            p.back();
            p.m(x6, ch);
            p.l(x3, ch);
            p.back();
            p.m(ch2, y3);
            p.arc(ch4, ch4, 270.0, 180.0);
            p.l(ch, y4);
            p.back();
            p.m(ch2, h);
            p.arc(ch2, ch2, 90.0, -90.0);
            p.l(ch, y3);
            p.back();
        }
        "gear6" => {
            let adj1 = raw("adj1", 15000.0);
            let adj2 = raw("adj2", 3526.0);
            let a1 = pin3(0.0, adj1, 20000.0);
            let a2 = pin3(0.0, adj2, 5358.0);
            let th = ss * a1 / 100000.0;
            let l_fd = ss * a2 / 100000.0;
            let th2 = th / 2.0;
            let l2 = l_fd / 2.0;
            let l3 = th2 + l2;
            let rh = (h / 2.0) - th;
            let rw = (w / 2.0) - th;
            let dr = rw - rh;
            let maxr = if dr > 0.0 { rh } else { rw };
            let ha = at2(maxr, l3);
            let a_a1 = 19800000.0 - ha;
            let a_d1 = 19800000.0 + ha;
            let ta11 = cos60(rw, a_a1);
            let ta12 = sin60(rh, a_a1);
            let b_a1 = at2(ta11, ta12);
            let cta1 = cos60(rh, b_a1);
            let sta1 = sin60(rw, b_a1);
            let ma1 = cta1.hypot(sta1);
            let na1 = div(rw * rh, ma1);
            let dxa1 = cos60(na1, b_a1);
            let dya1 = sin60(na1, b_a1);
            let x_a1 = (w / 2.0) + dxa1;
            let y_a1 = (h / 2.0) + dya1;
            let td11 = cos60(rw, a_d1);
            let td12 = sin60(rh, a_d1);
            let b_d1 = at2(td11, td12);
            let ctd1 = cos60(rh, b_d1);
            let std1 = sin60(rw, b_d1);
            let md1 = ctd1.hypot(std1);
            let nd1 = div(rw * rh, md1);
            let dxd1 = cos60(nd1, b_d1);
            let dyd1 = sin60(nd1, b_d1);
            let x_d1 = (w / 2.0) + dxd1;
            let y_d1 = (h / 2.0) + dyd1;
            let x_ad1 = x_a1 - x_d1;
            let y_ad1 = y_a1 - y_d1;
            let a1 = at2(y_ad1, x_ad1);
            let dx_f1 = sin60(l_fd, a1);
            let dy_f1 = cos60(l_fd, a1);
            let x_f1 = x_d1 + dx_f1;
            let y_f1 = y_d1 + dy_f1;
            let x_e1 = x_a1 - dx_f1;
            let y_e1 = y_a1 - dy_f1;
            let y_c1t = sin60(th, a1);
            let x_c1t = cos60(th, a1);
            let y_c1 = y_f1 + y_c1t;
            let x_c1 = x_f1 - x_c1t;
            let y_b1 = y_e1 + y_c1t;
            let x_b1 = x_e1 - x_c1t;
            let a_d6 = 16200000.0 + ha;
            let td61 = cos60(rw, a_d6);
            let td62 = sin60(rh, a_d6);
            let b_d6 = at2(td61, td62);
            let ctd6 = cos60(rh, b_d6);
            let std6 = sin60(rw, b_d6);
            let md6 = ctd6.hypot(std6);
            let nd6 = div(rw * rh, md6);
            let dxd6 = cos60(nd6, b_d6);
            let dyd6 = sin60(nd6, b_d6);
            let x_d6 = (w / 2.0) + dxd6;
            let y_d6 = (h / 2.0) + dyd6;
            let x_a6 = (w / 2.0) - dxd6;
            let x_f6 = x_d6 - l_fd;
            let x_e6 = x_a6 + l_fd;
            let y_c6 = y_d6 - th;
            let sw_ang1 = b_a1 - b_d6;
            let a_a2 = 1800000.0 - ha;
            let a_d2 = 1800000.0 + ha;
            let ta21 = cos60(rw, a_a2);
            let ta22 = sin60(rh, a_a2);
            let b_a2 = at2(ta21, ta22);
            let y_a2 = h - y_d1;
            let td21 = cos60(rw, a_d2);
            let td22 = sin60(rh, a_d2);
            let b_d2 = at2(td21, td22);
            let y_d2 = h - y_a1;
            let y_c2 = h - y_b1;
            let y_b2 = h - y_c1;
            let sw_ang2 = b_a2 - b_d1;
            let a_d3 = 5400000.0 + ha;
            let td31 = cos60(rw, a_d3);
            let td32 = sin60(rh, a_d3);
            let b_d3 = at2(td31, td32);
            let y_d3 = h - y_d6;
            let y_b3 = h - y_c6;
            let a_d4 = 9000000.0 + ha;
            let td41 = cos60(rw, a_d4);
            let td42 = sin60(rh, a_d4);
            let b_d4 = at2(td41, td42);
            let x_d4 = w - x_d1;
            let x_c4 = w - x_c1;
            let x_b4 = w - x_b1;
            let a_d5 = 12600000.0 + ha;
            let td51 = cos60(rw, a_d5);
            let td52 = sin60(rh, a_d5);
            let b_d5 = at2(td51, td52);
            let x_d5 = w - x_a1;
            let x_c5 = w - x_b1;
            let x_b5 = w - x_c1;
            p.m(x_a1, y_a1);
            p.l(x_b1, y_b1);
            p.l(x_c1, y_c1);
            p.l(x_d1, y_d1);
            p.arc(rw, rh, b_d1 / 60000.0, sw_ang2 / 60000.0);
            p.l(x_c1, y_b2);
            p.l(x_b1, y_c2);
            p.l(x_a1, y_d2);
            p.arc(rw, rh, b_d2 / 60000.0, sw_ang1 / 60000.0);
            p.l(x_f6, y_b3);
            p.l(x_e6, y_b3);
            p.l(x_a6, y_d3);
            p.arc(rw, rh, b_d3 / 60000.0, sw_ang1 / 60000.0);
            p.l(x_b4, y_c2);
            p.l(x_c4, y_b2);
            p.l(x_d4, y_a2);
            p.arc(rw, rh, b_d4 / 60000.0, sw_ang2 / 60000.0);
            p.l(x_b5, y_c1);
            p.l(x_c5, y_b1);
            p.l(x_d5, y_a1);
            p.arc(rw, rh, b_d5 / 60000.0, sw_ang1 / 60000.0);
            p.l(x_e6, y_c6);
            p.l(x_f6, y_c6);
            p.l(x_d6, y_d6);
            p.arc(rw, rh, b_d6 / 60000.0, sw_ang1 / 60000.0);
            p.close();
        }
        "funnel" => {
            let d = ss / 20.0;
            let rw2 = (w / 2.0) - d;
            let rh2 = (h / 4.0) - d;
            let t1 = cos60(w / 2.0, 480000.0);
            let t2 = sin60(h / 4.0, 480000.0);
            let da = at2(t1, t2);
            let v_2da = da * 2.0;
            let st_ang1 = 10800000.0 - da;
            let sw_ang1 = 10800000.0 + v_2da;
            let sw_ang3 = 10800000.0 - v_2da;
            let rw3 = (w / 2.0) / 4.0;
            let rh3 = (h / 4.0) / 4.0;
            let ct1 = cos60(h / 4.0, st_ang1);
            let st1 = sin60(w / 2.0, st_ang1);
            let m1 = ct1.hypot(st1);
            let n1 = div((w / 2.0) * (h / 4.0), m1);
            let dx1 = cos60(n1, st_ang1);
            let dy1 = sin60(n1, st_ang1);
            let x1 = (w / 2.0) + dx1;
            let y1 = (h / 4.0) + dy1;
            let ct3 = cos60(rh3, da);
            let st3 = sin60(rw3, da);
            let m3 = ct3.hypot(st3);
            let n3 = div(rw3 * rh3, m3);
            let dx3 = cos60(n3, da);
            let dy3 = sin60(n3, da);
            let x3 = (w / 2.0) + dx3;
            let vc3 = h - rh3;
            let y2 = vc3 + dy3;
            let x2 = (w / 2.0) - rw2;
            p.m(x1, y1);
            p.arc(w / 2.0, h / 4.0, st_ang1 / 60000.0, sw_ang1 / 60000.0);
            p.l(x3, y2);
            p.arc(rw3, rh3, da / 60000.0, sw_ang3 / 60000.0);
            p.close();
            p.m(x2, h / 4.0);
            p.arc(rw2, rh2, 180.0, -360.0);
            p.close();
        }
        "moon" => {
            let adj = raw("adj", 50000.0);
            let a = pin3(0.0, adj, 87500.0);
            let g0 = ss * a / 100000.0;
            let g0w = div(g0 * w, ss);
            let g1 = ss - g0;
            let g2 = div(g0 * g0, g1);
            let g3 = div(ss * ss, g1);
            let g4 = g3 * 2.0;
            let g5 = g4 - g2;
            let g6 = g5 - g0;
            let g6w = div(g6 * w, ss);
            let g7 = g5 / 2.0;
            let g8 = g7 - g0;
            let dy1 = div(g8 * (h / 2.0), ss);
            let g17w = g6w - g0w;
            let g18w = g17w / 2.0;
            let dx2p = g0w + g18w - w;
            let dx2 = -dx2p;
            let dy2 = -(h / 2.0);
            let st_ang1 = at2(dx2, dy2);
            let en_angp1 = at2(dx2, h / 2.0);
            let en_ang1 = en_angp1 - 21600000.0;
            let sw_ang1 = en_ang1 - st_ang1;
            p.m(w, h);
            p.arc(w, h / 2.0, 90.0, 180.0);
            p.arc(g18w, dy1, st_ang1 / 60000.0, sw_ang1 / 60000.0);
            p.close();
        }
        "actionButtonBackPrevious" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g11, h / 2.0);
            p.l(g12, g9);
            p.l(g12, g10);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonBeginning" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 / 8.0;
            let g15 = g13 / 4.0;
            let g16 = g11 + g14;
            let g17 = g11 + g15;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g17, h / 2.0);
            p.l(g12, g9);
            p.l(g12, g10);
            p.close();
            p.m(g16, g9);
            p.l(g11, g9);
            p.l(g11, g10);
            p.l(g16, g10);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonDocument" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let dx1 = ss * 9.0 / 32.0;
            let g11 = (w / 2.0) - dx1;
            let g12 = (w / 2.0) + dx1;
            let g13 = ss * 3.0 / 16.0;
            let g14 = g12 - g13;
            let g15 = g9 + g13;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g11, g9);
            p.l(g14, g9);
            p.l(g12, g15);
            p.l(g12, g10);
            p.l(g11, g10);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonEnd" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 * 3.0 / 4.0;
            let g15 = g13 * 7.0 / 8.0;
            let g16 = g11 + g14;
            let g17 = g11 + g15;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g16, h / 2.0);
            p.l(g11, g9);
            p.l(g11, g10);
            p.close();
            p.m(g17, g9);
            p.l(g12, g9);
            p.l(g12, g10);
            p.l(g17, g10);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonForwardNext" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g12, h / 2.0);
            p.l(g11, g9);
            p.l(g11, g10);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonHelp" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g11 = (w / 2.0) - dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 / 7.0;
            let g15 = g13 * 3.0 / 14.0;
            let g16 = g13 * 2.0 / 7.0;
            let g19 = g13 * 3.0 / 7.0;
            let g20 = g13 * 4.0 / 7.0;
            let g21 = g13 * 17.0 / 28.0;
            let g23 = g13 * 21.0 / 28.0;
            let g24 = g13 * 11.0 / 14.0;
            let g27 = g9 + g16;
            let g29 = g9 + g21;
            let g30 = g9 + g23;
            let g31 = g9 + g24;
            let g33 = g11 + g15;
            let g36 = g11 + g19;
            let g37 = g11 + g20;
            let g41 = g13 / 14.0;
            let g42 = g13 * 3.0 / 28.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g33, g27);
            p.arc(g16, g16, 180.0, 180.0);
            p.arc(g14, g15, 0.0, 90.0);
            p.arc(g41, g42, 270.0, -90.0);
            p.l(g37, g30);
            p.l(g36, g30);
            p.l(g36, g29);
            p.arc(g14, g15, 180.0, 90.0);
            p.arc(g41, g42, 90.0, -90.0);
            p.arc(g14, g14, 0.0, -180.0);
            p.close();
            p.m(w / 2.0, g31);
            p.arc(g42, g42, 270.0, 360.0);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonHome" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 / 16.0;
            let g15 = g13 / 8.0;
            let g16 = g13 * 3.0 / 16.0;
            let g17 = g13 * 5.0 / 16.0;
            let g20 = g13 * 11.0 / 16.0;
            let g22 = g13 * 13.0 / 16.0;
            let g23 = g13 * 7.0 / 8.0;
            let g24 = g9 + g14;
            let g25 = g9 + g16;
            let g26 = g9 + g17;
            let g28 = g11 + g15;
            let g31 = g11 + g20;
            let g32 = g11 + g22;
            let g33 = g11 + g23;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(w / 2.0, g9);
            p.l(g11, h / 2.0);
            p.l(g28, h / 2.0);
            p.l(g28, g10);
            p.l(g33, g10);
            p.l(g33, h / 2.0);
            p.l(g12, h / 2.0);
            p.l(g32, g26);
            p.l(g32, g24);
            p.l(g31, g24);
            p.l(g31, g25);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonInformation" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(w / 2.0, g9);
            p.arc(dx2, dx2, 270.0, 360.0);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonMovie" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 * 1455.0 / 21600.0;
            let g15 = g13 * 1905.0 / 21600.0;
            let g16 = g13 * 2325.0 / 21600.0;
            let g17 = g13 * 16155.0 / 21600.0;
            let g18 = g13 * 17010.0 / 21600.0;
            let g19 = g13 * 19335.0 / 21600.0;
            let g20 = g13 * 19725.0 / 21600.0;
            let g21 = g13 * 20595.0 / 21600.0;
            let g22 = g13 * 5280.0 / 21600.0;
            let g23 = g13 * 5730.0 / 21600.0;
            let g24 = g13 * 6630.0 / 21600.0;
            let g25 = g13 * 7492.0 / 21600.0;
            let g26 = g13 * 9067.0 / 21600.0;
            let g27 = g13 * 9555.0 / 21600.0;
            let g28 = g13 * 13342.0 / 21600.0;
            let g29 = g13 * 14580.0 / 21600.0;
            let g30 = g13 * 15592.0 / 21600.0;
            let g31 = g11 + g14;
            let g32 = g11 + g15;
            let g33 = g11 + g16;
            let g34 = g11 + g17;
            let g35 = g11 + g18;
            let g36 = g11 + g19;
            let g37 = g11 + g20;
            let g38 = g11 + g21;
            let g39 = g9 + g22;
            let g40 = g9 + g23;
            let g41 = g9 + g24;
            let g42 = g9 + g25;
            let g43 = g9 + g26;
            let g44 = g9 + g27;
            let g45 = g9 + g28;
            let g46 = g9 + g29;
            let g47 = g9 + g30;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g11, g39);
            p.l(g11, g44);
            p.l(g31, g44);
            p.l(g32, g43);
            p.l(g33, g43);
            p.l(g33, g47);
            p.l(g35, g47);
            p.l(g35, g45);
            p.l(g36, g45);
            p.l(g38, g46);
            p.l(g12, g46);
            p.l(g12, g41);
            p.l(g38, g41);
            p.l(g37, g42);
            p.l(g35, g42);
            p.l(g35, g41);
            p.l(g34, g40);
            p.l(g32, g40);
            p.l(g31, g39);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonReturn" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g12 = (w / 2.0) + dx2;
            let g13 = ss * 3.0 / 4.0;
            let g14 = g13 * 7.0 / 8.0;
            let g15 = g13 * 3.0 / 4.0;
            let g16 = g13 * 5.0 / 8.0;
            let g17 = g13 * 3.0 / 8.0;
            let g18 = g13 / 4.0;
            let g19 = g9 + g15;
            let g20 = g9 + g16;
            let g21 = g9 + g18;
            let g22 = g11 + g14;
            let g23 = g11 + g15;
            let g24 = g11 + g16;
            let g25 = g11 + g17;
            let g26 = g11 + g18;
            let g27 = g13 / 8.0;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g12, g21);
            p.l(g23, g9);
            p.l(w / 2.0, g21);
            p.l(g24, g21);
            p.l(g24, g20);
            p.arc(g27, g27, 0.0, 90.0);
            p.l(g25, g19);
            p.arc(g27, g27, 90.0, 90.0);
            p.l(g26, g21);
            p.l(g11, g21);
            p.l(g11, g20);
            p.arc(g17, g17, 180.0, -90.0);
            p.l(w / 2.0, g10);
            p.arc(g17, g17, 90.0, -90.0);
            p.l(g22, g21);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        "actionButtonSound" => {
            let dx2 = ss * 3.0 / 8.0;
            let g9 = (h / 2.0) - dx2;
            let g10 = (h / 2.0) + dx2;
            let g11 = (w / 2.0) - dx2;
            let g13 = ss * 3.0 / 4.0;
            let g15 = g13 * 5.0 / 16.0;
            let g16 = g13 * 5.0 / 8.0;
            let g17 = g13 * 11.0 / 16.0;
            let g21 = g9 + g15;
            let g22 = g9 + g17;
            let g24 = g11 + g15;
            let g25 = g11 + g16;
            p.m(0.0, 0.0);
            p.l(w, 0.0);
            p.l(w, h);
            p.l(0.0, h);
            p.close();
            p.m(g11, g21);
            p.l(g11, g22);
            p.l(g24, g22);
            p.l(g25, g10);
            p.l(g25, g9);
            p.l(g24, g21);
            p.close();
            return Some(Geometry::PathEvenOdd(p.cmds));
        }
        _ => return None,
    }
    Some(p.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults(_: &str, def: f32) -> f32 {
        def
    }

    /// Presets drawn by [`outline`].
    const DRAWN: &[&str] = &[
        "snip1Rect",
        "snip2SameRect",
        "snip2DiagRect",
        "snipRoundRect",
        "foldedCorner",
        "bevel",
        "cube",
        "pieWedge",
        "chord",
        "arc",
        "blockArc",
        "lineInv",
        "bentArrow",
        "bentUpArrow",
        "uturnArrow",
        "leftUpArrow",
        "leftRightUpArrow",
        "quadArrow",
        "upDownArrow",
        "leftArrowCallout",
        "rightArrowCallout",
        "upArrowCallout",
        "downArrowCallout",
        "leftRightArrowCallout",
        "upDownArrowCallout",
        "quadArrowCallout",
        "accentBorderCallout1",
        "accentBorderCallout2",
        "accentBorderCallout3",
        "accentCallout1",
        "accentCallout2",
        "accentCallout3",
        "borderCallout1",
        "borderCallout2",
        "borderCallout3",
        "bracePair",
        "bracketPair",
        "callout1",
        "callout2",
        "callout3",
        "chartPlus",
        "chartStar",
        "chartX",
        "circularArrow",
        "cloud",
        "cloudCallout",
        "cornerTabs",
        "curvedDownArrow",
        "curvedLeftArrow",
        "curvedRightArrow",
        "curvedUpArrow",
        "doubleWave",
        "ellipseRibbon",
        "ellipseRibbon2",
        "flowChartCollate",
        "flowChartDisplay",
        "flowChartInternalStorage",
        "flowChartMultidocument",
        "flowChartOfflineStorage",
        "flowChartOnlineStorage",
        "flowChartPredefinedProcess",
        "flowChartSort",
        "gear9",
        "horizontalScroll",
        "irregularSeal1",
        "irregularSeal2",
        "leftCircularArrow",
        "leftRightCircularArrow",
        "leftRightRibbon",
        "mathDivide",
        "mathEqual",
        "mathMinus",
        "mathMultiply",
        "mathNotEqual",
        "mathPlus",
        "nonIsoscelesTrapezoid",
        "plaqueTabs",
        "ribbon",
        "ribbon2",
        "squareTabs",
        "swooshArrow",
        "verticalScroll",
        "gear6",
        "funnel",
        "moon",
        "actionButtonBackPrevious",
        "actionButtonBeginning",
        "actionButtonDocument",
        "actionButtonEnd",
        "actionButtonForwardNext",
        "actionButtonHelp",
        "actionButtonHome",
        "actionButtonInformation",
        "actionButtonMovie",
        "actionButtonReturn",
        "actionButtonSound",
    ];

    #[test]
    fn every_drawn_preset_is_not_a_rectangle() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 4_000_000.0,
            h: 2_000_000.0,
        };
        for name in DRAWN {
            let (cmds, filled) = match outline(name, &defaults, r) {
                Some(Geometry::Path(c)) | Some(Geometry::PathEvenOdd(c)) => (c, true),
                // Strokes only (arcs, lines): never a box.
                Some(Geometry::Open(c)) => (c, false),
                other => panic!("{name} gave {other:?}"),
            };
            let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            let is_box = cmds.iter().all(|k| match *k {
                PathCmd::Move(x, y) | PathCmd::Line(x, y) => corners.contains(&(x, y)),
                PathCmd::Close => true,
                PathCmd::Cubic(..) => false,
            });
            // One closed subpath on the corners is a rectangle; the chart
            // symbols add strokes inside it, which makes them something else.
            let subpaths = cmds.iter().filter(|k| matches!(k, PathCmd::Move(..))).count();
            assert!(!(filled && is_box && subpaths == 1), "{name} is a plain rectangle");
            assert!(
                cmds.iter().all(|k| match *k {
                    PathCmd::Move(x, y) | PathCmd::Line(x, y) => x.is_finite() && y.is_finite(),
                    PathCmd::Cubic(a, b, c, d, e, f) => [a, b, c, d, e, f].iter().all(|v| v.is_finite()),
                    PathCmd::Close => true,
                }),
                "{name} has a non-finite coordinate"
            );
        }
    }

    fn end_points(cmds: &[PathCmd]) -> Vec<(f32, f32)> {
        cmds.iter()
            .filter_map(|c| match *c {
                PathCmd::Move(x, y) | PathCmd::Line(x, y) | PathCmd::Cubic(_, _, _, _, x, y) => Some((x, y)),
                PathCmd::Close => None,
            })
            .collect()
    }

    /// Shoelace area of the end points (the curves of the tests are straight).
    fn signed_area(cmds: &[PathCmd]) -> f32 {
        let pts = end_points(cmds);
        pts.iter()
            .zip(pts.iter().cycle().skip(1))
            .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
            .sum::<f32>()
            / 2.0
    }

    #[test]
    fn unknown_names_are_not_drawn() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        assert!(outline("noSuchShape", &defaults, r).is_none());
        assert!(outline("rect", &defaults, r).is_none());
    }

    #[test]
    fn adjust_values_move_the_outline() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w: 4_000_000.0,
            h: 2_000_000.0,
        };
        let wide = |n: &str, d: f32| if n == "adj" { 40000.0 } else { d };
        assert_ne!(outline("snip1Rect", &defaults, r), outline("snip1Rect", &wide, r));
    }

    #[test]
    fn retraced_strokes_enclose_no_area() {
        let mut p = Pen::new(100.0, 100.0);
        p.m(10.0, 10.0);
        p.l(60.0, 10.0);
        p.l(60.0, 50.0);
        p.back();
        assert!(signed_area(&p.cmds).abs() < 1e-6);
        assert_eq!(end_points(&p.cmds).len(), 5);
    }

    #[test]
    fn flipping_a_subpath_reverses_its_winding() {
        let mut p = Pen::new(100.0, 100.0);
        p.poly(&[(0.0, 0.0), (50.0, 0.0), (50.0, 50.0), (0.0, 50.0)]);
        let before = signed_area(&p.cmds);
        p.flip();
        assert!(before > 0.0 && signed_area(&p.cmds) < 0.0);
        assert_eq!(p.cmds.last(), Some(&PathCmd::Close));
    }

    #[test]
    fn a_quarter_arc_ends_a_quarter_turn_on() {
        // Start of a circle of radius 50 centred on (50, 50), at 180 degrees.
        let mut p = Pen::new(100.0, 100.0);
        p.m(0.0, 50.0);
        p.arc(50.0, 50.0, 180.0, 90.0);
        let (x, y) = p.cur;
        assert!((x - 50.0).abs() < 1e-3 && y.abs() < 1e-3, "ended at {:?}", p.cur);
    }

    #[test]
    fn visual_angles_are_measured_in_path_space() {
        // 45 to 135 degrees on a 40 x 20 ellipse of a path stretched 2:1.
        // In path space the arc runs from parametric angle 63.4 to 116.6
        // degrees, so it ends 2 * 40 * 2 * cos(63.4) = 71.55 units to the left
        // of where it started and level with it; reading the angles in shape
        // space (80 x 20) would end 2 * 80 * cos(76.0) = 38.8 to the left.
        let mut p = Pen::new(1000.0, 1000.0);
        p.m(0.0, 0.0);
        p.arc_in(2.0, 1.0, 40.0, 20.0, 45.0, 90.0);
        let (x, y) = p.cur;
        assert!((x + 71.554).abs() < 0.01 && y.abs() < 0.01, "ended at {:?}", p.cur);
    }
}
