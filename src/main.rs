//! `slidepdf` command line.

// Without `assemble`, the split/assemble helpers are compiled out.
#![cfg_attr(not(feature = "assemble"), allow(dead_code, unused_imports))]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use slidepdf::{fragments, Options, Preset};

/// Counting allocator, so `--memory-limit` can cut a conversion off and
/// the summary can show the peak.
#[global_allocator]
static ALLOC: slidepdf::budget::Budget = slidepdf::budget::Budget;

const VIEWER_JS: &str = include_str!("../viewer/slide-viewer.js");
const VIEWER_HTML: &str = include_str!("../viewer/index.html");

const USAGE: &str = "\
slidepdf - ultra-light PPTX to PDF converter with per-slide fragments

USAGE:
  slidepdf convert   <deck.pptx> [-o out.pdf] [options]
  slidepdf fragments <deck.pptx|doc.pdf> -o <dir> [options]
  slidepdf assemble  <dir|a.pdf b.pdf ...> -o out.pdf [--range 2-5,8]
  slidepdf report    <deck.pptx> [--json]
  slidepdf inspect   <deck.pptx> [--json]
  slidepdf search    <dir> <words...>

OPTIONS:
  --preset light|balanced|print   size/quality trade-off (default: balanced)
  --font <file.ttf>               embed this font (subset); repeatable
  --hidden                        include hidden slides
  --no-viewer                     fragments: do not write index.html / slide-viewer.js
  --no-pdf                        fragments: do not write the full deck.pdf
  --cache-mb <n>                  memory for reusing prepared images (default 16)
  --memory-limit <mb>             abort if heap use exceeds this (exit code 134)
  --timeout <seconds>             abort if the command runs longer (exit code 124)
  --quiet                         no summary on stderr (errors and warnings only)

`inspect` sizes up a deck in milliseconds (slides, media bytes) without
converting it, to decide which queue it goes to.
`fragments` writes slide-NNN.pdf (one standalone PDF per slide), manifest.json
(order, hashes, titles, text index), report.json, deck.pdf and a static viewer.
`assemble` rebuilds one PDF from stored fragments, checking their hashes
against manifest.json when given a directory.";

struct Args {
    cmd: String,
    inputs: Vec<String>,
    out: Option<PathBuf>,
    opts: Options,
    range: Option<String>,
    json: bool,
    viewer: bool,
    quiet: bool,
    memory_limit: usize,
    timeout: Option<u64>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or_else(|| USAGE.to_string())?;
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        return Err(USAGE.to_string());
    }
    let mut a = Args {
        cmd,
        inputs: Vec::new(),
        out: None,
        opts: Options::default(),
        range: None,
        json: false,
        viewer: true,
        quiet: false,
        memory_limit: 0,
        timeout: None,
    };
    let mut fonts = Vec::new();
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-o" | "--out" => a.out = Some(PathBuf::from(val("-o")?)),
            "--preset" => {
                let p: Preset = val("--preset")?.parse().map_err(|e: slidepdf::Error| e.0)?;
                let keep = std::mem::take(&mut a.opts.fonts);
                a.opts = Options {
                    fonts: keep,
                    ..Options::preset(p)
                };
            }
            "--font" => fonts.push(val("--font")?),
            "--range" => a.range = Some(val("--range")?),
            "--hidden" => a.opts.include_hidden = true,
            "--json" => a.json = true,
            "--no-viewer" => a.viewer = false,
            "--no-pdf" => a.opts.full_pdf = false,
            "--cache-mb" => a.opts.cache_bytes = val("--cache-mb")?.parse::<usize>().map_err(|_| "--cache-mb needs a number")? << 20,
            "--memory-limit" => {
                a.memory_limit = val("--memory-limit")?
                    .parse::<usize>()
                    .map_err(|_| "--memory-limit needs a number of MB")?
                    << 20
            }
            "--timeout" => {
                a.timeout = Some(
                    val("--timeout")?
                        .parse::<u64>()
                        .map_err(|_| "--timeout needs a number of seconds")?,
                )
            }
            "--quiet" | "-q" => a.quiet = true,
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => a.inputs.push(arg),
        }
    }
    for f in fonts {
        let data = std::fs::read(&f).map_err(|e| format!("{f}: {e}"))?;
        a.opts.add_font(data).map_err(|e| format!("{f}: {e}"))?;
    }
    Ok(a)
}

