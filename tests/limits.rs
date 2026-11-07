//! Operational limits: inspect, cooperative cancellation, and the CLI's
//! memory limit and timeout.

use std::io::Cursor;
use std::process::Command;

const DECK: &[u8] = include_bytes!("fixtures/deck.pptx");

#[test]
fn inspect_reads_only_the_directory() {
    let i = slidepdf::inspect(Cursor::new(DECK)).unwrap();
    assert_eq!(i.slides, 4);
    assert_eq!(i.width, 720.0);
    assert_eq!(i.height, 540.0);
    assert!(i.parts > 10);
    assert!(i.media >= 1);
    assert!(i.media_bytes <= i.bytes);
    assert!(i.largest_media.starts_with("ppt/media/"));
    assert!(i.largest_media_bytes <= i.media_bytes);
}

#[test]
fn inspect_rejects_garbage() {
    assert!(slidepdf::inspect(Cursor::new(b"not a zip")).is_err());
}

#[test]
fn progress_hook_can_cancel() {
    let opts = slidepdf::Options::preset(slidepdf::Preset::Light);
    let mut seen = Vec::new();
    let mut frags = 0;
    let err = slidepdf::convert_stream_with(
        Cursor::new(DECK),
        &opts,
        None,
        &mut |_| {
            frags += 1;
            Ok(())
        },
        &mut |p| {
            seen.push((p.done, p.total));
            p.done < 2
        },
    )
    .unwrap_err();
    assert_eq!(err.0, slidepdf::CANCELLED);
    assert_eq!(seen, vec![(1, 4), (2, 4)]);
    assert_eq!(frags, 2, "fragments before the cancellation were delivered");
}

#[test]
fn progress_hook_sees_every_slide() {
    let opts = slidepdf::Options::preset(slidepdf::Preset::Light);
    let mut seen = Vec::new();
    let s = slidepdf::convert_stream_with(Cursor::new(DECK), &opts, None, &mut |_| Ok(()), &mut |p| {
        seen.push(p.done);
        true
    })
    .unwrap();
    assert_eq!(seen, vec![1, 2, 3, 4]);
    assert_eq!(s.report.slides, 4);
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_slidepdf"))
}

fn fixture() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/deck.pptx")
}

#[test]
fn cli_memory_limit_aborts_with_a_message() {
    let dir = std::env::temp_dir().join(format!("slidepdf-limit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = cli()
        .args(["convert", fixture(), "-o"])
        .arg(dir.join("out.pdf"))
        .args(["--memory-limit", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(slidepdf::budget::EXIT_CODE));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("memory limit of 1 MB exceeded"), "stderr: {err}");
    assert!(!err.contains("allocation"), "a clean exit, not an abort: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_memory_limit_passes_when_generous() {
    let dir = std::env::temp_dir().join(format!("slidepdf-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = cli()
        .args(["convert", fixture(), "-o"])
        .arg(dir.join("out.pdf"))
        .args(["--memory-limit", "64", "--timeout", "60"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {err}");
    assert!(err.contains("peak memory"), "stderr: {err}");
    assert!(dir.join("out.pdf").metadata().unwrap().len() > 1000);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_inspect_json() {
    let out = cli().args(["inspect", fixture(), "--json"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.starts_with("{\"slides\":4,"), "stdout: {s}");
    assert!(s.contains("\"largestMedia\":\"ppt/media/"), "stdout: {s}");
}

#[test]
fn cli_quiet_prints_nothing_on_a_clean_run() {
    // The fixture has a warning on slide 4, so the report still prints; only
    // the summary line disappears.
    let dir = std::env::temp_dir().join(format!("slidepdf-quiet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = cli()
        .args(["convert", fixture(), "-o"])
        .arg(dir.join("out.pdf"))
        .arg("--quiet")
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("peak memory"), "stderr: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}
