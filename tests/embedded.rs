//! An embedded OLE object with no preview picture is drawn as the best thing
//! we can read: its own picture, the chart of its workbook, or a labelled
//! placeholder box. Everything runs through the same entry point a deck takes.

use slidepdf::ir::{Deck, Element, Fill, Picture, Shape};
use std::io::{Cursor, Read, Write};

const DECK: &[u8] = include_bytes!("fixtures/deck.pptx");
const FRAME: (i64, i64, i64, i64) = (1_000_000, 1_000_000, 6_000_000, 4_000_000);

/// An `oleObj` frame in the slide's tree. `attrs` are the object's own
/// attributes (`name`, `progId`); `preview` is a `p:pic` inside it, if any.
fn frame_xml(attrs: &str, preview: &str) -> String {
    let (x, y, w, h) = FRAME;
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="Object 3"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/presentationml/2006/ole"><p:oleObj r:id="rId2" {attrs}><p:embed/>{preview}</p:oleObj></a:graphicData></a:graphic></p:graphicFrame>"#
    )
}

/// The frame inside a group turned 90 degrees about its own centre.
fn turned(frame: &str) -> String {
    let (x, y, w, h) = FRAME;
    format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="9" name="g"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm rot="5400000"><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/><a:chOff x="{x}" y="{y}"/><a:chExt cx="{w}" cy="{h}"/></a:xfrm></p:grpSpPr>{frame}</p:grpSp>"#
    )
}

fn preview_pic(rid: &str) -> String {
    format!(
        r#"<p:pic><p:nvPicPr><p:cNvPr id="0" name="preview"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/></p:blipFill><p:spPr/></p:pic>"#
    )
}

/// The fixture deck with its first slide replaced by one holding `body`, the
/// embedded part `embedding` (name inside `ppt/embeddings`, bytes) that
/// `rId2` points at, and a picture part `rId3` for a preview.
fn deck(body: &str, embedding: Option<(&str, &[u8])>, preview: Option<&[u8]>) -> Vec<u8> {
    let slide = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{body}</p:spTree></p:cSld></p:sld>"#
    );
    let rel = |id: &str, kind: &str, target: &str| {
        format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
        )
    };
    let mut rels = rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml");
    if let Some((name, _)) = embedding {
        rels += &rel("rId2", "oleObject", &format!("../embeddings/{name}"));
    }
    if preview.is_some() {
        rels += &rel("rId3", "image", "../media/preview.png");
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
    let mut parts: Vec<(String, &[u8])> = Vec::new();
    if let Some((name, bytes)) = embedding {
        parts.push((format!("ppt/embeddings/{name}"), bytes));
    }
    if let Some(bytes) = preview {
        parts.push(("ppt/media/preview.png".to_string(), bytes));
    }
    for (name, bytes) in parts {
        out.start_file(name, opts).unwrap();
        out.write_all(bytes).unwrap();
    }
    out.finish().unwrap().into_inner()
}

fn png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 2, 2);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&[200; 12]).unwrap();
    out
}

/// A minimal workbook package: one chart part with a cached bar series, the
/// only thing of it that is read.
fn workbook(chart: Option<&str>) -> Vec<u8> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    out.start_file("xl/workbook.xml", opts).unwrap();
    out.write_all(b"<workbook/>").unwrap();
    if let Some(chart) = chart {
        out.start_file("xl/charts/chart1.xml", opts).unwrap();
        out.write_all(chart.as_bytes()).unwrap();
    }
    out.finish().unwrap().into_inner()
}

const BARS: &str = r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:ser><c:cat><c:strRef><c:f>S!A1:A3</c:f><c:strCache><c:ptCount val="3"/><c:pt idx="0"><c:v>North</c:v></c:pt><c:pt idx="1"><c:v>South</c:v></c:pt><c:pt idx="2"><c:v>West</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>S!B1:B3</c:f><c:numCache><c:ptCount val="3"/><c:pt idx="0"><c:v>3</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt><c:pt idx="2"><c:v>8</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;

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

