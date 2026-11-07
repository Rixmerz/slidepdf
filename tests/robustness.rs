//! Hostile and broken inputs must fail cleanly: an `Err`, never a panic
//! (a panic aborts the whole process in WebAssembly and in `panic = "abort"`
//! builds), and never unbounded memory.

use std::io::Write;

const DECK: &[u8] = include_bytes!("fixtures/deck.pptx");

fn opts() -> slidepdf::Options {
    slidepdf::Options::preset(slidepdf::Preset::Light)
}

/// Small deterministic PRNG so failures are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn fixture_converts() {
    let out = slidepdf::convert(DECK, &opts()).unwrap();
    assert_eq!(out.fragments.len(), 4);
    assert!(out.pdf.starts_with(b"%PDF-1.7"));
    assert!(out.pdf.len() < 20_000, "full PDF grew to {} bytes", out.pdf.len());
}

#[test]
fn random_garbage_is_rejected() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for len in [0usize, 1, 4, 100, 5000] {
        let data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        assert!(slidepdf::convert(&data, &opts()).is_err());
    }
}

#[test]
fn truncated_decks_do_not_panic() {
    for cut in (0..DECK.len()).step_by(997) {
        let _ = slidepdf::convert(&DECK[..cut], &opts());
    }
}

#[test]
fn corrupted_decks_do_not_panic() {
    let mut rng = Rng(42);
    for _ in 0..400 {
        let mut d = DECK.to_vec();
        for _ in 0..(1 + rng.next() % 8) {
            let i = (rng.next() as usize) % d.len();
            d[i] = rng.next() as u8;
        }
        let _ = slidepdf::convert(&d, &opts());
    }
}

/// Rewrites every XML part with random byte flips inside it (the zip stays
/// valid, so the XML and image parsers get the hostile input).
#[test]
fn corrupted_parts_do_not_panic() {
    let mut rng = Rng(7);
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(DECK)).unwrap();
    let parts: Vec<(String, Vec<u8>)> = (0..src.len())
        .map(|i| {
            let mut f = src.by_index(i).unwrap();
            let mut v = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut v).unwrap();
            (f.name().to_string(), v)
        })
        .collect();
    for _ in 0..150 {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            for (name, data) in &parts {
                let mut data = data.clone();
                if !data.is_empty() && rng.next().is_multiple_of(3) {
                    for _ in 0..(1 + rng.next() % 4) {
                        let i = (rng.next() as usize) % data.len();
                        data[i] = rng.next() as u8;
                    }
                }
                z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(&data).unwrap();
            }
            z.finish().unwrap();
        }
        let _ = slidepdf::convert(buf.get_ref(), &opts());
    }
}

/// A part that inflates to 200 MB from ~200 KB must be refused, not read.
#[test]
fn zip_bomb_is_bounded() {
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(DECK)).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut v = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut v).unwrap();
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
            if name == "ppt/slides/slide1.xml" {
                let zeros = vec![b' '; 1 << 20];
                for _ in 0..200 {
                    z.write_all(&zeros).unwrap();
                }
            } else {
                z.write_all(&v).unwrap();
            }
        }
        z.finish().unwrap();
    }
    assert!(buf.get_ref().len() < 2 << 20);
    // Slide 1 is skipped with a warning; the rest still converts.
    let out = slidepdf::convert(buf.get_ref(), &opts()).unwrap();
    assert_eq!(out.fragments.len(), 3);
    assert!(!out.report.general.is_empty());
}

#[test]
fn deeply_nested_xml_is_refused() {
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(DECK)).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut v = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut v).unwrap();
            if name == "ppt/slides/slide2.xml" {
                v = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000)).into_bytes();
            }
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(&v).unwrap();
        }
        z.finish().unwrap();
    }
    let out = slidepdf::convert(buf.get_ref(), &opts()).unwrap();
    assert_eq!(out.fragments.len(), 3);
}