fn read(p: &str) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{p}: {e}"))
}

fn open(p: &str) -> Result<std::io::BufReader<std::fs::File>, String> {
    std::fs::File::open(p).map(std::io::BufReader::new).map_err(|e| format!("{p}: {e}"))
}

fn create(p: &Path) -> Result<std::io::BufWriter<std::fs::File>, String> {
    std::fs::File::create(p)
        .map(std::io::BufWriter::new)
        .map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, data: &[u8]) -> Result<(), String> {
    std::fs::write(p, data).map_err(|e| format!("{}: {e}", p.display()))
}

fn kb(n: usize) -> String {
    format!("{:.1} KB", n as f64 / 1024.0)
}

fn peak_mb() -> String {
    format!("peak memory {:.1} MB", slidepdf::budget::peak() as f64 / 1048576.0)
}

/// Arms the limits from `--memory-limit` and `--timeout`. The timeout is a
/// watchdog thread: the converter is single-threaded and synchronous, so
/// exiting the process is the only way to cut it off.
fn arm_limits(a: &Args) {
    slidepdf::budget::reset_peak();
    if a.memory_limit > 0 {
        slidepdf::budget::set_limit(a.memory_limit);
    }
    if let Some(secs) = a.timeout {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            eprintln!("slidepdf: timeout of {secs} s exceeded");
            std::process::exit(124);
        });
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut a = parse_args()?;
    let first = a.inputs.first().cloned().ok_or_else(|| format!("missing input\n\n{USAGE}"))?;
    arm_limits(&a);
    let quiet = a.quiet;
    match a.cmd.as_str() {
        "convert" => {
            a.opts.fragments = false;
            a.opts.source_name = file_name(&first);
            let dest = a.out.unwrap_or_else(|| PathBuf::from(&first).with_extension("pdf"));
            let input = open(&first)?;
            let mut out = create(&dest)?;
            let summary = slidepdf::convert_stream(input, &a.opts, Some(&mut out), &mut |_| Ok(())).map_err(|e| e.0)?;
            out.flush().map_err(|e| format!("{}: {e}", dest.display()))?;
            if !quiet {
                eprintln!(
                    "{} -> {} ({}, {} slides, {})",
                    first,
                    dest.display(),
                    kb(summary.manifest.pdf_bytes.unwrap_or(0)),
                    summary.report.slides,
                    peak_mb()
                );
            }
            if !summary.report.is_clean() {
                eprint!("{}", summary.report.to_text());
            }
        }
        "fragments" => {
            let dir = a.out.ok_or("fragments needs -o <dir>")?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            a.opts.source_name = file_name(&first);
            let is_pdf = {
                let mut head = [0u8; 4];
                let mut f = open(&first)?;
                f.read_exact(&mut head).is_ok() && &head == b"%PDF"
            };
            let (manifest, report_json) = if is_pdf {
                let bytes = read(&first)?;
                let (m, r) = pdf_fragments(&bytes, &a.opts, &dir)?;
                if a.opts.full_pdf {
                    write(&dir.join("deck.pdf"), &bytes)?;
                }
                (m, r)
            } else {
                let mut full = if a.opts.full_pdf {
                    Some(create(&dir.join("deck.pdf"))?)
                } else {
                    None
                };
                let sink: Option<&mut dyn Write> = full.as_mut().map(|f| f as &mut dyn Write);
                let summary = slidepdf::convert_stream(open(&first)?, &a.opts, sink, &mut |f| {
                    std::fs::write(dir.join(&f.file), &f.pdf).map_err(|e| format!("{}: {e}", f.file))
                })
                .map_err(|e| e.0)?;
                if let Some(mut f) = full {
                    f.flush().map_err(|e| e.to_string())?;
                }
                if !quiet || !summary.report.is_clean() {
                    eprint!("{}", summary.report.to_text());
                }
                (summary.manifest, summary.report.to_json())
            };
            write(&dir.join("manifest.json"), manifest.to_json().as_bytes())?;
            write(&dir.join("report.json"), report_json.as_bytes())?;
            if a.viewer {
                write(&dir.join("slide-viewer.js"), VIEWER_JS.as_bytes())?;
                write(&dir.join("index.html"), VIEWER_HTML.as_bytes())?;
            }
            let total: usize = manifest.fragments.iter().map(|f| f.bytes).sum();
            if !quiet {
                eprintln!(
                    "{} fragments ({} total, {} largest) in {}{}, {}",
                    manifest.fragments.len(),
                    kb(total),
                    kb(manifest.fragments.iter().map(|f| f.bytes).max().unwrap_or(0)),
                    dir.display(),
                    manifest.pdf_bytes.map(|b| format!(", deck.pdf {}", kb(b))).unwrap_or_default(),
                    peak_mb()
                );
            }
        }
        "assemble" => assemble(&a)?,
        "inspect" => {
            let i = slidepdf::inspect(open(&first)?).map_err(|e| e.0)?;
            if a.json {
                println!(
                    "{{\"slides\":{},\"width\":{},\"height\":{},\"parts\":{},\"bytes\":{},\"media\":{},\"mediaBytes\":{},\"largestMediaBytes\":{},\"largestMedia\":{:?},\"embeddedFonts\":{}}}",
                    i.slides, i.width, i.height, i.parts, i.bytes, i.media, i.media_bytes, i.largest_media_bytes, i.largest_media, i.embedded_fonts
                );
            } else {
                println!(
                    "{first}: {} slides, {:.0}x{:.0} pt, {} parts ({:.1} MB), {} media parts ({:.1} MB, largest {:.1} MB {}){}",
                    i.slides,
                    i.width,
                    i.height,
                    i.parts,
                    i.bytes as f64 / 1048576.0,
                    i.media,
                    i.media_bytes as f64 / 1048576.0,
                    i.largest_media_bytes as f64 / 1048576.0,
                    i.largest_media,
                    if i.embedded_fonts > 0 {
                        format!(", {} embedded fonts", i.embedded_fonts)
                    } else {
                        String::new()
                    }
                );
            }
        }
        "report" => {
            let mut o = a.opts.clone();
            o.fragments = false;
            let summary = slidepdf::convert_stream(open(&first)?, &o, Some(&mut std::io::sink()), &mut |_| Ok(())).map_err(|e| e.0)?;
            if a.json {
                println!("{}", summary.report.to_json());
            } else {
                print!("{}", summary.report.to_text());
            }
        }
        "search" => search(&first, &a.inputs[1..])?,
        other => return Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
    Ok(())
}

fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.into())
}

#[cfg(feature = "assemble")]
fn pdf_fragments(bytes: &[u8], opts: &Options, dir: &Path) -> Result<(slidepdf::Manifest, String), String> {
    let mut entries = Vec::new();
    let info = slidepdf::fragments::split_each(bytes, |n, pdf| {
        let f = slidepdf::Fragment {
            number: n,
            file: fragments::fragment_file_name(n),
            pdf,
            title: None,
            text: String::new(),
            warnings: Vec::new(),
        };
        write(&dir.join(&f.file), &f.pdf)?;
        entries.push(f.entry());
        Ok(())
    })?;
    let (w, h) = (info.width, info.height);
    let m = slidepdf::Manifest {
        source: opts.source_name.clone(),
        width: w,
        height: h,
        pdf_file: opts.full_pdf.then(|| "deck.pdf".into()),
        pdf_bytes: opts.full_pdf.then_some(bytes.len()),
        fragments: entries,
        warnings: Vec::new(),
    };
    let n = m.fragments.len();
    Ok((
        m,
        format!("{{ \"slides\": {n}, \"clean\": true, \"general\": [], \"perSlide\": [] }}"),
    ))
}

#[cfg(not(feature = "assemble"))]
fn pdf_fragments(_: &[u8], _: &Options, _: &Path) -> Result<(slidepdf::Manifest, String), String> {
    Err("splitting PDFs needs the `assemble` feature".into())
}