fn pictures(deck: &Deck) -> Vec<&Picture> {
    deck.slides[0]
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Picture(p) => Some(p),
            Element::Shape(_) => None,
        })
        .collect()
}

fn frame_rect() -> [f32; 4] {
    [FRAME.0 as f32, FRAME.1 as f32, FRAME.2 as f32, FRAME.3 as f32]
}

fn rect_of(s: &Shape) -> [f32; 4] {
    [s.rect.x, s.rect.y, s.rect.w, s.rect.h]
}

fn label_of(s: &Shape) -> String {
    let body = s.text.as_ref().expect("the placeholder carries its label");
    body.paragraphs[0].runs.iter().map(|r| r.text.as_str()).collect()
}

#[test]
fn an_object_with_nothing_readable_becomes_a_labelled_placeholder() {
    let bytes = deck(
        &frame_xml(r#"name="Budget" progId="Excel.Sheet.8""#, ""),
        Some(("oleObject1.bin", &[0xD0, 0xCF, 0x11, 0xE0, 1, 2, 3])),
        None,
    );
    let d = parse(&bytes);
    assert_eq!(messages(&d), ["embedded Excel.Sheet.8 drawn as a placeholder"]);
    let boxes = shapes(&d);
    assert_eq!(boxes.len(), 1);
    assert!(pictures(&d).is_empty());
    let b = boxes[0];
    assert_eq!(rect_of(b), frame_rect());
    assert_eq!(label_of(b), "Budget");
    assert!(b.stroke.is_some(), "the box has a thin border");
    assert!(matches!(b.fill, Some(Fill::Translucent(..))), "light grey, not opaque");
    // Centred, in the middle of the box.
    let body = b.text.as_ref().unwrap();
    assert_eq!(body.anchor, slidepdf::ir::Anchor::Middle);
    assert_eq!(body.paragraphs[0].align, slidepdf::ir::Align::Center);
}

#[test]
fn the_label_falls_back_from_name_to_prog_id_to_a_generic_word() {
    for (attrs, warning, label) in [
        (
            r#"name="" progId="Word.Document.12""#,
            "embedded Word.Document.12 drawn as a placeholder",
            "Word.Document.12",
        ),
        (r#"name="Photo" progId="""#, "embedded Photo drawn as a placeholder", "Photo"),
        ("", "embedded object drawn as a placeholder", "Embedded object"),
    ] {
        let bytes = deck(&frame_xml(attrs, ""), Some(("oleObject1.bin", b"x")), None);
        let d = parse(&bytes);
        assert_eq!(messages(&d), [warning]);
        assert_eq!(label_of(shapes(&d)[0]), label);
    }
}

#[test]
fn an_object_whose_relationship_is_missing_still_keeps_its_place() {
    let d = parse(&deck(&frame_xml(r#"progId="Package""#, ""), None, None));
    assert_eq!(messages(&d), ["embedded Package drawn as a placeholder"]);
    assert_eq!(rect_of(shapes(&d)[0]), frame_rect());
}

#[test]
fn the_placeholder_turns_with_its_group() {
    let bytes = deck(&turned(&frame_xml(r#"name="Budget""#, "")), Some(("oleObject1.bin", b"x")), None);
    let d = parse(&bytes);
    let b = shapes(&d)[0].clone();
    assert!((b.rotation - 90.0).abs() < 0.01, "rotation {}", b.rotation);
}

#[test]
fn an_embedded_picture_is_drawn_in_the_frame() {
    let image = png();
    let bytes = deck(&frame_xml(r#"progId="Package""#, ""), Some(("image1.png", &image)), None);
    let d = parse(&bytes);
    assert!(messages(&d).is_empty(), "{:?}", messages(&d));
    assert!(shapes(&d).is_empty());
    let pics = pictures(&d);
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].media, "ppt/embeddings/image1.png");
    let r = pics[0].rect;
    assert_eq!([r.x, r.y, r.w, r.h], frame_rect());
    assert!(d.media.contains("ppt/embeddings/image1.png"));
    // The picture survives the whole conversion, not only parsing.
    let pdf = slidepdf::convert(&bytes, &slidepdf::Options::preset(slidepdf::Preset::Light)).unwrap();
    assert!(pdf.pdf.starts_with(b"%PDF"));
}

#[test]
fn an_embedded_picture_turns_with_its_group() {
    let image = png();
    let bytes = deck(&turned(&frame_xml("", "")), Some(("image1.png", &image)), None);
    let d = parse(&bytes);
    assert!((pictures(&d)[0].rotation - 90.0).abs() < 0.01);
}

#[test]
fn a_preview_picture_still_wins_over_the_embedding() {
    let image = png();
    let bytes = deck(
        &frame_xml(r#"progId="Excel.Sheet.12""#, &preview_pic("rId3")),
        Some(("workbook1.xlsx", &workbook(Some(BARS)))),
        Some(&image),
    );
    let d = parse(&bytes);
    assert!(messages(&d).is_empty(), "{:?}", messages(&d));
    let pics = pictures(&d);
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].media, "ppt/media/preview.png");
    assert!(shapes(&d).is_empty());
}

#[test]
fn the_chart_of_an_embedded_workbook_is_redrawn() {
    let bytes = deck(
        &frame_xml(r#"name="Chart 1" progId="Excel.Sheet.12""#, ""),
        Some(("workbook1.xlsx", &workbook(Some(BARS)))),
        None,
    );
    let d = parse(&bytes);
    assert!(messages(&d).is_empty(), "{:?}", messages(&d));
    let bars = shapes(&d)
        .iter()
        .filter(|s| matches!(s.fill, Some(Fill::Solid(_))) && s.text.is_none())
        .count();
    assert!(bars >= 3, "{bars} filled shapes for three bars");
    let [x, y, w, h] = frame_rect();
    for s in shapes(&d) {
        let r = s.rect;
        assert!(
            r.x >= x - 1.0 && r.y >= y - 1.0 && r.x + r.w <= x + w + 1.0 && r.y + r.h <= y + h + 1.0,
            "{r:?} leaves the frame"
        );
    }
}

#[test]
fn the_chart_of_an_embedded_workbook_turns_with_its_group() {
    let object = frame_xml(r#"progId="Excel.Sheet.12""#, "");
    let plain = parse(&deck(&object, Some(("workbook1.xlsx", &workbook(Some(BARS)))), None));
    let turned = parse(&deck(&turned(&object), Some(("workbook1.xlsx", &workbook(Some(BARS)))), None));
    let (flat, quarter) = (shapes(&plain), shapes(&turned));
    assert_eq!(flat.len(), quarter.len());
    assert!(flat.iter().all(|s| s.rotation == 0.0));
    // Every piece is carried to a different place; the group is not ignored.
    let moved = flat
        .iter()
        .zip(&quarter)
        .filter(|(a, b)| rect_of(a) != rect_of(b) || a.rotation != b.rotation)
        .count();
    assert!(moved > flat.len() / 2, "{moved} of {} pieces turned", flat.len());
}

#[test]
fn a_workbook_without_a_chart_gets_the_placeholder() {
    let bytes = deck(
        &frame_xml(r#"name="Sheet" progId="Excel.Sheet.12""#, ""),
        Some(("workbook1.xlsx", &workbook(None))),
        None,
    );
    let d = parse(&bytes);
    assert_eq!(messages(&d), ["embedded Excel.Sheet.12 drawn as a placeholder"]);
}

#[test]
fn a_workbook_is_only_opened_for_an_excel_prog_id() {
    let bytes = deck(
        &frame_xml(r#"name="Doc" progId="Word.Document.12""#, ""),
        Some(("workbook1.xlsx", &workbook(Some(BARS)))),
        None,
    );
    let d = parse(&bytes);
    assert_eq!(messages(&d), ["embedded Word.Document.12 drawn as a placeholder"]);
}
