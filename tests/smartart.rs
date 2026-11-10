//! SmartArt written without a saved drawing is laid out from its data model,
//! through the same entry points a deck takes (`parse`, `convert`).

use slidepdf::ir::{Deck, Element, Fill, Rgb, Shape};
use std::io::{Cursor, Read, Write};

const DECK: &[u8] = include_bytes!("fixtures/deck.pptx");
const LAID_OUT: &str = "SmartArt laid out by slidepdf, not PowerPoint";
const SKIPPED: &str = "SmartArt skipped (no saved drawing)";
const FRAME: (i64, i64, i64, i64) = (1_000_000, 1_000_000, 6_000_000, 4_000_000);

/// A data model of top-level nodes, each with optional children. `layout` is
/// the document node's `loTypeId`.
fn data_model(layout: &str, nodes: &[(&str, &[&str])]) -> String {
    let mut pts = format!(r#"<dgm:pt modelId="doc" type="doc"><dgm:prSet loTypeId="{layout}"/></dgm:pt>"#);
    let mut cxns = String::new();
    let mut id = 0;
    for (i, (text, kids)) in nodes.iter().enumerate() {
        let parent = id;
        pts += &node_pt(id, text);
        cxns += &format!(r#"<dgm:cxn modelId="c{id}" srcId="doc" destId="n{id}" srcOrd="{i}" destOrd="0"/>"#);
        id += 1;
        for (j, kid) in kids.iter().enumerate() {
            pts += &node_pt(id, kid);
            cxns += &format!(r#"<dgm:cxn modelId="c{id}" srcId="n{parent}" destId="n{id}" srcOrd="{j}" destOrd="0"/>"#);
            id += 1;
        }
    }
    wrap_model(&pts, &cxns)
}

fn node_pt(id: usize, text: &str) -> String {
    format!(r#"<dgm:pt modelId="n{id}"><dgm:t><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></dgm:t></dgm:pt>"#)
}

fn wrap_model(pts: &str, cxns: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dgm:ptLst>{pts}</dgm:ptLst><dgm:cxnLst>{cxns}</dgm:cxnLst></dgm:dataModel>"#
    )
}

fn frame_xml() -> String {
    let (x, y, w, h) = FRAME;
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="Diagram"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="rId2" r:lo="rId3" r:qs="rId4" r:cs="rId5"/></a:graphicData></a:graphic></p:graphicFrame>"#
    )
}

/// The frame inside a group turned `rot` (60000ths of a degree) about its centre.
fn turned_group(rot: i64) -> String {
    let (x, y, w, h) = FRAME;
    format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="9" name="g"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm rot="{rot}"><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/><a:chOff x="{x}" y="{y}"/><a:chExt cx="{w}" cy="{h}"/></a:xfrm></p:grpSpPr>{}</p:grpSp>"#,
        frame_xml()
    )
}

struct Parts<'a> {
    slide_body: String,
    data: Option<&'a str>,
    colors: Option<&'a str>,
    drawing: Option<&'a str>,
}

/// The fixture deck with its first slide replaced by one holding `slide_body`
/// and the diagram parts it points at.
fn deck(p: &Parts) -> Vec<u8> {
    let slide = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{}</p:spTree></p:cSld></p:sld>"#,
        p.slide_body
    );
    let rel = |id: &str, kind: &str, target: &str| {
        format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
        )
    };
    let mut rels = rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml");
    if p.data.is_some() {
        rels += &rel("rId2", "diagramData", "../diagrams/data1.xml");
    }
    if p.colors.is_some() {
        rels += &rel("rId5", "diagramColors", "../diagrams/colors1.xml");
    }
    if p.drawing.is_some() {
        rels += &rel("rId6", "diagramDrawing", "../diagrams/drawing1.xml");
    }
    let rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
    );

    let mut zin = zip::ZipArchive::new(Cursor::new(DECK)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).unwrap();
        match name.as_str() {
            "ppt/slides/slide1.xml" => bytes = slide.clone().into_bytes(),
            "ppt/slides/_rels/slide1.xml.rels" => bytes = rels.clone().into_bytes(),
            _ => {}
        }
        out.start_file(name, opts).unwrap();
        out.write_all(&bytes).unwrap();
    }
    for (name, xml) in [
        ("ppt/diagrams/data1.xml", p.data),
        ("ppt/diagrams/colors1.xml", p.colors),
        ("ppt/diagrams/drawing1.xml", p.drawing),
    ] {
        if let Some(xml) = xml {
            out.start_file(name, opts).unwrap();
            out.write_all(xml.as_bytes()).unwrap();
        }
    }
    out.finish().unwrap().into_inner()
}