/// Parses "2-5,8" into 1-based slide numbers.
fn parse_range(r: &str, max: usize) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in r.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim().parse::<usize>(),
                if b.trim().is_empty() { Ok(max) } else { b.trim().parse() },
            ),
            None => (part.parse(), part.parse()),
        };
        let (a, b) = (
            a.map_err(|_| format!("bad range '{part}'"))?,
            b.map_err(|_| format!("bad range '{part}'"))?,
        );
        if a == 0 || b > max || a > b {
            return Err(format!("range '{part}' is outside 1-{max}"));
        }
        out.extend(a..=b);
    }
    Ok(out)
}

#[cfg(feature = "assemble")]
fn assemble(a: &Args) -> Result<(), String> {
    let dest = a.out.clone().ok_or("assemble needs -o out.pdf")?;
    let first = Path::new(&a.inputs[0]);
    // (path, expected hash) in output order; read one at a time while merging.
    let mut plan: Vec<(PathBuf, Option<String>)> = Vec::new();
    if first.is_dir() {
        let json =
            std::fs::read_to_string(first.join("manifest.json")).map_err(|e| format!("{}: {e}", first.join("manifest.json").display()))?;
        let entries = fragments::manifest_entries(&json);
        let pick = match &a.range {
            Some(r) => parse_range(r, entries.len())?,
            None => (1..=entries.len()).collect(),
        };
        for n in pick {
            let (file, hash) = &entries[n - 1];
            plan.push((first.join(file), Some(hash.clone())));
        }
    } else {
        let all: Vec<PathBuf> = a.inputs.iter().map(PathBuf::from).collect();
        let pick = match &a.range {
            Some(r) => parse_range(r, all.len())?,
            None => (1..=all.len()).collect(),
        };
        plan = pick.into_iter().map(|n| (all[n - 1].clone(), None)).collect();
    }
    let mut input = 0usize;
    let parts = plan.iter().map(|(p, hash)| {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(h) = hash {
            if !fragments::verify(&bytes, h) {
                return Err(format!(
                    "{}: content does not match the manifest hash (corrupted or replaced)",
                    p.display()
                ));
            }
        }
        input += bytes.len();
        Ok(bytes)
    });
    let mut out = create(&dest)?;
    let n = slidepdf::fragments::assemble_to(parts, &mut out)?;
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    let size = std::fs::metadata(&dest).map(|m| m.len() as usize).unwrap_or(0);
    eprintln!(
        "{} pages from {} inputs ({}) -> {} ({})",
        n,
        plan.len(),
        kb(input),
        dest.display(),
        kb(size)
    );
    Ok(())
}

#[cfg(not(feature = "assemble"))]
fn assemble(_: &Args) -> Result<(), String> {
    Err("this build has no `assemble` feature".into())
}

/// Accent- and case-insensitive word search over the manifest text index.
fn search(dir: &str, words: &[String]) -> Result<(), String> {
    let p = Path::new(dir).join("manifest.json");
    let json = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let words: Vec<String> = words.iter().map(|w| fold(w)).collect();
    if words.is_empty() {
        return Err("search needs at least one word".into());
    }
    let mut hits = 0;
    for line in json.lines().filter(|l| l.contains("\"number\":")) {
        let f = fold(line);
        if words.iter().all(|w| f.contains(w.as_str())) {
            let num = line.split("\"number\": ").nth(1).and_then(|s| s.split(',').next()).unwrap_or("?");
            let title = line.split("\"title\": \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
            println!("slide {num}: {title}");
            hits += 1;
        }
    }
    if hits == 0 {
        println!("no slides match");
    }
    Ok(())
}

fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'Á' | 'À' | 'Ä' | 'Â' => 'a',
            'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'Ó' | 'Ò' | 'Ö' | 'Ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}
