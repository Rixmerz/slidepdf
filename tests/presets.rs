//! Every preset shape of the DrawingML catalogue is drawn, not approximated
//! by a rectangle with a warning, and its adjust values are honoured.

use slidepdf::ir::{Element, Geometry};
use std::io::{Cursor, Read, Write};

const DECK: &[u8] = include_bytes!("fixtures/deck.pptx");

/// The names of ECMA-376 `presetShapeDefinitions.xml`.
const CATALOGUE: &[&str] = &[
    "accentBorderCallout1",
    "accentBorderCallout2",
    "accentBorderCallout3",
    "accentCallout1",
    "accentCallout2",
    "accentCallout3",
    "actionButtonBackPrevious",
    "actionButtonBeginning",
    "actionButtonBlank",
    "actionButtonDocument",
    "actionButtonEnd",
    "actionButtonForwardNext",
    "actionButtonHelp",
    "actionButtonHome",
    "actionButtonInformation",
    "actionButtonMovie",
    "actionButtonReturn",
    "actionButtonSound",
    "arc",
    "bentArrow",
    "bentConnector2",
    "bentConnector3",
    "bentConnector4",
    "bentConnector5",
    "bentUpArrow",
    "bevel",
    "blockArc",
    "borderCallout1",
    "borderCallout2",
    "borderCallout3",
    "bracePair",
    "bracketPair",
    "callout1",
    "callout2",
    "callout3",
    "can",
    "chartPlus",
    "chartStar",
    "chartX",
    "chevron",
    "chord",
    "circularArrow",
    "cloud",
    "cloudCallout",
    "corner",
    "cornerTabs",
    "cube",
    "curvedConnector2",
    "curvedConnector3",
    "curvedConnector4",
    "curvedConnector5",
    "curvedDownArrow",
    "curvedLeftArrow",
    "curvedRightArrow",
    "curvedUpArrow",
    "decagon",
    "diagStripe",
    "diamond",
    "dodecagon",
    "donut",
    "doubleWave",
    "downArrow",
    "downArrowCallout",
    "ellipse",
    "ellipseRibbon",
    "ellipseRibbon2",
    "flowChartAlternateProcess",
    "flowChartCollate",
    "flowChartConnector",
    "flowChartDecision",
    "flowChartDelay",
    "flowChartDisplay",
    "flowChartDocument",
    "flowChartExtract",
    "flowChartInputOutput",
    "flowChartInternalStorage",
    "flowChartMagneticDisk",
    "flowChartMagneticDrum",
    "flowChartMagneticTape",
    "flowChartManualInput",
    "flowChartManualOperation",
    "flowChartMerge",
    "flowChartMultidocument",
    "flowChartOfflineStorage",
    "flowChartOffpageConnector",
    "flowChartOnlineStorage",
    "flowChartOr",
    "flowChartPredefinedProcess",
    "flowChartPreparation",
    "flowChartProcess",
    "flowChartPunchedCard",
    "flowChartPunchedTape",
    "flowChartSort",
    "flowChartSummingJunction",
    "flowChartTerminator",
    "foldedCorner",
    "frame",
    "funnel",
    "gear6",
    "gear9",
    "halfFrame",
    "heart",
    "heptagon",
    "hexagon",
    "homePlate",
    "horizontalScroll",
    "irregularSeal1",
    "irregularSeal2",
    "leftArrow",
    "leftArrowCallout",
    "leftBrace",
    "leftBracket",
    "leftCircularArrow",
    "leftRightArrow",
    "leftRightArrowCallout",
    "leftRightCircularArrow",
    "leftRightRibbon",
    "leftRightUpArrow",
    "leftUpArrow",
    "lightningBolt",
    "line",
    "lineInv",
    "mathDivide",
    "mathEqual",
    "mathMinus",
    "mathMultiply",
    "mathNotEqual",
    "mathPlus",
    "moon",
    "noSmoking",
    "nonIsoscelesTrapezoid",
    "notchedRightArrow",
    "octagon",
    "parallelogram",
    "pentagon",
    "pie",
    "pieWedge",
    "plaque",
    "plaqueTabs",
    "plus",
    "quadArrow",
    "quadArrowCallout",
    "rect",
    "ribbon",
    "ribbon2",
    "rightArrow",
    "rightArrowCallout",
    "rightBrace",
    "rightBracket",
    "round1Rect",
    "round2DiagRect",
    "round2SameRect",
    "roundRect",
    "rtTriangle",
    "smileyFace",
    "snip1Rect",
    "snip2DiagRect",
    "snip2SameRect",
    "snipRoundRect",
    "squareTabs",
    "star10",
    "star12",
    "star16",
    "star24",
    "star32",
    "star4",
    "star5",
    "star6",
    "star7",
    "star8",
    "straightConnector1",
    "stripedRightArrow",
    "sun",
    "swooshArrow",
    "teardrop",
    "trapezoid",
    "triangle",
    "upArrow",
    "upArrowCallout",
    "upDownArrow",
    "upDownArrowCallout",
    "uturnArrow",
    "verticalScroll",
    "wave",
    "wedgeEllipseCallout",
    "wedgeRectCallout",
    "wedgeRoundRectCallout",
];

/// Presets that really are a plain rectangle (the others fall back to one
/// only when the name is unknown).
const PLAIN_RECTANGLES: &[&str] = &["actionButtonBlank", "flowChartProcess", "rect"];