fn plain(data: &str) -> Vec<u8> {
    deck(&Parts {
        slide_body: frame_xml(),
        data: Some(data),
        colors: None,
        drawing: None,
    })
}

fn parse(bytes: &[u8]) -> Deck {
    slidepdf::parse(bytes, true).unwrap()
}

fn messages(deck: &Deck) -> Vec<&str> {
    deck.warnings.iter().map(|w| w.message.as_str()).collect()
}

fn shapes(deck: &Deck) -> Vec<&Shape> {
    deck.slides[0]
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Shape(s) => Some(s),
            Element::Picture(_) => None,
        })
        .collect()
}

fn text_of(s: &Shape) -> String {
    s.text
        .iter()
        .flat_map(|t| &t.paragraphs)
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("|")
}

/// The shapes that carry text, with it.
fn labelled(deck: &Deck) -> Vec<(String, &Shape)> {
    shapes(deck)
        .into_iter()
        .map(|s| (text_of(s), s))
        .filter(|(t, _)| !t.is_empty())
        .collect()
}

#[test]
fn a_diagram_without_a_drawing_is_laid_out_and_says_so() {
    let d = parse(&plain(&data_model(
        "urn:x/layout/process1",
        &[("Plan", &[]), ("Build", &[]), ("Ship", &[])],
    )));
    let m = messages(&d);
    assert!(m.contains(&LAID_OUT), "{m:?}");
    assert!(!m.contains(&SKIPPED), "{m:?}");
    let texts: Vec<String> = labelled(&d).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, ["Plan", "Build", "Ship"]);
    // Three boxes and two arrows, all inside the frame (EMU).
    let all = shapes(&d);
    assert_eq!(all.len(), 5);
    let (x, y, w, h) = FRAME;
    for s in all {
        let r = s.rect;
        assert!(r.x >= x as f32 - 1.0 && r.y >= y as f32 - 1.0, "{r:?}");
        assert!(r.x + r.w <= (x + w) as f32 + 1.0 && r.y + r.h <= (y + h) as f32 + 1.0, "{r:?}");
    }
}

#[test]
fn a_saved_drawing_wins_over_the_layout_engine() {
    let drawing = r#"<?xml version="1.0"?><dsp:drawing xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dsp:spTree><dsp:sp><dsp:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1000000" cy="500000"/></a:xfrm><a:prstGeom prst="rect"/></dsp:spPr><dsp:txBody><a:bodyPr/><a:p><a:r><a:t>SAVED</a:t></a:r></a:p></dsp:txBody></dsp:sp></dsp:spTree></dsp:drawing>"#;
    let data = data_model("urn:x/layout/default", &[("computed", &[])]).replace(
        "</dgm:dataModel>",
        r#"<dgm:extLst><a:ext uri="http://schemas.microsoft.com/office/drawing/2008/diagram"><dsp:dataModelExt xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" relId="rId6" minVer="http://schemas.openxmlformats.org/drawingml/2006/diagram"/></a:ext></dgm:extLst></dgm:dataModel>"#,
    );
    let d = parse(&deck(&Parts {
        slide_body: frame_xml(),
        data: Some(&data),
        colors: None,
        drawing: Some(drawing),
    }));
    let texts: Vec<String> = labelled(&d).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, ["SAVED"]);
    assert!(
        !messages(&d).contains(&LAID_OUT) && !messages(&d).contains(&SKIPPED),
        "{:?}",
        messages(&d)
    );
}

#[test]
fn a_frame_with_no_data_part_is_still_reported_as_skipped() {
    let d = parse(&deck(&Parts {
        slide_body: frame_xml(),
        data: None,
        colors: None,
        drawing: None,
    }));
    assert!(messages(&d).contains(&SKIPPED), "{:?}", messages(&d));
    assert!(shapes(&d).is_empty());
}