#[cfg(feature = "assemble")]
#[test]
fn assemble_round_trip_and_garbage() {
    let out = slidepdf::convert(DECK, &opts()).unwrap();
    let parts: Vec<&[u8]> = out.fragments.iter().map(|f| f.pdf.as_slice()).collect();
    let merged = slidepdf::assemble(&parts).unwrap();
    assert_eq!(slidepdf::split(&merged).unwrap().len(), 4);
    assert!(
        merged.len() < parts.iter().map(|p| p.len()).sum::<usize>(),
        "dedupe did not shrink the merge"
    );
    assert!(slidepdf::assemble(&[b"not a pdf".as_slice()]).is_err());
    let mut rng = Rng(99);
    for _ in 0..100 {
        let mut d = out.fragments[2].pdf.clone();
        let i = (rng.next() as usize) % d.len();
        d[i] = rng.next() as u8;
        let _ = slidepdf::assemble(&[d.as_slice()]);
        let _ = slidepdf::split(&d);
    }
}

/// Rewrites the fixture, replacing or adding parts.
fn patched(edit: impl Fn(&str, Vec<u8>) -> Vec<u8>, extra: &[(&str, &str)]) -> Vec<u8> {
    let mut src = zip::ZipArchive::new(std::io::Cursor::new(DECK)).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut v = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut v).unwrap();
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(&edit(&name, v)).unwrap();
        }
        for (name, data) in extra {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn insert_before(xml: Vec<u8>, marker: &str, add: &str) -> Vec<u8> {
    let s = String::from_utf8(xml).unwrap();
    let i = s.find(marker).expect("marker");
    format!("{}{}{}", &s[..i], add, &s[i..]).into_bytes()
}

const FRAME: &str = r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="90" name="d"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"/></a:graphicData></a:graphic></p:graphicFrame>"#;

/// A SmartArt drawing that contains a SmartArt frame resolving to itself
/// must not recurse (a stack overflow aborts the process).
#[test]
fn self_referencing_smartart_terminates() {
    let drawing = format!(
        r#"<dsp:drawing xmlns:dsp="http://schemas.microsoft.com/office/drawing/2008/diagram" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><dsp:spTree>{FRAME}</dsp:spTree></dsp:drawing>"#
    );
    let self_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="drawing1.xml"/></Relationships>"#;
    let deck = patched(
        |name, v| match name {
            "ppt/slides/slide1.xml" => insert_before(v, "</p:spTree>", FRAME),
            "ppt/slides/_rels/slide1.xml.rels" => insert_before(
                v,
                "</Relationships>",
                r#"<Relationship Id="rId99" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="../diagrams/drawing1.xml"/>"#,
            ),
            _ => v,
        },
        &[
            ("ppt/diagrams/drawing1.xml", &drawing),
            ("ppt/diagrams/_rels/drawing1.xml.rels", self_rels),
        ],
    );
    let out = slidepdf::convert(&deck, &opts()).unwrap();
    assert_eq!(out.fragments.len(), 4);
}

/// Hostile numbers: a huge numbering start and non-finite shape adjusts.
#[test]
fn hostile_numbers_render() {
    let deck = patched(
        |name, v| match name {
            "ppt/slides/slide2.xml" => {
                let s = String::from_utf8(v).unwrap().replacen(
                    "<a:pPr/>",
                    r#"<a:pPr><a:buAutoNum type="romanUcPeriod" startAt="4294967295"/></a:pPr>"#,
                    1,
                );
                insert_before(
                    s.into_bytes(),
                    "</p:spTree>",
                    r#"<p:sp><p:nvSpPr><p:cNvPr id="77" name="n"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="val NaN"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></p:spPr></p:sp>"#,
                )
            }
            _ => v,
        },
        &[],
    );
    let out = slidepdf::convert(&deck, &opts()).unwrap();
    let page = &out.fragments[1].pdf;
    let text = String::from_utf8_lossy(page);
    assert!(
        !text.contains("NaN") && !text.contains("inf"),
        "non-finite number written to the PDF"
    );
}
