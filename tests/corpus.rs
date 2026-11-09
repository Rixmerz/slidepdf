//! Real-deck regression: converts a corpus of PPTX files from the LibreOffice
//! and Apache POI test suites and compares the outcome of every deck with
//! `tests/corpus-baseline.json`.
//!
//! Ignored by default because the corpus is ~180 MB. Fetch it with
//! `scripts/corpus.sh <folder>`, then:
//!
//! ```sh
//! SLIDEPDF_CORPUS=<folder> cargo test --release --test corpus -- --ignored
//! SLIDEPDF_CORPUS_UPDATE=1 SLIDEPDF_CORPUS=<folder> cargo test --release --test corpus -- --ignored
//! ```
//!
//! The second form rewrites the baseline after an intended change.
//! `SLIDEPDF_CORPUS_TIMEOUT_SECS` overrides the per-deck time cap.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufReader, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use slidepdf::{Options, Preset};

#[global_allocator]
static ALLOC: slidepdf::budget::Budget = slidepdf::budget::Budget;

/// Live heap cap for the whole test process. Exceeding it aborts the
/// process with the allocator's message, which is the failure signal.
const MEMORY_BUDGET: usize = 256 << 20;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus-baseline.json");

/// Counts the bytes of the PDF and throws them away.
struct Sink(u64);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn collect_decks(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let kind = e.file_type().unwrap();
        if kind.is_dir() {
            if e.file_name() != ".git" {
                collect_decks(&path, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("pptx")) {
            out.push(path);
        }
    }
}

/// Strips what varies between decks from a warning so equal problems
/// compare equal: quoted names (fonts, shapes), part paths, the detail after
/// a path prefix or a closing parenthesis (an error text, a character list)
/// and digits. What is left names the kind of loss.
fn warning_kind(message: &str) -> String {
    let mut text = message.to_string();
    // "ppt/media/image1.png: <decoder error>, skipped" -> "<part>: skipped"
    if let Some((head, _)) = text.split_once(": ") {
        if head.contains('/') {
            let verdict = if text.ends_with(", skipped") { "skipped" } else { "" };
            return format!("<part>: {verdict}").trim_end().to_string();
        }
    }
    // "... (register a font that has them): ÄÖ" -> "... (register a font that has them)"
    if let Some(i) = text.find("): ") {
        text.truncate(i + 1);
    }
    // 'Aptos' -> '_' and (ppt/media/image1.tiff) -> (_)
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(c) = rest.chars().next() {
        let close = match c {
            '\'' => Some('\''),
            '(' if rest[1..].starts_with("ppt/") => Some(')'),
            _ => None,
        };
        match close.and_then(|q| rest[1..].find(q).map(|i| (q, i))) {
            Some((q, i)) => {
                out.push_str(&format!("{c}_{q}"));
                rest = &rest[i + 2..];
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    normalize(&out)
}

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_digits = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    out
}

fn json_str(s: &str) -> String {
    let mut o = String::from('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One deck's outcome as a single JSON object on one line, so the baseline
/// diffs line by line in review.
enum Outcome {
    Converted {
        slides: usize,
        warnings: usize,
        kinds: Vec<String>,
    },
    Error(String),
}

impl Outcome {
    fn to_json(&self) -> String {
        match self {
            Outcome::Converted { slides, warnings, kinds } => {
                let kinds: Vec<String> = kinds.iter().map(|k| json_str(k)).collect();
                format!(
                    "{{\"converted\": true, \"slides\": {slides}, \"warnings\": {warnings}, \"kinds\": [{}]}}",
                    kinds.join(", ")
                )
            }
            Outcome::Error(e) => format!("{{\"converted\": false, \"error\": {}}}", json_str(e)),
        }
    }
}

/// Reads `"key": <number>` out of a line this file wrote.
fn number(line: &str, key: &str) -> Option<usize> {
    let rest = &line[line.find(&format!("\"{key}\": "))? + key.len() + 4..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Converts one deck. Err carries a problem that fails the test whatever
/// the baseline says: a panic, a timeout, or a slide count that disagrees
/// with `inspect`.
fn run_deck(path: &Path, cap: Duration) -> Result<Outcome, String> {
    let open = || BufReader::new(File::open(path).unwrap());
    let started = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Outcome, String> {
        let inspected = match slidepdf::inspect(open()) {
            Ok(i) => i,
            Err(e) => return Ok(Outcome::Error(normalize(&e.0))),
        };
        // Hidden slides are rendered too: `inspect` counts them, and the
        // slide-count check below needs both sides to see the same slides.
        let mut opts = Options::preset(Preset::Light);
        opts.include_hidden = true;
        let (mut fragments, mut last_done, mut timed_out) = (0usize, 0usize, false);
        let mut pdf = Sink(0);
        let converted = slidepdf::convert_stream_with(
            open(),
            &opts,
            Some(&mut pdf),
            &mut |_| {
                fragments += 1;
                Ok(())
            },
            &mut |p| {
                last_done = p.done;
                timed_out = started.elapsed() > cap;
                !timed_out
            },
        );
        if timed_out {
            return Err(format!("took longer than {cap:?}"));
        }
        let summary = match converted {
            Ok(s) => s,
            Err(e) => return Ok(Outcome::Error(normalize(&e.0))),
        };
        let r = &summary.report;
        let mut messages: Vec<&String> = r.general.iter().collect();
        messages.extend(r.per_slide.iter().flat_map(|(_, w)| w));
        // `inspect` counts every slide listed in the package; a slide part
        // that is missing or not XML is skipped with this warning instead.
        let skipped = messages.iter().filter(|m| m.ends_with(": missing or invalid XML, skipped")).count();
        if fragments + skipped != inspected.slides || last_done != fragments {
            return Err(format!(
                "inspect says {} slides, conversion rendered {fragments} fragments (progress {last_done}, {skipped} skipped with a warning)",
                inspected.slides
            ));
        }
        if pdf.0 == 0 {
            return Err("converted but wrote no PDF bytes".into());
        }
        let mut kinds: Vec<String> = messages.iter().map(|m| warning_kind(m)).collect();
        kinds.sort();
        kinds.dedup();
        Ok(Outcome::Converted {
            slides: r.slides,
            warnings: messages.len(),
            kinds,
        })
    }));
    let outcome = match result {
        Ok(r) => r?,
        Err(_) => return Err("panicked".into()),
    };
    if started.elapsed() > cap {
        return Err(format!("took longer than {cap:?}"));
    }
    Ok(outcome)
}

fn parse_baseline(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim().trim_end_matches(',');
        // Entry lines look like `"lo/a.pptx": {...}`; the braces alone are skipped.
        if let Some(rest) = line.strip_prefix('"') {
            if let Some((name, value)) = rest.split_once("\": ") {
                map.insert(name.replace("\\\"", "\"").replace("\\\\", "\\"), value.to_string());
            }
        }
    }
    map
}

fn render_baseline(map: &BTreeMap<String, String>) -> String {
    let mut o = String::from("{\n");
    for (i, (name, value)) in map.iter().enumerate() {
        let comma = if i + 1 < map.len() { "," } else { "" };
        let _ = writeln!(o, "  {}: {value}{comma}", json_str(name));
    }
    o.push_str("}\n");
    o
}

/// Names the direction of a change so a reviewer sees what got better.
fn describe(old: &str, new: &str) -> &'static str {
    let (old_ok, new_ok) = (old.contains("\"converted\": true"), new.contains("\"converted\": true"));
    match (old_ok, new_ok) {
        (false, true) => "BETTER (now converts)",
        (true, false) => "WORSE (no longer converts)",
        (false, false) => "changed error",
        (true, true) => {
            let (ow, nw) = (number(old, "warnings"), number(new, "warnings"));
            if number(old, "slides") != number(new, "slides") {
                "WORSE? (slide count changed)"
            } else if nw < ow {
                "BETTER (fewer warnings)"
            } else if nw > ow {
                "WORSE (more warnings)"
            } else {
                "changed (same warning count, different kinds)"
            }
        }
    }
}

#[test]
#[ignore = "needs the corpus: set SLIDEPDF_CORPUS (see scripts/corpus.sh)"]
fn real_decks_match_the_baseline() {
    let root = std::env::var("SLIDEPDF_CORPUS").expect("set SLIDEPDF_CORPUS to the folder made by scripts/corpus.sh");
    let root = PathBuf::from(root);
    let cap = std::env::var("SLIDEPDF_CORPUS_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs);

    let mut decks = Vec::new();
    collect_decks(&root, &mut decks);
    assert!(
        decks.len() > 100,
        "only {} decks under {}: wrong folder?",
        decks.len(),
        root.display()
    );

    slidepdf::budget::set_limit(MEMORY_BUDGET);
    let mut now = BTreeMap::new();
    let mut problems = Vec::new();
    for path in &decks {
        let name = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        // Straight to stderr: the test harness drops captured output when the
        // allocator exits the process on a budget breach, and this line
        // names the deck that did it.
        let _ = writeln!(io::stderr(), "corpus: {name}");
        match run_deck(path, cap) {
            Ok(o) => {
                now.insert(name, o.to_json());
            }
            Err(p) => problems.push(format!("{name}: {p}")),
        }
    }
    slidepdf::budget::set_limit(0);
    assert!(problems.is_empty(), "robustness failures:\n  {}", problems.join("\n  "));

    if std::env::var_os("SLIDEPDF_CORPUS_UPDATE").is_some() {
        fs::write(BASELINE, render_baseline(&now)).unwrap();
        println!("baseline rewritten: {} decks", now.len());
        return;
    }

    let old = parse_baseline(&fs::read_to_string(BASELINE).expect("tests/corpus-baseline.json is missing"));
    let mut report = String::new();
    for (name, value) in &now {
        match old.get(name) {
            None => {
                let _ = writeln!(report, "NEW      {name}\n           now: {value}");
            }
            Some(before) if before != value => {
                let _ = writeln!(
                    report,
                    "{:<8} {name}\n          was: {before}\n          now: {value}",
                    describe(before, value).split(' ').next().unwrap()
                );
                let _ = writeln!(report, "          ({})", describe(before, value));
            }
            Some(_) => {}
        }
    }
    for name in old.keys().filter(|n| !now.contains_key(*n)) {
        let _ = writeln!(report, "MISSING  {name} (in the baseline, not in the corpus)");
    }
    assert!(
        report.is_empty(),
        "corpus results differ from tests/corpus-baseline.json:\n\n{report}\n\
         If the change is intended, refresh the baseline and commit it:\n  \
         SLIDEPDF_CORPUS_UPDATE=1 SLIDEPDF_CORPUS={} cargo test --release --test corpus -- --ignored",
        root.display()
    );
}