fn shape(id: usize, prst: &str, adjust: &[(&str, i64)]) -> String {
    let gds: String = adjust
        .iter()
        .map(|(n, v)| format!(r#"<a:gd name="{n}" fmla="val {v}"/>"#))
        .collect();
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="s{id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="100000" y="100000"/><a:ext cx="3000000" cy="1500000"/></a:xfrm><a:prstGeom prst="{prst}"><a:avLst>{gds}</a:avLst></a:prstGeom></p:spPr></p:sp>"#
    )
}

/// The fixture deck with its first slide replaced by `shapes`.
fn deck_with(shapes: &str) -> Vec<u8> {
    let slide = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    let mut zin = zip::ZipArchive::new(Cursor::new(DECK)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        if name == "ppt/slides/slide1.xml" {
            data = slide.clone().into_bytes();
        }
        out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(&data).unwrap();
    }
    out.finish().unwrap().into_inner()
}

/// The geometries of the shapes on the first slide, in order.
fn geometries(shapes: &str) -> (Vec<Geometry>, Vec<String>) {
    let deck = slidepdf::parse(&deck_with(shapes), true).unwrap();
    let geoms = deck.slides[0]
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Shape(s) => Some(s.geometry.clone()),
            Element::Picture(_) => None,
        })
        .collect();
    (geoms, deck.warnings.into_iter().map(|w| w.message).collect())
}

#[test]
fn no_preset_of_the_catalogue_is_drawn_as_a_rectangle_with_a_warning() {
    let shapes: String = CATALOGUE.iter().enumerate().map(|(i, p)| shape(10 + i, p, &[])).collect();
    let (geoms, warnings) = geometries(&shapes);
    let fallbacks: Vec<&String> = warnings.iter().filter(|w| w.contains("drawn as a rectangle")).collect();
    assert!(fallbacks.is_empty(), "still drawn as a rectangle: {fallbacks:?}");
    assert_eq!(geoms.len(), CATALOGUE.len());
    let boxes: Vec<&str> = CATALOGUE
        .iter()
        .zip(&geoms)
        .filter(|(_, g)| **g == Geometry::Rect)
        .map(|(p, _)| *p)
        .collect();
    assert_eq!(boxes, PLAIN_RECTANGLES);
}

#[test]
fn angle_adjusts_above_ten_million_are_honoured() {
    // 16200000 (270 degrees) is a legal angle; it must not be mistaken for garbage.
    let shapes = [
        shape(10, "arc", &[]),
        shape(11, "arc", &[("adj1", 16_200_000), ("adj2", 5_400_000)]),
        shape(12, "arc", &[("adj1", 10_800_000), ("adj2", 16_200_000)]),
        shape(13, "blockArc", &[("adj1", 16_200_000), ("adj2", 5_400_000), ("adj3", 10_000)]),
    ]
    .concat();
    let (g, _) = geometries(&shapes);
    assert_ne!(g[0], g[1]);
    assert_ne!(g[0], g[2]);
    assert_ne!(g[1], g[2]);
    assert!(matches!(g[3], Geometry::Path(_)));
}

#[test]
fn adjust_values_reshape_the_outline() {
    for (prst, adjust) in [
        ("snip1Rect", ("adj", 40_000)),
        ("bentUpArrow", ("adj2", 10_000)),
        ("downArrowCallout", ("adj4", 30_000)),
        ("gear6", ("adj1", 5_000)),
        ("mathPlus", ("adj1", 10_000)),
        ("cloudCallout", ("adj1", 60_000)),
    ] {
        let shapes = [shape(10, prst, &[]), shape(11, prst, &[adjust])].concat();
        let (g, warnings) = geometries(&shapes);
        assert!(warnings.is_empty(), "{prst}: {warnings:?}");
        assert_ne!(g[0], g[1], "{prst} ignores {}", adjust.0);
    }
}

#[test]
fn degenerate_boxes_and_extreme_adjusts_stay_finite() {
    let mut shapes = String::new();
    for (i, p) in CATALOGUE.iter().enumerate() {
        let extreme: Vec<(&str, i64)> = ["adj", "adj1", "adj2", "adj3", "adj4", "adj5", "adj6", "adj7", "adj8"]
            .into_iter()
            .map(|n| (n, -2_000_000_000))
            .collect();
        shapes.push_str(&shape(10 + 2 * i, p, &extreme));
        let huge: Vec<(&str, i64)> = extreme.iter().map(|(n, _)| (*n, 2_000_000_000)).collect();
        shapes.push_str(&shape(11 + 2 * i, p, &huge));
    }
    let (geoms, _) = geometries(&shapes);
    for g in geoms {
        if let Geometry::Path(c) | Geometry::Open(c) | Geometry::PathEvenOdd(c) = g {
            let finite = |v: f32| v.is_finite();
            assert!(c.iter().all(|k| match *k {
                slidepdf::ir::PathCmd::Move(x, y) | slidepdf::ir::PathCmd::Line(x, y) => finite(x) && finite(y),
                slidepdf::ir::PathCmd::Cubic(a, b, c, d, e, f) => [a, b, c, d, e, f].into_iter().all(finite),
                slidepdf::ir::PathCmd::Close => true,
            }));
        }
    }
}