#[test]
fn a_diagram_with_no_nodes_draws_nothing_and_loses_nothing() {
    let d = parse(&plain(&data_model("urn:x/layout/vList3", &[])));
    assert!(shapes(&d).is_empty());
    assert!(messages(&d).iter().all(|m| !m.contains("SmartArt")), "{:?}", messages(&d));
}

#[test]
fn nodes_take_the_accent_of_the_color_part_with_readable_text() {
    let colors = r#"<?xml version="1.0"?><dgm:colorsDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><dgm:styleLbl name="node1"><dgm:fillClrLst meth="repeat"><a:srgbClr val="C00000"/><a:srgbClr val="FFFF99"/></dgm:fillClrLst></dgm:styleLbl></dgm:colorsDef>"#;
    let d = parse(&deck(&Parts {
        slide_body: frame_xml(),
        data: Some(&data_model("urn:x/layout/default", &[("one", &[]), ("two", &[]), ("three", &[])])),
        colors: Some(colors),
        drawing: None,
    }));
    let boxes = labelled(&d);
    let fill = |s: &Shape| match &s.fill {
        Some(Fill::Solid(c)) => *c,
        other => panic!("fill {other:?}"),
    };
    let colour_of_text = |s: &Shape| s.text.as_ref().unwrap().paragraphs[0].runs[0].color;
    // The list repeats: red, pale yellow, red. White text on red, dark on yellow.
    assert_eq!(fill(boxes[0].1), Rgb(0xC0, 0, 0));
    assert_eq!(fill(boxes[1].1), Rgb(0xFF, 0xFF, 0x99));
    assert_eq!(fill(boxes[2].1), Rgb(0xC0, 0, 0));
    assert_eq!(colour_of_text(boxes[0].1), Rgb::WHITE);
    assert_ne!(colour_of_text(boxes[1].1), Rgb::WHITE);
}

#[test]
fn without_a_color_part_nodes_use_the_themes_first_accent() {
    let d = parse(&plain(&data_model("urn:x/layout/default", &[("one", &[])])));
    let boxes = labelled(&d);
    let accent = match &boxes[0].1.fill {
        Some(Fill::Solid(c)) => *c,
        other => panic!("fill {other:?}"),
    };
    // Whatever the theme's accent1 is, the text on it is white and it is not black.
    assert_ne!(accent, Rgb::BLACK);
    assert_eq!(boxes[0].1.text.as_ref().unwrap().paragraphs[0].runs[0].color, Rgb::WHITE);
}

#[test]
fn text_is_fitted_and_shared_so_longer_text_shrinks_every_node() {
    // Four boxes: PowerPoint gives them all one size, set by the longest text.
    let size = |first: &str| {
        let nodes = [(first, &[][..]), ("Two", &[][..]), ("Three", &[][..]), ("Four", &[][..])];
        let d = parse(&plain(&data_model("urn:x/layout/default", &nodes)));
        let t = labelled(&d).remove(1).1.text.clone().unwrap();
        t.paragraphs[0].runs[0].size * t.font_scale
    };
    let (short, long) = (
        size("One"),
        size("A considerably longer sentence that must wrap and shrink to stay inside its box"),
    );
    assert!(short > long * 1.5, "short {short}pt, long {long}pt");
    assert!(long >= 5.0 && short <= 65.0, "{long} {short}");
}

#[test]
fn every_family_is_laid_out_with_its_text() {
    let nodes: &[(&str, &[&str])] = &[("Alpha", &["a1", "a2"]), ("Beta", &["b1"]), ("Gamma", &[])];
    for layout in [
        "default",
        "vList2",
        "hList1",
        "process1",
        "vProcess5",
        "chevron1",
        "cycle2",
        "radial3",
        "orgChart1",
        "matrix1",
        "pyramid1",
        "unknownLayout",
    ] {
        let d = parse(&plain(&data_model(
            &format!("urn:microsoft.com/office/officeart/2005/8/layout/{layout}"),
            nodes,
        )));
        assert!(messages(&d).contains(&LAID_OUT), "{layout}: {:?}", messages(&d));
        let all = labelled(&d).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join(" ");
        for word in ["Alpha", "Beta", "Gamma"] {
            assert!(all.contains(word), "{layout} lost '{word}': {all}");
        }
    }
}

