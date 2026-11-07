//! # slidepdf
//!
//! Ultra-light PPTX -> PDF conversion with per-slide fragments.
//!
//! ```no_run
//! let pptx = std::fs::read("deck.pptx").unwrap();
//! let out = slidepdf::convert(&pptx, &slidepdf::Options::preset(slidepdf::Preset::Balanced)).unwrap();
//! std::fs::write("deck.pdf", &out.pdf).unwrap();
//! for f in &out.fragments {
//!     std::fs::write(&f.file, &f.pdf).unwrap();
//! }
//! std::fs::write("manifest.json", out.manifest.to_json()).unwrap();
//! println!("{}", out.report.to_text());
//! ```
//!
//! The core does no I/O: bytes in, bytes out. That keeps the WASM and
//! native wrappers thin.

pub mod budget;
mod chart;
pub mod fonts;
pub mod fragments;
pub mod images;
pub mod ir;
mod metafile;
mod metrics;
pub mod pdf;
pub mod pptx;
mod xml;

pub use fonts::FontFile;
pub use fragments::{Entry, Fragment, Manifest};

pub use ir::{Deck, Warning};
pub use pptx::Inspection;
use std::io::{Cursor, Read, Seek, Write};

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

/// Ready-made trade-offs between size and quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Smallest files: images at 110 DPI, JPEG quality 65, base-14 fonts.
    Light,
    /// Default: images at 150 DPI, JPEG quality 75.
    Balanced,
    /// For printing: images at 300 DPI, quality 88, embeds registered fonts.
    Print,
}

