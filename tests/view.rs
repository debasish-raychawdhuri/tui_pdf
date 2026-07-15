//! End-to-end view tests: drive the real `tui-pdf` binary headless through the
//! `ghostty-term-use` harness (a libghostty-vt terminal on a PTY) and assert on
//! the rendered screen — chrome as text, the PDF page as kitty-image placements.
//!
//! Requires the `gtu` harness binary. Build it once:
//!     (cd ../ghostty-term-use && zig build -Doptimize=ReleaseFast)
//! or point GTU_BIN at it. If it's not found, these tests skip (not fail).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Locate the harness binary: $GTU_BIN, else the sibling project's build output.
fn gtu_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("GTU_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ghostty-term-use/zig-out/bin/gtu");
    if sibling.exists() {
        return Some(sibling);
    }
    None
}

fn fixture() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample.pdf")
        .to_string_lossy()
        .into_owned()
}

/// A throwaway HOME so tui-pdf reads no real config/sessions.
fn temp_home(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("tui_pdf_view_{}_{}", std::process::id(), tag));
    let _ = std::fs::create_dir_all(dir.join("docs"));
    dir.to_string_lossy().into_owned()
}

/// Run a scenario through gtu and return the parsed result, or None to skip.
fn run(steps: serde_json::Value, home: &str) -> Option<serde_json::Value> {
    let gtu = match gtu_bin() {
        Some(g) => g,
        None => {
            eprintln!("SKIP: gtu harness not found (build ../ghostty-term-use or set GTU_BIN)");
            return None;
        }
    };
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let scenario = serde_json::json!({
        "cmd": [env!("CARGO_BIN_EXE_tui-pdf"), fixture()],
        "cols": 100, "rows": 30, "cell_w": 10, "cell_h": 20,
        "env": [
            "TERM=xterm-kitty",
            "COLORTERM=truecolor",
            format!("HOME={home}"),
            format!("PATH={path}"),
        ],
        "steps": steps,
    });

    let mut child = Command::new(&gtu)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gtu");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(scenario.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("gtu output");
    let val: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("gtu did not return JSON: {e}\nstdout: {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(val["ok"], true, "gtu error: {val}");
    Some(val)
}

/// Fetch a named snapshot's text.
fn snap_text<'a>(v: &'a serde_json::Value, name: &str) -> &'a str {
    v["snaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no snap named {name}"))
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
}

fn snap<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    v["snaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap()
}

#[test]
fn initial_view_shows_status_bar_and_page_image() {
    let home = temp_home("initial");
    let Some(v) = run(
        serde_json::json!([
            {"op":"wait","idle_ms":700,"budget_ms":15000},
            {"op":"snap","name":"initial"},
        ]),
        &home,
    ) else {
        return;
    };

    let text = snap_text(&v, "initial");
    assert!(
        text.contains("Page 1/1"),
        "status bar should show page count; got:\n{text}"
    );
    assert!(text.contains("Zoom:"), "status bar should show zoom");

    let images = snap(&v, "initial")["images"].as_u64().unwrap();
    assert!(images > 0, "the PDF page should render as >=1 kitty image, got {images}");
}

#[test]
fn pressing_e_opens_bordered_file_browser() {
    let home = temp_home("browser");
    let Some(v) = run(
        serde_json::json!([
            {"op":"wait","idle_ms":700,"budget_ms":15000},
            {"op":"send_keys","keys":["e"]},
            {"op":"wait","idle_ms":500,"budget_ms":6000},
            {"op":"snap","name":"browser"},
        ]),
        &home,
    ) else {
        return;
    };

    let text = snap_text(&v, "browser");
    // The rounded border we added.
    assert!(text.contains('╭') && text.contains('╰'), "file browser should be bordered; got:\n{text}");
    // Its status line.
    assert!(text.contains("Esc: cancel"), "file browser status line missing; got:\n{text}");
    assert!(text.contains("filter"), "file browser hint missing");
}

#[test]
fn pressing_d_opens_document_picker() {
    let home = temp_home("picker");
    let Some(v) = run(
        serde_json::json!([
            {"op":"wait","idle_ms":700,"budget_ms":15000},
            {"op":"send_keys","keys":["d"]},
            {"op":"wait","idle_ms":400,"budget_ms":5000},
            {"op":"snap","name":"picker"},
        ]),
        &home,
    ) else {
        return;
    };

    let text = snap_text(&v, "picker");
    assert!(text.contains("Open Documents"), "doc picker title missing; got:\n{text}");
}

/// A single stateful session that opens and closes each overlay, re-asserting
/// the base PDF view after each. This is what the batch runner buys us: it
/// catches an overlay's teardown leaving the viewer in a bad state, which
/// isolated per-assertion runs would miss.
#[test]
fn overlay_open_close_roundtrips_preserve_viewer() {
    let home = temp_home("journey");
    let Some(v) = run(
        serde_json::json!([
            {"op":"wait","idle_ms":700,"budget_ms":15000},
            {"op":"snap","name":"initial"},
            {"op":"send_keys","keys":["e"]},
            {"op":"wait","idle_ms":500,"budget_ms":6000},
            {"op":"snap","name":"browser"},
            {"op":"send_keys","keys":["Esc"]},
            {"op":"wait","idle_ms":500,"budget_ms":6000},
            {"op":"snap","name":"after_browser"},
            {"op":"send_keys","keys":["d"]},
            {"op":"wait","idle_ms":400,"budget_ms":5000},
            {"op":"snap","name":"picker"},
            {"op":"send_keys","keys":["Esc"]},
            {"op":"wait","idle_ms":400,"budget_ms":5000},
            {"op":"snap","name":"after_picker"},
        ]),
        &home,
    ) else {
        return;
    };

    let is_pdf_view = |name: &str| {
        let t = snap_text(&v, name);
        assert!(t.contains("Page 1/1"), "{name}: expected PDF view (Page 1/1); got:\n{t}");
        let imgs = snap(&v, name)["images"].as_u64().unwrap();
        assert!(imgs > 0, "{name}: expected page image present, got {imgs} images");
    };

    // Base view, and the base view restored after each overlay closes.
    is_pdf_view("initial");
    is_pdf_view("after_browser");
    is_pdf_view("after_picker");

    // Overlays actually opened in between.
    let browser = snap_text(&v, "browser");
    assert!(browser.contains('╭') && browser.contains("Esc: cancel"), "browser overlay not shown");
    assert!(snap_text(&v, "picker").contains("Open Documents"), "doc picker not shown");
}