#[test]
fn a_hostile_model_is_cut_and_reported() {
    // 2000 siblings: only the first MAX_NODES are laid out.
    let pts: String = (0..2000).map(|i| node_pt(i, "x")).collect();
    let cxns: String = (0..2000)
        .map(|i| format!(r#"<dgm:cxn srcId="doc" destId="n{i}" srcOrd="{i}"/>"#))
        .collect();
    let doc = r#"<dgm:pt modelId="doc" type="doc"/>"#;
    let d = parse(&plain(&wrap_model(&format!("{doc}{pts}"), &cxns)));
    assert!(shapes(&d).len() < 1_000, "{} shapes", shapes(&d).len());
    assert!(
        messages(&d).iter().any(|m| m.contains("the rest was left out")),
        "{:?}",
        messages(&d)
    );

    // A connection cycle terminates.
    let cyc = r#"<dgm:pt modelId="doc" type="doc"/><dgm:pt modelId="p"/><dgm:pt modelId="q"/>"#;
    let cx = r#"<dgm:cxn srcId="doc" destId="p" srcOrd="0"/><dgm:cxn srcId="p" destId="q" srcOrd="0"/><dgm:cxn srcId="q" destId="p" srcOrd="0"/>"#;
    let d = parse(&plain(&wrap_model(cyc, cx)));
    assert!(messages(&d).contains(&LAID_OUT));
}

#[test]
fn a_laid_out_diagram_turns_with_its_rotated_group() {
    let data = data_model("urn:x/layout/process1", &[("a", &[]), ("b", &[]), ("c", &[])]);
    let still = parse(&plain(&data));
    let turned = parse(&deck(&Parts {
        slide_body: turned_group(5_400_000),
        data: Some(&data),
        colors: None,
        drawing: None,
    }));
    let (before, after) = (shapes(&still), shapes(&turned));
    assert_eq!(before.len(), after.len());
    assert!(!before.is_empty());
    // A quarter turn clockwise about the frame's centre.
    let (x, y, w, h) = FRAME;
    let (px, py) = ((x + w / 2) as f32, (y + h / 2) as f32);
    let centre = |s: &Shape| (s.rect.x + s.rect.w / 2.0, s.rect.y + s.rect.h / 2.0);
    for (b, a) in before.iter().zip(&after) {
        let (bx, by) = centre(b);
        let want = (px - (by - py), py + (bx - px));
        let got = centre(a);
        assert!((got.0 - want.0).abs() < 2.0 && (got.1 - want.1).abs() < 2.0, "{got:?} != {want:?}");
        assert!(
            ((a.rotation - b.rotation).rem_euclid(360.0) - 90.0).abs() < 0.01,
            "{} vs {}",
            a.rotation,
            b.rotation
        );
        // The text box moved with its shape.
        if let (Some(tb), Some(ta)) = (b.text_rect, a.text_rect) {
            let (tbc, tac) = ((tb.x + tb.w / 2.0, tb.y + tb.h / 2.0), (ta.x + ta.w / 2.0, ta.y + ta.h / 2.0));
            let (dx, dy) = (tbc.0 - bx, tbc.1 - by);
            // The offset from the shape's centre is turned by the shape's own rotation at draw
            // time, so the stored box keeps the same offset.
            assert!(((tac.0 - got.0) - dx).abs() < 2.0 && ((tac.1 - got.1) - dy).abs() < 2.0);
        }
    }
}

#[test]
fn the_whole_deck_converts_and_the_report_tells_the_truth() {
    let data = data_model("urn:x/layout/cycle2", &[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[])]);
    let opts = slidepdf::Options::preset(slidepdf::Preset::Light);
    let out = slidepdf::convert(&plain(&data), &opts).unwrap();
    assert!(out.pdf.starts_with(b"%PDF-"));
    let report = out.report.to_text();
    assert!(report.contains(LAID_OUT), "{report}");
    assert!(!report.contains(SKIPPED), "{report}");
}