impl std::str::FromStr for Preset {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "light" => Ok(Preset::Light),
            "balanced" => Ok(Preset::Balanced),
            "print" => Ok(Preset::Print),
            o => Err(Error(format!("unknown preset '{o}' (expected light, balanced or print)"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub preset: Preset,
    /// Resolution images are kept at, relative to their displayed size.
    pub image_dpi: Option<f32>,
    pub jpeg_quality: u8,
    pub include_hidden: bool,
    /// Fonts to embed as subsets. See [`FontFile`].
    pub fonts: Vec<FontFile>,
    /// Produce the full PDF (otherwise only fragments).
    pub full_pdf: bool,
    /// Produce one PDF per slide.
    pub fragments: bool,
    /// Name recorded in the manifest.
    pub source_name: String,
    /// Memory for prepared images reused across slides. Lower it for tight
    /// containers; images are then re-prepared when needed again.
    pub cache_bytes: usize,
}

impl Options {
    pub fn preset(p: Preset) -> Self {
        let (dpi, q) = match p {
            Preset::Light => (110.0, 65),
            Preset::Balanced => (150.0, 75),
            Preset::Print => (300.0, 88),
        };
        Options {
            preset: p,
            image_dpi: Some(dpi),
            jpeg_quality: q,
            include_hidden: false,
            fonts: Vec::new(),
            full_pdf: true,
            fragments: true,
            source_name: "deck.pptx".into(),
            cache_bytes: 16 << 20,
        }
    }

    /// Registers a TTF/OTF font for embedding.
    pub fn add_font(&mut self, data: Vec<u8>) -> Result<&mut Self, Error> {
        self.fonts.push(FontFile::parse(data)?);
        Ok(self)
    }
}

impl Default for Options {
    fn default() -> Self {
        Options::preset(Preset::Balanced)
    }
}

/// What was lost in the conversion, per slide, so whoever uploads a deck
/// can check it before publishing.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub slides: usize,
    pub general: Vec<String>,
    pub per_slide: Vec<(usize, Vec<String>)>,
}

impl Report {
    fn from(warnings: &[Warning], slides: usize) -> Self {
        let mut r = Report {
            slides,
            ..Default::default()
        };
        for w in warnings {
            match w.slide {
                None => r.general.push(w.message.clone()),
                Some(n) => match r.per_slide.iter_mut().find(|(s, _)| *s == n) {
                    Some((_, v)) => v.push(w.message.clone()),
                    None => r.per_slide.push((n, vec![w.message.clone()])),
                },
            }
        }
        r.per_slide.sort_by_key(|(n, _)| *n);
        r
    }

    pub fn is_clean(&self) -> bool {
        self.general.is_empty() && self.per_slide.is_empty()
    }

    pub fn to_text(&self) -> String {
        if self.is_clean() {
            return format!("All {} slides converted without known losses.\n", self.slides);
        }
        let mut o = format!(
            "{} of {} slides have elements that were simplified or skipped:\n",
            self.per_slide.len(),
            self.slides
        );
        for (n, ws) in &self.per_slide {
            o += &format!("  slide {n}:\n");
            for w in ws {
                o += &format!("    - {w}\n");
            }
        }
        if !self.general.is_empty() {
            o += "  whole deck:\n";
            for w in &self.general {
                o += &format!("    - {w}\n");
            }
        }
        o
    }

    pub fn to_json(&self) -> String {
        let list = |v: &[String]| v.iter().map(|s| fragments::js(s)).collect::<Vec<_>>().join(", ");
        let slides: Vec<String> = self
            .per_slide
            .iter()
            .map(|(n, w)| format!("{{ \"slide\": {n}, \"warnings\": [{}] }}", list(w)))
            .collect();
        format!(
            "{{ \"slides\": {}, \"clean\": {}, \"general\": [{}], \"perSlide\": [{}] }}",
            self.slides,
            self.is_clean(),
            list(&self.general),
            slides.join(", ")
        )
    }
}

#[derive(Debug, Clone)]
pub struct Conversion {
    /// Full PDF, empty when `Options::full_pdf` is false.
    pub pdf: Vec<u8>,
    pub fragments: Vec<Fragment>,
    pub manifest: Manifest,
    pub report: Report,
}

/// Result of a streaming conversion: the outputs went to the sinks.
#[derive(Debug, Clone)]
pub struct Summary {
    pub manifest: Manifest,
    pub report: Report,
}

/// Sizes up a package in milliseconds, without converting it: slide count,
/// page size and media bytes. See [`Inspection`].
pub fn inspect<R: Read + Seek>(input: R) -> Result<Inspection, Error> {
    Ok(pptx::inspect(&mut pptx::Package::open(input)?)?)
}

/// Where a streaming conversion is, reported after each slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Slides finished so far.
    pub done: usize,
    pub total: usize,
}

/// The error a cancelled conversion returns (see [`convert_stream_with`]).
pub const CANCELLED: &str = "conversion cancelled";

/// Parses a PPTX into the intermediate representation.
pub fn parse(pptx: &[u8], include_hidden: bool) -> Result<Deck, Error> {
    Ok(pptx::parse(pptx, &pptx::ParseOptions { include_hidden })?)
}

/// Converts a PPTX held in memory. Convenient, but every output is kept in
/// memory too; for servers with tight memory limits use [`convert_stream`].
pub fn convert(pptx: &[u8], opts: &Options) -> Result<Conversion, Error> {
    let mut pdf = Vec::new();
    let mut frags = Vec::new();
    let full: Option<&mut dyn Write> = if opts.full_pdf { Some(&mut pdf) } else { None };
    let summary = convert_stream(Cursor::new(pptx), opts, full, &mut |f| {
        frags.push(f);
        Ok(())
    })?;
    Ok(Conversion {
        pdf,
        fragments: frags,
        manifest: summary.manifest,
        report: summary.report,
    })
}

/// Converts a PPTX read from any seekable source (e.g. a `File`) with
/// bounded memory: the full PDF is written to `full_pdf` page by page, and
/// each fragment is handed to `on_fragment` as soon as it is ready, then
/// dropped. Peak memory is about one slide plus the image being prepared
/// plus `Options::cache_bytes`, whatever the size of the deck.
pub fn convert_stream<R: Read + Seek>(
    input: R,
    opts: &Options,
    full_pdf: Option<&mut dyn Write>,
    on_fragment: &mut dyn FnMut(Fragment) -> Result<(), String>,
) -> Result<Summary, Error> {
    convert_stream_with(input, opts, full_pdf, on_fragment, &mut |_| true)
}

/// [`convert_stream`] with a progress hook, called after every slide.
/// Returning `false` stops the conversion with an error whose message is
/// [`CANCELLED`]: that is how a server enforces a deadline, or an abort
/// signal, without killing the process or the WebAssembly instance.
pub fn convert_stream_with<R: Read + Seek>(
    input: R,
    opts: &Options,
    full_pdf: Option<&mut dyn Write>,
    on_fragment: &mut dyn FnMut(Fragment) -> Result<(), String>,
    on_progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<Summary, Error> {
    let mut pkg = pptx::Package::open(input)?;
    let deck = pptx::parse_package(
        &mut pkg,
        &pptx::ParseOptions {
            include_hidden: opts.include_hidden,
        },
    )?;
    let fonts = if opts.preset == Preset::Light {
        Vec::new()
    } else {
        // A variable font becomes one face per weight it covers.
        opts.fonts.iter().flat_map(FontFile::instances).collect()
    };
    let embedded = &deck.embedded;
    let mut r = pdf::Renderer::new(
        &deck,
        Box::new(move |path: &str| embedded.get(path).cloned().or_else(|| pkg.read_media(path))),
        pdf::PdfOptions {
            image_dpi: opts.image_dpi,
            jpeg_quality: opts.jpeg_quality,
            fonts,
            cache_bytes: opts.cache_bytes,
        },
    );

    let mut counter = CountingWriter { inner: full_pdf, bytes: 0 };
    let has_full = counter.inner.is_some();
    let mut doc = has_full.then(|| r.begin(&mut counter));
    let mut entries = Vec::new();
    for (i, s) in deck.slides.iter().enumerate() {
        if let Some(d) = doc.as_mut() {
            r.add_slide(d, i);
        }
        if opts.fragments {
            let pdf = r.render(&[i]);
            let warnings = deck
                .warnings
                .iter()
                .chain(&r.warnings)
                .filter(|w| w.slide == Some(s.number))
                .map(|w| w.message.clone())
                .collect();
            let f = Fragment {
                number: s.number,
                file: fragments::fragment_file_name(s.number),
                pdf,
                title: s.title().filter(|t| !t.is_empty()),
                text: s.text(),
                warnings,
            };
            entries.push(f.entry());
            on_fragment(f).map_err(Error)?;
        }
        if !on_progress(Progress {
            done: i + 1,
            total: deck.slides.len(),
        }) {
            return Err(Error(CANCELLED.into()));
        }
    }
    if let Some(d) = doc {
        r.finish(d).map_err(Error)?;
    }

    let mut warnings = deck.warnings.clone();
    for w in r.warnings.drain(..) {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    let report = Report::from(&warnings, deck.slides.len());
    let manifest = Manifest {
        source: opts.source_name.clone(),
        width: deck.width / ir::EMU_PER_PT,
        height: deck.height / ir::EMU_PER_PT,
        fragments: entries,
        pdf_file: has_full.then(|| "deck.pdf".into()),
        pdf_bytes: has_full.then_some(counter.bytes),
        warnings: report.general.clone(),
    };
    Ok(Summary { manifest, report })
}

struct CountingWriter<'a> {
    inner: Option<&'a mut dyn Write>,
    bytes: usize,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = match self.inner.as_mut() {
            Some(w) => w.write(b)?,
            None => b.len(),
        };
        self.bytes += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.as_mut().map_or(Ok(()), |w| w.flush())
    }
}

#[cfg(feature = "assemble")]
pub use fragments::{assemble, split};
