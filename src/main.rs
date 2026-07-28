use tui_pdf::synctex_positions;
use std::fs;
use std::io::{self, stdout, BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, StatefulWidget, Widget};
use ratatui::Terminal;
use ratatui_image::picker::Picker;


use tui_pdf::{
    ContentSource, Document, LinkState, PdfViewState, PdfWidget, SearchState, StatusBar,
    TocState, TocWidget, capture_url,
    ZoteroEntry, ZoteroLibrary, latest_pdf, load_config, load_library, save_config,
    send_forward, socket_path, synctex_edit, synctex_view, jump_to_neovim,
    load_session, save_session, list_sessions, move_sessions_dir, lookup_by_path, Session, SessionDoc,
};

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// If the given path is a .tex file, find the corresponding PDF.
/// Strategy:
/// 1. Same basename with .pdf extension next to the .tex
/// 2. Walk up from the .tex's directory, scanning each level for a
///    .synctex.gz/.synctex file whose Input entries reference this .tex. The
///    PDF and synctex are often in the project root while the source lives in
///    a subdirectory (multi-file projects, `latexmk -outdir`, etc.)
/// 3. Falls back to the original path if nothing is found
fn resolve_tex_to_pdf(path: &str) -> String {
    let p = std::path::Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !ext.eq_ignore_ascii_case("tex") {
        return path.to_string();
    }

    let canonical_tex = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());

    // 1. Same basename, .pdf extension next to the .tex
    let same_name_pdf = p.with_extension("pdf");
    if same_name_pdf.exists() {
        return same_name_pdf.to_string_lossy().to_string();
    }

    // 2. Walk up parent directories, scanning each for a synctex file that
    //    references this .tex. Bounded to a handful of levels to avoid walking
    //    to the filesystem root on unrelated trees.
    let mut dir = canonical_tex.parent();
    for _ in 0..8 {
        let Some(d) = dir else { break };
        if let Some(pdf) = find_pdf_via_synctex(d, &canonical_tex) {
            return pdf;
        }
        dir = d.parent();
    }

    path.to_string()
}

/// Scan a single directory for a .synctex(.gz) file whose Input entries
/// reference `canonical_tex`, returning the sibling PDF path if found.
fn find_pdf_via_synctex(dir: &std::path::Path, canonical_tex: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let epath = entry.path();
        let fname = epath.file_name().unwrap_or_default().to_string_lossy().to_string();
        let is_synctex_gz = fname.ends_with(".synctex.gz");
        let is_synctex = !is_synctex_gz && fname.ends_with(".synctex");
        if !is_synctex_gz && !is_synctex {
            continue;
        }

        // Derive the PDF path from the synctex filename
        let pdf_name = if is_synctex_gz {
            fname.strip_suffix(".synctex.gz").unwrap().to_string() + ".pdf"
        } else {
            fname.strip_suffix(".synctex").unwrap().to_string() + ".pdf"
        };
        let pdf_path = dir.join(&pdf_name);
        if !pdf_path.exists() {
            continue;
        }

        // Check if this synctex file references our .tex file
        if synctex_references_tex(&epath, canonical_tex, dir) {
            return Some(pdf_path.to_string_lossy().to_string());
        }
    }
    None
}

/// Check whether a synctex file references a given .tex file in its Input entries.
fn synctex_references_tex(synctex_path: &std::path::Path, tex_path: &std::path::Path, pdf_dir: &std::path::Path) -> bool {
    use std::io::Read as _;

    let fname = synctex_path.file_name().unwrap_or_default().to_string_lossy();
    let data = if fname.ends_with(".synctex.gz") {
        let file = match std::fs::File::open(synctex_path) { Ok(f) => f, Err(_) => return false };
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut s = String::new();
        if decoder.read_to_string(&mut s).is_err() { return false; }
        s
    } else {
        match std::fs::read_to_string(synctex_path) { Ok(s) => s, Err(_) => return false }
    };

    for line in data.lines() {
        if let Some(rest) = line.strip_prefix("Input:") {
            if let Some((_tag_str, filepath)) = rest.split_once(':') {
                let resolved = if filepath.starts_with("./") || !filepath.starts_with('/') {
                    pdf_dir.join(filepath)
                } else {
                    std::path::PathBuf::from(filepath)
                };
                let canonical = resolved.canonicalize().unwrap_or(resolved);
                if canonical == *tex_path {
                    return true;
                }
            }
        }
    }
    false
}

fn render_metadata_overlay(
    fields: &[(String, String)],
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    // Wrap the fields in a rounded border with the help text as its title,
    // matching the file/Zotero browsers.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " Metadata (c: copy BibTeX | u: open URL | Esc/m: close) ",
            Style::default().fg(Color::Cyan),
        ));
    let inner = block.inner(area);
    block.render(area, buf);

    let label_style = Style::default().fg(Color::Yellow);
    let value_style = Style::default().fg(Color::White);
    let max_label = fields.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    let prefix_width = 2 + max_label + 2; // "  Label: "
    let value_width = (inner.width as usize).saturating_sub(prefix_width);
    let mut row = inner.y;
    for (label, value) in fields.iter() {
        if row >= inner.y + inner.height { break; }
        let prefix = format!("  {:>width$}: ", label, width = max_label);
        let lines: Vec<&str> = if value_width > 0 && value.len() > value_width {
            let mut parts = Vec::new();
            let mut start = 0;
            while start < value.len() {
                let end = (start + value_width).min(value.len());
                parts.push(&value[start..end]);
                start = end;
            }
            parts
        } else {
            vec![value.as_str()]
        };
        for (li, line_text) in lines.iter().enumerate() {
            if row >= inner.y + inner.height { break; }
            let line_area = ratatui::layout::Rect {
                x: inner.x, y: row, width: inner.width, height: 1,
            };
            let line = if li == 0 {
                Line::from(vec![
                    Span::styled(prefix.clone(), label_style),
                    Span::styled(line_text.to_string(), value_style),
                ])
            } else {
                Line::from(vec![
                    Span::raw(" ".repeat(prefix_width)),
                    Span::styled(line_text.to_string(), value_style),
                ])
            };
            Paragraph::new(line).render(line_area, buf);
            row += 1;
        }
    }
}

fn build_session(
    open_docs: &[OpenDoc],
    current_idx: usize,
    pdf_state: &PdfViewState,
    last_browse_dir: Option<&str>,
) -> Session {
    let now = tui_pdf::remarkable::now_secs();
    Session {
        last_browse_dir: last_browse_dir.map(|s| s.to_string()),
        docs: open_docs.iter().enumerate().map(|(i, d)| {
            let is_current = i == current_idx;
            let page = if is_current { Some(pdf_state.current_page()) } else { d.page };
            // Bump the position timestamp only when the page actually changed,
            // so merely viewing a doc doesn't make the computer win a sync.
            let modified = if is_current && page != d.page { now } else { d.modified };
            SessionDoc {
                path: d.path.clone(),
                scroll: if is_current { pdf_state.global_scroll } else { d.scroll },
                zoom: if is_current { pdf_state.zoom } else { d.zoom },
                page,
                modified,
                remarkable_uuid: d.remarkable_uuid.clone(),
                render_path: d.render_path.clone(),
            }
        }).collect(),
        current: current_idx,
    }
}

fn metadata_fields(entry: &ZoteroEntry) -> Vec<(String, String)> {
    let mut fields = vec![
        ("Title".to_string(), entry.title.clone()),
        ("Authors".to_string(), entry.authors.clone()),
    ];
    if !entry.year.is_empty() {
        fields.push(("Year".to_string(), entry.year.clone()));
    }
    if !entry.publication.is_empty() {
        let mut pub_str = entry.publication.clone();
        let mut details = Vec::new();
        if !entry.volume.is_empty() { details.push(format!("Vol. {}", entry.volume)); }
        if !entry.issue.is_empty() { details.push(format!("No. {}", entry.issue)); }
        if !entry.pages.is_empty() { details.push(format!("pp. {}", entry.pages)); }
        if !details.is_empty() {
            pub_str.push_str(&format!(", {}", details.join(", ")));
        }
        fields.push(("Published in".to_string(), pub_str));
    }
    if !entry.doi.is_empty() {
        fields.push(("DOI".to_string(), entry.doi.clone()));
    }
    if !entry.url.is_empty() {
        fields.push(("URL".to_string(), entry.url.clone()));
    }
    fields.push(("File".to_string(), entry.pdf_path.display().to_string()));
    fields.push(("BibTeX".to_string(), entry.to_bibtex()));
    fields
}

fn copy_to_clipboard(text: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    // Try xclip, xsel, wl-copy in order
    let candidates = [
        ("xclip", &["-selection", "clipboard"] as &[&str]),
        ("xsel", &["--clipboard", "--input"]),
        ("wl-copy", &[]),
    ];
    for (cmd, args) in &candidates {
        if let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()?;
            return Ok(());
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no clipboard tool found"))
}

fn send_to_remarkable_cloud(path: &str) -> io::Result<String> {
    use std::process::Command;
    if !std::path::Path::new(path).exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    // Ensure /tui-pdf directory exists (ignore error if it already exists)
    let _ = Command::new("rmapi").args(["mkdir", "/tui-pdf"]).output();
    let output = Command::new("rmapi")
        .args(["put", path, "/tui-pdf/"])
        .output()
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("rmapi not found: {}", e)))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = if stderr.trim().is_empty() {
            format!("rmapi failed (exit code {})", output.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        };
        Err(io::Error::new(io::ErrorKind::Other, msg))
    }
}

/// Find the "tui-pdf" folder UUID on reMarkable via the USB web interface.
/// If not found, create it via rmapi and re-check.
fn remarkable_usb_folder_id() -> io::Result<String> {
    use std::process::Command;
    let find_folder = || -> Option<String> {
        let list = Command::new("curl")
            .args(["-sS", "-f", "--connect-timeout", "3", "http://10.11.99.1/documents/"])
            .output().ok()?;
        if !list.status.success() { return None; }
        let body = String::from_utf8_lossy(&list.stdout);
        let docs: Vec<serde_json::Value> = serde_json::from_str(&body).ok()?;
        for doc in &docs {
            if doc.get("Type").and_then(|t| t.as_str()) == Some("CollectionType")
                && doc.get("VissibleName").and_then(|n| n.as_str()) == Some("tui-pdf")
            {
                return doc.get("ID").and_then(|i| i.as_str()).map(|s| s.to_string());
            }
        }
        None
    };
    if let Some(id) = find_folder() {
        return Ok(id);
    }
    // Folder not on device yet — create via rmapi (cloud) and wait for sync
    let mkdir = Command::new("rmapi").args(["mkdir", "/tui-pdf"]).output();
    if mkdir.is_err() || !mkdir.unwrap().status.success() {
        return Err(io::Error::new(io::ErrorKind::Other,
            "tui-pdf folder not found; rmapi mkdir failed — create it on the device manually"));
    }
    // Give the device a moment to sync, then re-check
    std::thread::sleep(std::time::Duration::from_secs(3));
    find_folder().ok_or_else(|| io::Error::new(io::ErrorKind::Other,
        "created tui-pdf via cloud but device hasn't synced yet — try again shortly"))
}

fn send_to_remarkable(path: &str) -> io::Result<String> {
    use std::process::Command;
    if !std::path::Path::new(path).exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let folder_id = remarkable_usb_folder_id()?;
    // Navigate into the folder first — the device tracks "current directory"
    // server-side, and the next upload lands in whatever was last fetched.
    let nav = Command::new("curl")
        .args(["-sS", "-f", "--connect-timeout", "3",
               &format!("http://10.11.99.1/documents/{}", folder_id)])
        .output()?;
    if !nav.status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "failed to navigate to tui-pdf folder"));
    }
    let output = Command::new("curl")
        .args(["-sS", "-f", "--connect-timeout", "3",
               "http://10.11.99.1/upload",
               "-F", &format!("file=@\"{}\"", path)])
        .output()?;
    if output.status.success() {
        let body = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(body)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = if stderr.trim().is_empty() {
            format!("curl failed (exit code {})", output.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        };
        Err(io::Error::new(io::ErrorKind::Other, msg))
    }
}

/// Upload a single PDF to the device's `tui-pdf` folder over SSH (Developer
/// Mode). Named by Zotero title, opened at `page`, deduped by name, and the
/// device is refreshed so it appears. Errors if SSH isn't reachable.
fn send_to_remarkable_ssh(
    host: &str,
    path: &str,
    page: usize,
    zotero_dir: Option<&str>,
) -> io::Result<String> {
    use tui_pdf::remarkable as rm;
    rm::preflight(host)?;
    let mut index = rm::read_index(host)?;
    let root = ensure_collection(host, &mut index, "tui-pdf", "")?;
    let dev_name = device_display_name(path, zotero_dir);
    if index
        .iter()
        .any(|it| !it.is_collection && it.parent == root && it.visible_name == dev_name)
    {
        return Ok(format!("'{}' already on reMarkable", dev_name));
    }
    let page_count = Document::open(path)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("open PDF: {e}")))?
        .page_count();
    let page = page.min(page_count.saturating_sub(1));
    rm::upload_pdf(host, path, &dev_name, &root, page_count, page)?;
    rm::restart_xochitl(host)?;
    Ok(format!("Sent '{}' to reMarkable", dev_name))
}

/// Send the current PDF to the reMarkable, preferring SSH (Developer Mode) and
/// falling back to the USB web interface for devices not in Developer Mode.
fn send_one_to_remarkable(path: &str, page: usize, zotero_dir: Option<&str>) -> io::Result<String> {
    if !std::path::Path::new(path).exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let host = load_config().remarkable_host();
    if tui_pdf::remarkable::preflight(&host).is_ok() {
        send_to_remarkable_ssh(&host, path, page, zotero_dir)
    } else {
        send_to_remarkable(path)
    }
}

/// Display name for a document on the device: the Zotero title if the file is a
/// Zotero attachment, otherwise the file stem. Sanitized for use as a name.
fn device_display_name(path: &str, zotero_dir: Option<&str>) -> String {
    if let Some(dir) = zotero_dir {
        if let Some(entry) = lookup_by_path(std::path::Path::new(dir), std::path::Path::new(path)) {
            if !entry.title.trim().is_empty() {
                return sanitize_name(&entry.title);
            }
        }
    }
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    sanitize_name(&stem)
}

fn sanitize_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '/' || c.is_control() { ' ' } else { c })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True for documents we sync to the device: local PDFs only (skip URLs and
/// Markdown, which reMarkable can't open natively).
fn is_syncable_pdf(path: &str) -> bool {
    if is_url(path) {
        return false;
    }
    let lower = path.to_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        return false;
    }
    true
}

/// Convert a saved scroll (stripe) offset into a page index, replicating the
/// viewer's geometry (`PdfViewState::recompute_geometry`). Used to migrate
/// sessions that stored a scroll offset before page tracking existed.
fn scroll_to_page(source: &ContentSource, zoom: f32, font_height: u32, scroll: usize) -> usize {
    const PAGE_GAP: usize = 1; // matches widget.rs
    let n = source.page_count();
    let mut starts = Vec::with_capacity(n);
    let mut cumulative = 0usize;
    for i in 0..n {
        if i > 0 {
            cumulative += PAGE_GAP;
        }
        starts.push(cumulative);
        cumulative += source.compute_stripe_count(i, zoom, font_height).unwrap_or(1);
    }
    match starts.binary_search(&scroll) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

/// Find an existing child collection by name under `parent`, or create it.
fn ensure_collection(
    host: &str,
    index: &mut Vec<tui_pdf::remarkable::RmItem>,
    name: &str,
    parent: &str,
) -> io::Result<String> {
    if let Some(item) = index
        .iter()
        .find(|it| it.is_collection && it.parent == parent && it.visible_name == name)
    {
        return Ok(item.uuid.clone());
    }
    let uuid = tui_pdf::remarkable::create_collection(host, name, parent)?;
    index.push(tui_pdf::remarkable::RmItem {
        uuid: uuid.clone(),
        visible_name: name.to_string(),
        parent: parent.to_string(),
        is_collection: true,
        last_opened_page: 0,
        activity_ms: 0,
        last_modified_ms: 0,
    });
    Ok(uuid)
}

/// Pull a device document's annotations (if newer than our stored copy), render
/// their strokes into PDF-point coordinates, and store them locally keyed by
/// `uuid`. Returns the device's page count (from `.content`) when known, so the
/// caller can detect pages deleted on the tablet. Read-only on the device.
/// Parse a `.content` file into an ordered list of `(page_uuid, original_pdf_page)`.
/// Handles both the flat v1 format (`pages[]` + `redirectionPageMap`) and the v2
/// `cPages` format used by newer firmware / Paper Pro (`cPages.pages[].id` and
/// `.redir.value`). `redir` is `None` for pages with no PDF backing (inserted /
/// notebook pages).
fn content_pages(content: &serde_json::Value) -> Vec<(String, Option<i64>)> {
    // v2: cPages.pages[] with per-page id + redir.
    if let Some(pages) = content
        .get("cPages")
        .and_then(|c| c.get("pages"))
        .and_then(|p| p.as_array())
    {
        return pages
            .iter()
            .map(|pg| {
                let id = pg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let redir = pg.get("redir").and_then(|r| r.get("value")).and_then(|v| v.as_i64());
                (id, redir)
            })
            .collect();
    }
    // v1: flat pages[] (uuids) + redirectionPageMap (ints; -1 = inserted).
    let pages: Vec<String> = content
        .get("pages")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let redirect: Vec<i64> = content
        .get("redirectionPageMap")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default();
    pages
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), redirect.get(i).copied().filter(|&n| n >= 0)))
        .collect()
}

fn pull_and_store_annotations(
    host: &str,
    uuid: &str,
    device_last_modified_ms: i64,
    source: &ContentSource,
    device_width: f32,
    device_height: f32,
) -> io::Result<(Option<usize>, Option<String>)> {
    use tui_pdf::remarkable as rm;
    use tui_pdf::rm_lines;

    let content_text = rm::rm_read_file(host, &format!("{}/{}.content", rm::XOCHITL_DIR, uuid))?;
    let content: serde_json::Value = serde_json::from_str(content_text.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad .content: {e}")))?;
    let is_notebook = content.get("fileType").and_then(|v| v.as_str()) == Some("notebook");
    // "Adjust view" (zoomMode: customFit) fits the PDF to a custom zoom, and
    // strokes are recorded against that page span rather than the device width.
    // Capture it so the overlay transform can normalize by the right width.
    let custom_zoom_page_width = if content.get("zoomMode").and_then(|v| v.as_str())
        == Some("customFit")
    {
        content
            .get("customZoomPageWidth")
            .and_then(|v| v.as_f64())
            .map(|w| w as f32)
            .filter(|w| *w > 0.0)
    } else {
        None
    };
    let page_list = content_pages(&content);
    let device_page_count = content
        .get("pageCount")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or(if page_list.is_empty() { None } else { Some(page_list.len()) });

    // Ordinals with no PDF backing (inserted blank pages). A PDF with inserted
    // pages is rendered from a *merged* backing PDF (original pages + blanks) so
    // the notes appear in position; annotations then map by ordinal. Notebooks
    // already have a blank backing PDF from the caller.
    let inserted: Vec<usize> = page_list
        .iter()
        .enumerate()
        .filter(|(_, (_, r))| r.is_none())
        .map(|(i, _)| i)
        .collect();
    let merge = !is_notebook && !inserted.is_empty();
    let merged_path = tui_pdf::config::notebooks_dir().join(uuid).join("merged.pdf");
    let render_path = if merge {
        Some(merged_path.to_string_lossy().to_string())
    } else {
        None
    };

    // Skip re-pulling if our stored copy is current (still report render_path so
    // the caller keeps pointing at the merged PDF).
    if device_last_modified_ms > 0 {
        if let Some(existing) = rm_lines::load(uuid) {
            if existing.last_modified_ms >= device_last_modified_ms && (!merge || merged_path.exists())
            {
                return Ok((device_page_count, render_path));
            }
        }
    }

    // Pull the `<uuid>/` directory of `.rm` files (scp -r drops them under
    // `<tmp>/<uuid>/`).
    let tmp = std::env::temp_dir().join(format!("tui-pdf-rm-{uuid}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    rm::rm_scp_from(host, &format!("{}/{}", rm::XOCHITL_DIR, uuid), &tmp.to_string_lossy())?;

    let rm_dir = tmp.join(uuid);
    let orig_page_count = source.page_count();
    let mut raw_strokes: Vec<tui_pdf::rm_lines::RawPageStroke> = Vec::new();
    if let Ok(entries) = fs::read_dir(&rm_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rm") {
                continue; // skip per-page -metadata.json etc.
            }
            let page_uuid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let dev_idx = match page_list.iter().position(|(id, _)| id == &page_uuid) {
                Some(i) => i,
                None => continue,
            };
            // Target page in the RENDERED doc, and whether that page is blank.
            let (target_page, page_backing_less) = if is_notebook {
                (dev_idx, true)
            } else if merge {
                (dev_idx, page_list[dev_idx].1.is_none())
            } else {
                match page_list[dev_idx].1 {
                    Some(n) if (n as usize) < orig_page_count => (n as usize, false),
                    _ => continue,
                }
            };
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let raw = match rm_lines::parse_rm_v6(&bytes) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for s in raw {
                if s.pts.len() < 2 {
                    continue;
                }
                raw_strokes.push(tui_pdf::rm_lines::RawPageStroke {
                    page: target_page,
                    pts: s.pts,
                    color_id: s.color_id,
                    tool_id: s.tool_id,
                    thickness: s.thickness,
                    backing_less: page_backing_less,
                });
            }
        }
    }

    let ann = rm_lines::DocAnnotations {
        last_modified_ms: device_last_modified_ms,
        device_width,
        custom_zoom_page_width,
        raw: raw_strokes,
    };
    rm_lines::save(uuid, &ann)?;

    // Build the merged backing PDF (original pages + blank inserts).
    if merge {
        let orig = source.path_or_url().to_string();
        if let Ok(orig_bytes) = fs::read(&orig) {
            let (w_pt, h_pt) = backing_page_size(device_width, device_height);
            match rm_lines::build_merged_pdf(&orig_bytes, &inserted, w_pt, h_pt) {
                Ok(bytes) => {
                    if let Some(parent) = merged_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::write(&merged_path, bytes);
                }
                Err(e) => eprintln!("  warning: merge failed for {uuid}: {e}"),
            }
        }
    }

    let _ = fs::remove_dir_all(&tmp);
    Ok((device_page_count, render_path))
}

/// Size in PDF points for a device-only ("blank") reMarkable page — a notebook
/// page or a page inserted into a PDF on the tablet. The *absolute* size carries
/// no meaning: there is no true page, strokes are placed proportionally, and the
/// page is normalized to the viewport when rendered. Only the device's width:height
/// **aspect** matters, so we anchor the height to a neutral reference and derive
/// the width from the aspect — no device DPI needed. Keeping the aspect exact also
/// keeps the annotation transform's in-frame check (`dh = page_h * dw / page_w`)
/// equal to the true device height.
fn backing_page_size(device_width: f32, device_height: f32) -> (f32, f32) {
    /// Neutral reference height (US Letter). Arbitrary — only the ratio below matters.
    const REF_HEIGHT_PT: f32 = 792.0;
    let aspect = if device_height > 0.0 {
        device_width / device_height
    } else {
        0.75 // reMarkable devices are ~3:4; only used if the device reported nothing
    };
    (REF_HEIGHT_PT * aspect, REF_HEIGHT_PT)
}

/// Find notebooks/quick sheets in `folder_uuid` on the device that aren't yet in
/// the session, render a blank backing PDF for each, pull their strokes, and add
/// them to `session`. Returns how many were added. Read-only on the device.
fn pull_notebooks_in_folder(
    host: &str,
    folder_uuid: &str,
    index: &[tui_pdf::remarkable::RmItem],
    annotated: &std::collections::HashSet<String>,
    device_width: f32,
    device_height: f32,
    session: &mut Session,
) -> u32 {
    use tui_pdf::remarkable as rm;

    let existing: std::collections::HashSet<String> = session
        .docs
        .iter()
        .filter_map(|d| d.remarkable_uuid.clone())
        .collect();

    let mut added = 0u32;
    for item in index.iter().filter(|it| {
        !it.is_collection && it.parent == folder_uuid && !existing.contains(&it.uuid)
    }) {
        // Only notebooks (a device-added PDF is a separate feature).
        let content_text =
            match rm::rm_read_file(host, &format!("{}/{}.content", rm::XOCHITL_DIR, item.uuid)) {
                Ok(t) => t,
                Err(_) => continue,
            };
        let content: serde_json::Value = match serde_json::from_str(content_text.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if content.get("fileType").and_then(|v| v.as_str()) != Some("notebook") {
            continue;
        }
        let page_count = content
            .get("pageCount")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                content
                    .get("pages")
                    .and_then(|p| p.as_array())
                    .map(|a| a.len() as u64)
            })
            .unwrap_or(1) as usize;

        // Blank backing PDF, sized to the device page aspect.
        let (w_pt, h_pt) = backing_page_size(device_width, device_height);
        let name = sanitize_name(&item.visible_name);
        let name = if name.trim().is_empty() { "notebook".to_string() } else { name };
        let pdf_path = tui_pdf::config::notebooks_dir()
            .join(&item.uuid)
            .join(format!("{name}.pdf"));
        if let Err(e) = tui_pdf::rm_lines::write_blank_pdf(&pdf_path, page_count, w_pt, h_pt) {
            eprintln!("  warning: could not create notebook '{}': {e}", item.visible_name);
            continue;
        }

        // Pull its strokes (if any) so they show on first open.
        if annotated.contains(&item.uuid) {
            if let Ok(src) = Document::open(&pdf_path).map(ContentSource::Pdf) {
                let _ = pull_and_store_annotations(
                    host,
                    &item.uuid,
                    item.last_modified_ms,
                    &src,
                    device_width,
                    device_height,
                );
            }
        }

        session.docs.push(tui_pdf::SessionDoc {
            path: pdf_path.to_string_lossy().to_string(),
            scroll: 0,
            zoom: 1.0,
            page: Some(item.last_opened_page.max(0) as usize),
            modified: item.activity_ms / 1000,
            remarkable_uuid: Some(item.uuid.clone()),
            render_path: None, // a notebook's blank PDF *is* its `path`
        });
        added += 1;
    }
    added
}

/// Sync every saved session to/from the reMarkable over SSH. For each session:
/// create a `tui-pdf/<session>` folder, upload missing PDFs (named by Zotero
/// title), never re-upload existing ones (annotation-safe), and reconcile the
/// reading position with latest-time-wins.
fn sync_sessions(only: &[String], host_override: Option<&str>) -> io::Result<()> {
    use tui_pdf::remarkable as rm;

    let config = load_config();
    let host = host_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| config.remarkable_host());
    let zotero_dir = config.zotero_dir.clone();

    rm::preflight(&host)?;
    println!("Connected to reMarkable at {host}");

    let mut sessions = list_sessions();
    if !only.is_empty() {
        // Restrict to the named sessions; fail loudly if any don't exist.
        for name in only {
            if !sessions.iter().any(|s| s == name) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("session '{name}' not found"),
                ));
            }
        }
        sessions.retain(|s| only.iter().any(|n| n == s));
    }
    if sessions.is_empty() {
        println!("No saved sessions to sync.");
        return Ok(());
    }

    // Terminal font height, so we can convert a scroll offset → page the same
    // way the viewer does (for sessions that predate page tracking).
    let font_height = Picker::from_query_stdio()
        .unwrap_or_else(|_| Picker::halfblocks())
        .font_size()
        .height as u32;

    // NB: we never stop xochitl — that blanks the device and breaks the USB
    // link mid-sync. We write sidecars while it runs and restart it once at the
    // end (only if something changed) so new documents appear.
    let mut index = rm::read_index(&host)?;
    let mut device_changed = false;
    // UUIDs that carry handwritten annotations (one round-trip for the store).
    let annotated = rm::list_annotated_uuids(&host).unwrap_or_default();
    // Device stroke-canvas size, queried once (used for the annotation transform
    // and for sizing generated notebook pages).
    let (device_width, device_height) = rm::device_stroke_size(&host);

    let root_uuid = ensure_collection(&host, &mut index, "tui-pdf", "")?;

    for name in &sessions {
        let mut session = match load_session(name) {
            Some(s) => s,
            None => continue,
        };
        let folder_uuid = ensure_collection(&host, &mut index, name, &root_uuid)?;

        let (mut uploaded, mut existing, mut pushed, mut pulled) = (0u32, 0u32, 0u32, 0u32);
        let mut pulled_ann = 0u32;
        let mut changed = false;

        for doc in &mut session.docs {
            let path = doc.path.clone();
            if !is_syncable_pdf(&path) {
                continue;
            }
            if !std::path::Path::new(&path).exists() {
                continue;
            }
            let dev_name = device_display_name(&path, zotero_dir.as_deref());

            // Open the PDF for its page count and, for sessions that predate
            // page tracking, to convert the saved scroll offset into a page.
            let source = match Document::open(&path) {
                Ok(d) => ContentSource::Pdf(d),
                Err(_) => continue,
            };
            let page_count = source.page_count();
            if page_count == 0 {
                continue;
            }
            let last_page = page_count - 1;
            let was_legacy = doc.page.is_none();
            let computer_page = match doc.page {
                Some(p) => p.min(last_page),
                None => scroll_to_page(&source, doc.zoom, font_height, doc.scroll).min(last_page),
            };

            // Identify the device document: stored UUID first, else match an
            // existing document by name within this session's folder.
            let dev_uuid = doc
                .remarkable_uuid
                .clone()
                .filter(|u| index.iter().any(|it| &it.uuid == u && !it.is_collection))
                .or_else(|| {
                    index
                        .iter()
                        .find(|it| {
                            !it.is_collection
                                && it.parent == folder_uuid
                                && it.visible_name == dev_name
                        })
                        .map(|it| it.uuid.clone())
                });

            match dev_uuid {
                None => {
                    let new_uuid = rm::upload_pdf(
                        &host, &path, &dev_name, &folder_uuid, page_count, computer_page,
                    )?;
                    doc.remarkable_uuid = Some(new_uuid.clone());
                    doc.page = Some(computer_page);
                    doc.modified = rm::now_secs();
                    changed = true;
                    uploaded += 1;
                    index.push(rm::RmItem {
                        uuid: new_uuid,
                        visible_name: dev_name,
                        parent: folder_uuid.clone(),
                        is_collection: false,
                        last_opened_page: computer_page as i64,
                        activity_ms: rm::now_ms(),
                        last_modified_ms: rm::now_ms(),
                    });
                }
                Some(u) => {
                    existing += 1;
                    if doc.remarkable_uuid.as_deref() != Some(u.as_str()) {
                        doc.remarkable_uuid = Some(u.clone());
                        changed = true;
                    }
                    let item = match index.iter().find(|it| it.uuid == u).cloned() {
                        Some(it) => it,
                        None => continue,
                    };
                    let device_ms = item.activity_ms;
                    let device_page = item.last_opened_page.max(0) as usize;
                    let computer_ms = doc.modified.saturating_mul(1000);

                    // Latest-wins — except a legacy session being migrated always
                    // pushes its (scroll-derived) page so its position isn't lost.
                    if !was_legacy && device_ms > computer_ms {
                        // Device is newer → pull the page. On restore the viewer
                        // drops to the top of that page.
                        if doc.page != Some(device_page) {
                            doc.page = Some(device_page);
                            doc.modified = device_ms / 1000;
                            changed = true;
                            pulled += 1;
                        }
                    } else {
                        // Computer wins → push the page to the device.
                        if device_page != computer_page {
                            rm::set_position(&host, &u, computer_page)?;
                            pushed += 1;
                        }
                        if doc.page != Some(computer_page) {
                            doc.page = Some(computer_page);
                            changed = true;
                        }
                        if was_legacy {
                            doc.modified = rm::now_secs();
                            changed = true;
                        }
                    }

                    // Scribbles: the reMarkable is authoritative. Pull them down
                    // (read-only on the device) when the doc carries annotations.
                    if annotated.contains(&u) {
                        match pull_and_store_annotations(
                            &host, &u, item.last_modified_ms, &source, device_width, device_height,
                        ) {
                            Ok((dev_pages, render_path)) => {
                                pulled_ann += 1;
                                // A PDF with inserted pages is displayed from a
                                // merged backing PDF; keep the original as `path`
                                // (sync source of truth) and point the viewer at
                                // the merged copy via `render_path`.
                                if doc.render_path != render_path {
                                    doc.render_path = render_path;
                                    changed = true;
                                }
                                // Edge case: if the page we were on was deleted on
                                // the tablet, fall back to the device's position.
                                if let Some(dpc) = dev_pages {
                                    if computer_page >= dpc && dpc > 0 {
                                        let dp = device_page.min(dpc - 1);
                                        if doc.page != Some(dp) {
                                            doc.page = Some(dp);
                                            doc.modified = rm::now_secs();
                                            changed = true;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("  warning: annotation pull for '{name}' failed: {e}")
                            }
                        }
                    }
                }
            }
        }

        // Pull notebooks / quick sheets created directly in this session's folder
        // on the device. They have no PDF of their own, so we render a blank
        // backing PDF (like Markdown is rendered to a PDF) and add it to the
        // session; the strokes overlay via the annotation path on open.
        let notebooks_added = pull_notebooks_in_folder(
            &host,
            &folder_uuid,
            &index,
            &annotated,
            device_width,
            device_height,
            &mut session,
        );
        if notebooks_added > 0 {
            changed = true;
        }

        if uploaded > 0 || pushed > 0 {
            device_changed = true;
        }
        if changed {
            if let Err(e) = save_session(name, &session) {
                eprintln!("  warning: could not update session '{name}': {e}");
            }
        }
        println!(
            "  {name}: uploaded {uploaded}, existing {existing}, pushed {pushed}, pulled {pulled}, annotated {pulled_ann}, notebooks {notebooks_added}"
        );
    }

    if device_changed {
        println!("Refreshing reMarkable (the screen will blank briefly)...");
        rm::restart_xochitl(&host)?;
    }
    Ok(())
}

/// Copy every saved session's documents into `<dir>/<session>/`, mirroring the
/// layout `--sync-sessions` creates on the reMarkable: one folder per session,
/// each PDF named by its Zotero title. Files already present are left untouched
/// (so annotations made in the target folder survive) and nothing is deleted.
/// Unlike the reMarkable sync there is no reading position or annotation to
/// reconcile — a plain directory carries no such metadata — so this is one-way.
fn sync_sessions_to_directory(dir: &str, only: &[String]) -> io::Result<()> {
    let config = load_config();
    let zotero_dir = config.zotero_dir.clone();

    let root = std::path::Path::new(dir);
    std::fs::create_dir_all(root)
        .map_err(|e| io::Error::new(e.kind(), format!("cannot create {dir}: {e}")))?;

    let mut sessions = list_sessions();
    if !only.is_empty() {
        // Restrict to the named sessions; fail loudly if any don't exist.
        for name in only {
            if !sessions.iter().any(|s| s == name) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("session '{name}' not found"),
                ));
            }
        }
        sessions.retain(|s| only.iter().any(|n| n == s));
    }
    if sessions.is_empty() {
        println!("No saved sessions to sync.");
        return Ok(());
    }
    println!("Syncing to {}", root.display());

    for name in &sessions {
        let session = match load_session(name) {
            Some(s) => s,
            None => continue,
        };
        let folder = root.join(sanitize_name(name));
        std::fs::create_dir_all(&folder)
            .map_err(|e| io::Error::new(e.kind(), format!("cannot create {}: {e}", folder.display())))?;

        let (mut copied, mut existing, mut skipped) = (0u32, 0u32, 0u32);
        // Names claimed during this run, so two documents whose Zotero titles
        // collide don't silently overwrite (or hide behind) each other.
        let mut claimed: Vec<String> = Vec::new();

        for doc in &session.docs {
            if !is_syncable_pdf(&doc.path) {
                continue;
            }
            let src = std::path::Path::new(&doc.path);
            if !src.exists() {
                skipped += 1;
                continue;
            }
            let ext = src
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_else(|| ".pdf".to_string());
            let stem = device_display_name(&doc.path, zotero_dir.as_deref());
            let mut file_name = format!("{stem}{ext}");
            let mut n = 2;
            while claimed.iter().any(|c| c == &file_name) {
                file_name = format!("{stem} ({n}){ext}");
                n += 1;
            }
            claimed.push(file_name.clone());

            let dest = folder.join(&file_name);
            if dest.exists() {
                existing += 1;
                continue;
            }
            match std::fs::copy(src, &dest) {
                Ok(_) => copied += 1,
                Err(e) => {
                    eprintln!("  warning: copying '{}' failed: {e}", src.display());
                    skipped += 1;
                }
            }
        }
        println!("  {name}: copied {copied}, existing {existing}, skipped {skipped}");
    }
    Ok(())
}

/// Ask the terminal to delete all kitty images and free their data.
/// Stripe protocols transmit image data once and never delete it, so protocols
/// dropped on document switches and previews leak terminal memory until kitty
/// starts evicting images that may still be on screen. Other terminals ignore
/// the sequence.
fn free_kitty_images() {
    let mut out = stdout();
    let _ = out.write_all(b"\x1b_Ga=d,d=A\x1b\\");
    let _ = out.flush();
}

enum AppAction {
    Quit,
    OpenZotero,
    SwitchDoc(usize),
    CloseDoc,
    OpenLatest,
    /// Open a PDF (or other document) by browsing the filesystem
    OpenFile,
    /// Temporary URL preview (from metadata view) — not added to open docs
    PreviewUrl(String),
}

struct ProbeCell {
    number: usize,
    page: usize,
    pdf_x: f32,
    pdf_y: f32,
    file: String,
    line: usize,
}

/// A source position projected for the reverse-search probe grid:
/// `(page, pdf_x, pdf_y, file, line)`.
type ProbePos = (usize, f32, f32, String, usize);

fn compute_probe_grid(
    pdf_state: &PdfViewState,
    source: &ContentSource,
    _area: ratatui::layout::Rect,
) -> Vec<ProbeCell> {
    // Gather all source positions as (page, x, y, file, line). Markdown uses
    // the in-memory source map (x at the left margin); LaTeX parses synctex.
    let positions: Vec<ProbePos> = if source.is_markdown() {
        let file = source.path_or_url().to_string();
        source
            .as_document()
            .map(|d| {
                d.md_positions()
                    .iter()
                    .map(|l| (l.page, 60.0_f32, l.y, file.clone(), l.line))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        synctex_positions(std::path::Path::new(source.path_or_url()))
            .into_iter()
            .map(|p| (p.page, p.x, p.y, p.file, p.line))
            .collect()
    };

    // Collect visible positions with their terminal coordinates
    let mut visible: Vec<(u16, u16, &ProbePos)> = Vec::new();
    for pos in &positions {
        if let Some((row, col)) = pdf_state.pdf_to_terminal(pos.0, pos.1, pos.2) {
            visible.push((row, col, pos));
        }
    }

    // Sort by visual reading order: top-to-bottom, then left-to-right,
    // then by source line (smallest first) so dedup keeps the paragraph start
    visible.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2 .4.cmp(&b.2 .4)));

    // Deduplicate by terminal cell — keep only one probe per screen position
    let mut seen_cells = std::collections::HashSet::new();
    let mut cells = Vec::new();
    let mut number = 1;

    for (row, col, pos) in &visible {
        if !seen_cells.insert((*row, *col)) {
            continue;
        }
        cells.push(ProbeCell {
            number,
            page: pos.0,
            pdf_x: pos.1,
            pdf_y: pos.2,
            file: pos.3.clone(),
            line: pos.4,
        });
        number += 1;
    }
    cells
}

struct OpenDoc {
    path: String,
    scroll: usize,
    zoom: f32,
    page: Option<usize>,
    modified: i64,
    remarkable_uuid: Option<String>,
    /// View-only backing PDF (merged / notebook blank) if one was generated.
    render_path: Option<String>,
}

fn print_help() {
    println!("tui-pdf — a terminal PDF viewer with image rendering

USAGE:
    tui-pdf [OPTIONS] <pdf|url>...

ARGUMENTS:
    <pdf|url>...                One or more PDF files or URLs to open

OPTIONS:
    -h, --help                  Show this help message
    --session <name>            Restore a saved session by name
    --list-sessions             List all saved sessions
    --sync-sessions [--ip <addr>] [name...]
                                Sync all sessions (or only the named ones) to/from
                                a reMarkable. Defaults to USB (10.11.99.1); pass
                                --ip <addr> to sync over WiFi
    --sync-session-to-directory <dir> [name...]
                                Copy all sessions (or only the named ones) into
                                <dir>/<session>/, named by Zotero title. Existing
                                files are left untouched; one-way, nothing deleted
    --zotero                    Browse Zotero library and open a PDF
    --setup-zotero <dir>        Configure Zotero data directory (one-time)
    --move-sessions <dir>       Move session storage to a custom directory
    --completions <shell>       Generate shell completions (bash, fish, zsh)
    --forward <line:col:file> <pdf>
                                Send forward search to a running instance

KEYBINDINGS:
    j/k, Up/Down                Scroll up/down
    n/p, Right/Left, PgDn/PgUp  Next/previous page
    Home/End                    First/last page
    g                           Go to page number
    +/- or =/−                  Zoom in/out
    w                           Fit to width
    /                           Search text (n/p: next/prev match)
    t                           Toggle table of contents
    l                           Enter link mode (j/k: select, Enter: follow)
    b                           Go back after following a link
    i                           Toggle color inversion
    a                           Toggle reMarkable annotation overlay (if any)
    m                           Show Zotero metadata for current PDF
    s                           SyncTeX probe (keyboard reverse search)
    o                           Open Zotero browser
    O                           Open latest Zotero PDF
    e                           Open a file from the filesystem (file browser)
    R                           Send PDF to reMarkable over USB (SSH in Developer
                                Mode, else the USB web interface)
    C                           Send PDF to reMarkable cloud (rmapi)
    S                           Save session (prompts for name first time)
    d                           Document picker
    Tab/Shift+Tab               Cycle between open documents
    x                           Close current document
    q/Esc                       Quit
    Mouse wheel                 Scroll
    Ctrl+Click                  SyncTeX reverse search");
}

fn print_completions_bash() {
    print!(r#"_tui_pdf() {{
    local cur prev opts
    COMPREPLY=()
    cur="${{COMP_WORDS[COMP_CWORD]}}"
    prev="${{COMP_WORDS[COMP_CWORD-1]}}"
    opts="--help --session --list-sessions --sync-sessions --sync-session-to-directory --zotero --setup-zotero --move-sessions --forward --completions"

    case "$prev" in
        --session)
            local sessions
            sessions=$(tui-pdf --list-sessions 2>/dev/null | grep '^ ' | sed 's/^ *//' | cut -d' ' -f1)
            COMPREPLY=( $(compgen -W "$sessions" -- "$cur") )
            return 0
            ;;
        --setup-zotero|--move-sessions|--sync-session-to-directory)
            COMPREPLY=( $(compgen -d -- "$cur") )
            return 0
            ;;
        --completions)
            COMPREPLY=( $(compgen -W "bash fish zsh" -- "$cur") )
            return 0
            ;;
    esac

    # --sync-sessions and --sync-session-to-directory take session names
    if [[ (" ${{COMP_WORDS[*]}} " == *" --sync-sessions "* \
        || " ${{COMP_WORDS[*]}} " == *" --sync-session-to-directory "*) && "$cur" != -* ]]; then
        local sessions
        sessions=$(tui-pdf --list-sessions 2>/dev/null | grep '^ ' | sed 's/^ *//' | cut -d' ' -f1)
        COMPREPLY=( $(compgen -W "$sessions" -- "$cur") )
        return 0
    fi

    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
    else
        COMPREPLY=( $(compgen -f -X '!*.pdf' -- "$cur") $(compgen -d -- "$cur") )
    fi
}}
complete -F _tui_pdf tui-pdf
"#);
}

fn print_completions_fish() {
    print!(r#"complete -c tui-pdf -l help -s h -d 'Show help message'
complete -c tui-pdf -l session -x -d 'Restore a saved session' -a '(tui-pdf --list-sessions 2>/dev/null | string match -r "^  \S+" | string trim)'
complete -c tui-pdf -l list-sessions -d 'List all saved sessions'
complete -c tui-pdf -l sync-sessions -d 'Sync all sessions to/from a connected reMarkable'
complete -c tui-pdf -n '__fish_seen_argument -l sync-sessions' -d 'Session to sync' -a '(tui-pdf --list-sessions 2>/dev/null | string match -r "^  \S+" | string trim)'
complete -c tui-pdf -l sync-session-to-directory -r -F -d 'Copy sessions into <dir>/<session>/'
complete -c tui-pdf -n '__fish_seen_argument -l sync-session-to-directory' -d 'Session to copy' -a '(tui-pdf --list-sessions 2>/dev/null | string match -r "^  \S+" | string trim)'
complete -c tui-pdf -l zotero -d 'Browse Zotero library'
complete -c tui-pdf -l setup-zotero -r -F -d 'Configure Zotero data directory'
complete -c tui-pdf -l move-sessions -r -F -d 'Move session storage directory'
complete -c tui-pdf -l forward -x -d 'Send forward search to running instance'
complete -c tui-pdf -l completions -x -d 'Generate shell completions' -a 'bash fish zsh'
complete -c tui-pdf -a '(__fish_complete_suffix .pdf)'
"#);
}

fn print_completions_zsh() {
    print!(r#"#compdef tui-pdf

_tui-pdf() {{
    local -a sessions
    _arguments -s \
        '(-h --help)'{{'{{-h,--help}}'}}'[Show help message]' \
        '--session[Restore a saved session]:session name:->sessions' \
        '--list-sessions[List all saved sessions]' \
        '--sync-sessions[Sync all sessions to/from a connected reMarkable]:*:session name:->sessions' \
        '--sync-session-to-directory[Copy sessions into <dir>/<session>/]:directory:_directories:*:session name:->sessions' \
        '--zotero[Browse Zotero library]' \
        '--setup-zotero[Configure Zotero data directory]:directory:_directories' \
        '--move-sessions[Move session storage directory]:directory:_directories' \
        '--forward[Send forward search]:spec:' \
        '--completions[Generate shell completions]:shell:(bash fish zsh)' \
        '*:PDF file:_files -g "*.pdf"'

    case "$state" in
        sessions)
            sessions=(${{(f)"$(tui-pdf --list-sessions 2>/dev/null | grep '^ ' | sed 's/^ *//' | cut -d' ' -f1)"}})
            compadd -a sessions
            ;;
    esac
}}

_tui-pdf "$@"
"#);
}

fn main() -> io::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}\n\nBacktrace:\n{}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write("/tmp/tui-pdf-panic.log", &msg);
    }));

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        print_help();
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    // Handle --completions
    if args.len() >= 3 && args[1] == "--completions" {
        match args[2].as_str() {
            "bash" => print_completions_bash(),
            "fish" => print_completions_fish(),
            "zsh" => print_completions_zsh(),
            other => {
                eprintln!("Unknown shell: {}. Supported: bash, fish, zsh", other);
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    // Relocate annotation/notebook storage into the sessions dir if it's still
    // at the old config-dir location (one-time, no-op once moved).
    tui_pdf::config::migrate_storage_into_sessions_dir();

    // Handle --list-sessions
    if args[1] == "--list-sessions" {
        let sessions = list_sessions();
        if sessions.is_empty() {
            println!("No saved sessions.");
        } else {
            println!("Saved sessions:");
            for name in &sessions {
                if let Some(sess) = load_session(name) {
                    println!("  {} ({} doc{})", name, sess.docs.len(), if sess.docs.len() == 1 { "" } else { "s" });
                    for doc in &sess.docs {
                        let short = std::path::Path::new(&doc.path)
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| doc.path.clone());
                        println!("    {}", short);
                    }
                } else {
                    println!("  {}", name);
                }
            }
        }
        std::process::exit(0);
    }

    // Handle --forward: send command to running instance and exit
    if args.len() >= 4 && args[1] == "--forward" {
        let pdf_path = std::path::Path::new(&args[3]);
        let sock = socket_path(pdf_path);
        let message = format!("forward:{}", args[2]);
        if send_forward(&sock, &message) {
            std::process::exit(0);
        } else {
            eprintln!("No running tui-pdf instance for this PDF");
            std::process::exit(1);
        }
    }

    // Handle --setup-zotero: save Zotero directory to config
    if args.len() >= 3 && args[1] == "--setup-zotero" {
        let dir = &args[2];
        let path = std::path::Path::new(dir);
        if !path.join("zotero.sqlite").exists() {
            eprintln!("Error: {}/zotero.sqlite not found", dir);
            std::process::exit(1);
        }
        let mut config = load_config();
        config.zotero_dir = Some(dir.to_string());
        save_config(&config).unwrap_or_else(|e| {
            eprintln!("Failed to save config: {e}");
            std::process::exit(1);
        });
        eprintln!("Zotero directory saved. You can now use: tui-pdf --zotero");
        std::process::exit(0);
    }

    // Handle --move-sessions: move session storage to a custom directory
    if args.len() >= 3 && args[1] == "--move-sessions" {
        let dir = &args[2];
        move_sessions_dir(dir).unwrap_or_else(|e| {
            eprintln!("Failed to move sessions: {e}");
            std::process::exit(1);
        });
        println!("Sessions moved to {}", dir);
        std::process::exit(0);
    }

    // Handle --session: restore a named session
    if args.len() >= 3 && args[1] == "--session" {
        let name = &args[2];
        match load_session(name) {
            Some(session) if !session.docs.is_empty() => {
                let paths: Vec<String> = session.docs.iter().map(|d| d.path.clone()).collect();
                let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
                return open_viewer(&refs, Some(name.clone()), Some(&session));
            }
            _ => {
                eprintln!("Session '{}' not found or empty", name);
                std::process::exit(1);
            }
        }
    }

    // Handle --sync-sessions [--ip <addr>] [name...]: sync all saved sessions,
    // or only the named ones if any are listed, to/from the reMarkable. `--ip`
    // overrides the SSH host (default `10.11.99.1`, the USB address) so the
    // sync can run over WiFi.
    if args[1] == "--sync-sessions" {
        let rest = &args[2..];
        let mut host_override: Option<String> = None;
        let mut only: Vec<String> = Vec::new();
        let mut i = 0;
        while i < rest.len() {
            let a = &rest[i];
            if a == "--ip" {
                match rest.get(i + 1) {
                    Some(addr) => {
                        host_override = Some(addr.clone());
                        i += 2;
                    }
                    None => {
                        eprintln!("--ip requires an address, e.g. --ip 192.168.1.42");
                        std::process::exit(1);
                    }
                }
            } else if let Some(addr) = a.strip_prefix("--ip=") {
                host_override = Some(addr.to_string());
                i += 1;
            } else {
                only.push(a.clone());
                i += 1;
            }
        }
        match sync_sessions(&only, host_override.as_deref()) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("sync failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // Handle --sync-session-to-directory <dir> [name...]: copy all saved
    // sessions, or only the named ones, into <dir>/<session>/. Same document
    // selection and naming as --sync-sessions, but into a plain folder.
    if args[1] == "--sync-session-to-directory" || args[1] == "--sync-sessions-to-directory" {
        let dir = match args.get(2) {
            Some(d) if !d.starts_with('-') => d.clone(),
            _ => {
                eprintln!("{} requires a target directory, e.g. {} ~/tablet", args[1], args[1]);
                std::process::exit(1);
            }
        };
        let only: Vec<String> = args[3..].to_vec();
        match sync_sessions_to_directory(&dir, &only) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("sync failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // Handle --zotero: browse Zotero library and open selected PDF
    if args.len() >= 2 && args[1] == "--zotero" {
        let config = load_config();
        let zotero_dir = config.zotero_dir.unwrap_or_else(|| {
            eprintln!("No Zotero directory configured. Run: tui-pdf --setup-zotero <dir>");
            std::process::exit(1);
        });
        let library = load_library(std::path::Path::new(&zotero_dir)).unwrap_or_else(|e| {
            eprintln!("Failed to load Zotero library: {e}");
            std::process::exit(1);
        });
        if library.entries.is_empty() {
            eprintln!("No PDF entries found in Zotero library.");
            std::process::exit(1);
        }
        match run_zotero_browser(&library) {
            Ok(Some(pdf_path)) => {
                let s = pdf_path.to_string_lossy().to_string();
                return open_viewer(&[&s], None, None);
            }
            Ok(None) => std::process::exit(0),
            Err(e) => {
                eprintln!("Browser error: {e}");
                std::process::exit(1);
            }
        }
    }

    // Collect all remaining args as PDF paths, resolving .tex files to their PDFs
    let pdf_paths: Vec<String> = args[1..].iter().map(|s| resolve_tex_to_pdf(s)).collect();
    let pdf_refs: Vec<&str> = pdf_paths.iter().map(|s| s.as_str()).collect();
    open_viewer(&pdf_refs, None, None)
}

fn open_viewer(pdf_paths: &[&str], session_name: Option<String>, session: Option<&Session>) -> io::Result<()> {
    let mut open_docs: Vec<OpenDoc> = if let Some(sess) = session {
        sess.docs.iter().map(|d| OpenDoc {
            path: d.path.clone(),
            scroll: d.scroll,
            zoom: d.zoom,
            page: d.page,
            modified: d.modified,
            remarkable_uuid: d.remarkable_uuid.clone(),
            render_path: d.render_path.clone(),
        }).collect()
    } else {
        pdf_paths.iter().map(|p| OpenDoc {
            path: p.to_string(),
            scroll: 0,
            zoom: 1.0,
            page: None,
            modified: 0,
            remarkable_uuid: None,
            render_path: None,
        }).collect()
    };
    let mut current_idx: usize = session.map_or(0, |s| s.current.min(open_docs.len().saturating_sub(1)));
    let mut current_path = open_docs[current_idx].path.clone();
    let mut inverted = false;
    let zotero_dir: Option<String> = load_config().zotero_dir;
    let session_name = session_name;
    let mut saved_session_name: Option<String> = None;
    // Directory the filesystem browser (`e`) last visited; restored from the
    // session and persisted back into it. Defaults to $HOME on first use.
    let mut last_browse_dir: Option<String> = session.and_then(|s| s.last_browse_dir.clone());

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        // Free kitty images from a previously displayed document and force a
        // full repaint. ratatui-image 11 draws kitty rows as one escape-laden
        // cell plus Skip-marked cells that the buffer records as blanks, so
        // ratatui's diff state does not reflect what is physically on screen;
        // without a clear, switching documents leaves stale artifacts behind.
        free_kitty_images();
        let _ = terminal.clear();

        // Save current doc state if we have one open
        if let Some(doc) = open_docs.get_mut(current_idx) {
            // scroll/zoom already saved on switch
            let _ = doc;
        }

        // Find or create entry for current path
        let existing = open_docs.iter().position(|d| d.path == current_path);
        current_idx = match existing {
            Some(i) => i,
            None => {
                open_docs.push(OpenDoc {
                    path: current_path.clone(),
                    scroll: 0,
                    zoom: 1.0,
                    page: None,
                    modified: 0,
                    remarkable_uuid: None,
                    render_path: None,
                });
                open_docs.len() - 1
            }
        };
        let saved_scroll = open_docs[current_idx].scroll;
        let saved_zoom = open_docs[current_idx].zoom;
        let saved_page = open_docs[current_idx].page;

        // Display the locally-generated backing PDF (merged pages / notebook
        // blanks) when one exists; fall back to the original path otherwise.
        let view_path = open_docs[current_idx]
            .render_path
            .as_deref()
            .filter(|p| std::path::Path::new(p).exists())
            .unwrap_or(&current_path)
            .to_string();

        // Try to open content source (URL or PDF file)
        let open_result = if is_url(&current_path) {
            let _ = terminal.draw(|frame| {
                let area = frame.area();
                let msg = format!("Loading {}...", &current_path);
                Paragraph::new(Span::styled(msg, Style::default().fg(Color::Yellow)))
                    .render(area, frame.buffer_mut());
            });
            capture_url(&current_path).map(ContentSource::Web)
        } else {
            Document::open(&view_path).map(ContentSource::Pdf)
        };

        let mut source = match open_result {
            Ok(s) => s,
            Err(_) => {
                // Show "file not available" screen until user switches or quits
                let result: io::Result<AppAction> = loop {
                    terminal.draw(|frame| {
                        let area = frame.area();
                        let msg = if is_url(&current_path) {
                            format!("Failed to load URL: {}", &current_path)
                        } else {
                            let filename = std::path::Path::new(&current_path)
                                .file_name()
                                .map(|f| f.to_string_lossy().to_string())
                                .unwrap_or_else(|| current_path.clone());
                            format!("File not available: {}", filename)
                        };
                        let style = Style::default().fg(Color::Red);
                        let para = Paragraph::new(Line::from(Span::styled(msg, style)));
                        para.render(area, frame.buffer_mut());
                        // Status bar
                        let status_area = ratatui::layout::Rect {
                            x: area.x, y: area.y + area.height.saturating_sub(1),
                            width: area.width, height: 1,
                        };
                        let status = Paragraph::new(Span::styled(
                            " Tab: switch doc | d: doc picker | e: open file | q: quit ",
                            Style::default().fg(Color::Black).bg(Color::Cyan),
                        )).style(Style::default().bg(Color::Cyan));
                        status.render(status_area, frame.buffer_mut());
                    })?;
                    if event::poll(Duration::from_millis(100))? {
                        if let Event::Key(key) = event::read()? {
                            if key.kind != KeyEventKind::Press { continue; }
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => break Ok(AppAction::Quit),
                                KeyCode::Tab => {
                                    let next = (current_idx + 1) % open_docs.len();
                                    break Ok(AppAction::SwitchDoc(next));
                                }
                                KeyCode::BackTab => {
                                    let next = if current_idx == 0 { open_docs.len() - 1 } else { current_idx - 1 };
                                    break Ok(AppAction::SwitchDoc(next));
                                }
                                KeyCode::Char('d') => {
                                    break Ok(AppAction::SwitchDoc(current_idx));
                                }
                                KeyCode::Char('o') => break Ok(AppAction::OpenZotero),
                                KeyCode::Char('O') => break Ok(AppAction::OpenLatest),
                                KeyCode::Char('e') => break Ok(AppAction::OpenFile),
                                _ => {}
                            }
                        }
                    }
                };
                match result {
                    Ok(AppAction::Quit) => break,
                    Ok(AppAction::SwitchDoc(idx)) => {
                        if idx < open_docs.len() {
                            current_path = open_docs[idx].path.clone();
                        }
                    }
                    Ok(AppAction::OpenZotero) => {
                        if let Some(ref dir) = zotero_dir {
                            if let Ok(lib) = load_library(std::path::Path::new(dir)) {
                                let _ = stdout().execute(DisableMouseCapture);
                                let _ = stdout().execute(LeaveAlternateScreen);
                                let _ = disable_raw_mode();
                                match run_zotero_browser(&lib) {
                                    Ok(Some(path)) => {
                                        current_path = path.to_string_lossy().to_string();
                                    }
                                    _ => {}
                                }
                                enable_raw_mode()?;
                                stdout().execute(EnterAlternateScreen)?;
                                stdout().execute(EnableMouseCapture)?;
                            }
                        }
                    }
                    Ok(AppAction::OpenLatest) => {
                        if let Some(ref dir) = zotero_dir {
                            if let Some(path) = latest_pdf(std::path::Path::new(dir)) {
                                current_path = path.to_string_lossy().to_string();
                            }
                        }
                    }
                    Ok(AppAction::OpenFile) => {
                        if let Some(path) = browse_for_file(&mut last_browse_dir)? {
                            current_path = path;
                        }
                    }
                    _ => {}
                }
                continue;
            }
        };

        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        picker.set_background_color(Some(image::Rgba([0, 0, 0, 255])));

        let mut pdf_state = PdfViewState::new(source.page_count(), picker);
        pdf_state.zoom = saved_zoom;
        // Load any reMarkable scribbles pulled for this document (keyed by its
        // device UUID) so the first render bakes them into the page.
        if source.is_pdf() {
            if let Some(uuid) = open_docs[current_idx].remarkable_uuid.as_deref() {
                if let Some(ann) = tui_pdf::rm_lines::load(uuid) {
                    // Transform canvas strokes to PDF points using each page's
                    // real size (works for any page size / device).
                    let map = ann.strokes_by_page(|p| source.page_size(p).ok());
                    pdf_state.set_annotations(map);
                }
            }
        }
        if inverted { pdf_state.toggle_invert(&source); }
        let _ = pdf_state.initial_render(&source);
        pdf_state.global_scroll = saved_scroll;
        // If a sync pulled a newer page from the device, the stored page won't
        // match the (stale) scroll offset — honor the page in that case.
        // Normal restores keep the precise sub-page scroll.
        if let Some(p) = saved_page {
            if p < pdf_state.page_count() && pdf_state.current_page() != p {
                pdf_state.go_to_page(p);
            }
        }

        let outlines = source.outlines();
        let mut toc_state = TocState::new(&outlines);
        let mut link_state = LinkState::new();
        let mut search_state = SearchState::new();
        let mut goto_input: Option<String> = None;
        let mut search_input: Option<String> = None;

        // SyncTeX socket only for PDF files
        let sock = if source.is_pdf() {
            let s = socket_path(std::path::Path::new(source.path_or_url()));
            let _ = fs::remove_file(&s);
            Some(s)
        } else {
            None
        };
        let listener = sock.as_ref().and_then(|s| UnixListener::bind(s).ok());
        if let Some(ref l) = listener {
            l.set_nonblocking(true).ok();
        }

        let result = run_app(
            &mut terminal,
            &mut source,
            &mut pdf_state,
            &mut toc_state,
            &mut link_state,
            &mut search_state,
            &mut goto_input,
            &mut search_input,
            listener.as_ref(),
            &open_docs,
            current_idx,
            &session_name,
            &zotero_dir,
            &mut saved_session_name,
            last_browse_dir.as_deref(),
        );

        if let Some(ref s) = sock {
            let _ = fs::remove_file(s);
        }

        // Save state before switching
        let page_now = pdf_state.current_page();
        if open_docs[current_idx].page != Some(page_now) {
            open_docs[current_idx].modified = tui_pdf::remarkable::now_secs();
        }
        open_docs[current_idx].page = Some(page_now);
        open_docs[current_idx].scroll = pdf_state.global_scroll;
        open_docs[current_idx].zoom = pdf_state.zoom;
        inverted = pdf_state.inverted();

        match result {
            Ok(AppAction::Quit) => break,
            Ok(AppAction::OpenZotero) => {
                if let Some(ref dir) = zotero_dir {
                    // Reload library fresh from DB each time
                    if let Ok(lib) = load_library(std::path::Path::new(dir)) {
                        let _ = stdout().execute(DisableMouseCapture);
                        let _ = stdout().execute(LeaveAlternateScreen);
                        let _ = disable_raw_mode();

                        match run_zotero_browser(&lib) {
                            Ok(Some(path)) => {
                                current_path = path.to_string_lossy().to_string();
                            }
                            _ => {}
                        }

                        enable_raw_mode()?;
                        stdout().execute(EnterAlternateScreen)?;
                        stdout().execute(EnableMouseCapture)?;
                    }
                }
            }
            Ok(AppAction::SwitchDoc(idx)) => {
                if idx < open_docs.len() {
                    current_path = open_docs[idx].path.clone();
                }
            }
            Ok(AppAction::CloseDoc) => {
                if open_docs.len() <= 1 {
                    break; // last doc, quit
                }
                open_docs.remove(current_idx);
                let switch_to = if current_idx >= open_docs.len() {
                    open_docs.len() - 1
                } else {
                    current_idx
                };
                current_path = open_docs[switch_to].path.clone();
                current_idx = switch_to;
            }
            Ok(AppAction::OpenLatest) => {
                if let Some(ref dir) = zotero_dir {
                    if let Some(path) = latest_pdf(std::path::Path::new(dir)) {
                        current_path = path.to_string_lossy().to_string();
                    }
                }
            }
            Ok(AppAction::OpenFile) => {
                if let Some(path) = browse_for_file(&mut last_browse_dir)? {
                    current_path = path;
                }
            }
            Ok(AppAction::PreviewUrl(url)) => {
                // Temporary preview: capture and display the URL, then return to current doc
                let _ = terminal.draw(|frame| {
                    let area = frame.area();
                    let msg = format!("Loading {}...", &url);
                    Paragraph::new(Span::styled(msg, Style::default().fg(Color::Yellow)))
                        .render(area, frame.buffer_mut());
                });
                match capture_url(&url) {
                    Ok(web) => {
                        let mut preview_source = ContentSource::Web(web);
                        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
                        picker.set_background_color(Some(image::Rgba([0, 0, 0, 255])));
                        let mut pdf_state = PdfViewState::new(preview_source.page_count(), picker);
                        if inverted { pdf_state.toggle_invert(&preview_source); }
                        let _ = pdf_state.initial_render(&preview_source);
                        let outlines = preview_source.outlines();
                        let mut toc_state = TocState::new(&outlines);
                        let mut link_state = LinkState::new();
                        let mut search_state = SearchState::new();
                        let mut goto_input: Option<String> = None;
                        let mut search_input: Option<String> = None;
                        let _ = run_app(
                            &mut terminal,
                            &mut preview_source,
                            &mut pdf_state,
                            &mut toc_state,
                            &mut link_state,
                            &mut search_state,
                            &mut goto_input,
                            &mut search_input,
                            None,
                            &open_docs,
                            current_idx,
                            &session_name,
                            &zotero_dir,
                            &mut saved_session_name,
                            last_browse_dir.as_deref(),
                        );
                        // After quitting the preview, return to the current document
                        inverted = pdf_state.inverted();
                    }
                    Err(_) => {
                        // Silently return to current doc
                    }
                }
                // current_path unchanged — will reopen the original doc
            }
            Err(e) => {
                let _ = stdout().execute(DisableMouseCapture);
                let _ = disable_raw_mode();
                let _ = stdout().execute(LeaveAlternateScreen);
                return Err(e);
            }
        }
    }

    free_kitty_images();
    let _ = stdout().execute(DisableMouseCapture);
    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    source: &mut ContentSource,
    pdf_state: &mut PdfViewState,
    toc_state: &mut TocState,
    link_state: &mut LinkState,
    search_state: &mut SearchState,
    goto_input: &mut Option<String>,
    search_input: &mut Option<String>,
    listener: Option<&UnixListener>,
    open_docs: &[OpenDoc],
    current_idx: usize,
    session_name: &Option<String>,
    zotero_dir: &Option<String>,
    saved_session_name: &mut Option<String>,
    last_browse_dir: Option<&str>,
) -> io::Result<AppAction> {
    // Auto-reload state (PDF only)
    let mut last_mtime: Option<SystemTime> = if source.is_pdf() {
        fs::metadata(std::path::Path::new(source.path_or_url()))
            .and_then(|m| m.modified())
            .ok()
    } else {
        None
    };
    let mut last_mtime_check = Instant::now();

    // Status message (shown temporarily, expires after 3s)
    let mut status_message: Option<(String, Instant)> = None;

    // Document picker state
    let mut doc_picker: Option<usize> = None; // selected index

    // SyncTeX probe state
    let mut synctex_probe: Option<String> = None;
    let mut last_probe_grid: Vec<ProbeCell> = Vec::new();

    // Session name input
    let mut session_input: Option<String> = None;

    // Metadata view
    let mut metadata_view: Option<Vec<(String, String)>> = None;

    // Kitty image rows leave placeholder characters on screen that ratatui's
    // diff records as blank cells, so when an overlay opens or closes, cells
    // the diff considers unchanged keep stale image fragments. Track the view
    // layout and force a full repaint whenever it changes.
    let mut prev_view_mode = (false, false, toc_state.visible);

    loop {
        let view_mode = (metadata_view.is_some(), doc_picker.is_some(), toc_state.visible);
        if view_mode != prev_view_mode {
            prev_view_mode = view_mode;
            let _ = terminal.clear();
        }
        // Progress incremental search
        if search_state.searching {
            let _ = search_state.search_tick(source);
            if !search_state.jumped && !search_state.hits.is_empty() {
                search_state.jumped = true;
                search_state.next_hit_from_page(pdf_state.current_page());
                if let Some(hit) = search_state.current_hit() {
                    pdf_state.scroll_to_point(hit.page, hit.y0);
                }
            }
        }

        // Expire status message after 3 seconds
        if let Some((_, created)) = &status_message {
            if created.elapsed() > Duration::from_secs(3) {
                status_message = None;
            }
        }

        // Check for forward search commands from the socket
        if let Some(l) = listener {
            while let Ok((stream, _)) = l.accept() {
                let _ = stream.set_nonblocking(false);
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("forward:") {
                        // Parse "line:col:file"
                        let parts: Vec<&str> = rest.splitn(3, ':').collect();
                        if parts.len() == 3 {
                            if let (Ok(src_line), Ok(col)) =
                                (parts[0].parse::<usize>(), parts[1].parse::<usize>())
                            {
                                let tex_file = parts[2];
                                // Markdown documents use the in-memory source
                                // map we built at render time; LaTeX uses the
                                // synctex CLI. Both yield (page, y-from-top).
                                let target = if source.is_markdown() {
                                    source
                                        .as_document()
                                        .and_then(|d| d.md_forward(src_line))
                                } else {
                                    synctex_view(
                                        std::path::Path::new(source.path_or_url()),
                                        tex_file,
                                        src_line,
                                        col,
                                    )
                                    .map(|fwd| (fwd.page, fwd.y))
                                };
                                if let Some((page, y)) = target {
                                    pdf_state.scroll_to_point(page, y);
                                    status_message = Some((
                                        format!("Forward: {}:{}", tex_file, src_line),
                                        Instant::now(),
                                    ));
                                } else {
                                    status_message = Some((
                                        "Forward search: no result".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    }
                    // Send ack
                    let mut stream = stream;
                    let _ = writeln!(stream, "ok");
                }
            }
        }

        // Ensure current page is rendered before draw (avoids blank on uncached pages)
        pdf_state.ensure_visible_rendered(source);

        let draw_result = terminal.draw(|frame| {
            let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)])
                .split(frame.area());

            let main_area = outer[0];
            let status_area = outer[1];

            if let Some(ref fields) = metadata_view {
                render_metadata_overlay(
                    fields,
                    main_area,
                    frame.buffer_mut(),
                );
            } else if doc_picker.is_some() {
                // Document picker: bordered list in the main area
                let sel = doc_picker.unwrap();
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(Span::styled(
                        " Open Documents (Enter: switch | x: close | Esc: cancel) ",
                        Style::default().fg(Color::Cyan),
                    ));
                let inner = block.inner(main_area);
                block.render(main_area, frame.buffer_mut());

                let list_height = inner.height as usize;
                for (i, doc) in open_docs.iter().enumerate().take(list_height) {
                    let label = std::path::Path::new(&doc.path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_else(|| doc.path.clone());
                    let marker = if i == current_idx { "* " } else { "  " };
                    let text = format!(" {}{}", marker, label);
                    let width = inner.width as usize;
                    let truncated = if text.len() > width {
                        format!("{}…", &text[..width - 1])
                    } else {
                        text
                    };
                    let style = if i == sel {
                        Style::default().fg(Color::Black).bg(Color::White)
                    } else {
                        Style::default().fg(Color::White).bg(Color::Reset)
                    };
                    let area = ratatui::layout::Rect {
                        x: inner.x, y: inner.y + i as u16,
                        width: inner.width, height: 1,
                    };
                    Paragraph::new(Span::styled(truncated, style))
                        .style(style)
                        .render(area, frame.buffer_mut());
                }
            } else {
                let search_opt = if search_state.active {
                    Some(&*search_state)
                } else {
                    None
                };

                if toc_state.visible {
                    let cols = Layout::horizontal([
                        Constraint::Percentage(30),
                        Constraint::Percentage(70),
                    ])
                    .split(main_area);

                    TocWidget.render(cols[0], frame.buffer_mut(), toc_state);

                    if synctex_probe.is_none() {
                        if let Err(e) = pdf_state.update_image(Some(link_state), search_opt, cols[1].width) {
                            let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                            frame.render_widget(msg, cols[1]);
                        } else {
                            frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                        }
                    } else {
                        frame.render_stateful_widget(PdfWidget, cols[1], pdf_state);
                    }
                } else {
                    if synctex_probe.is_none() {
                        if let Err(e) = pdf_state.update_image(Some(link_state), search_opt, main_area.width) {
                            let msg = ratatui::widgets::Paragraph::new(format!("Render error: {e}"));
                            frame.render_widget(msg, main_area);
                        } else {
                            frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                        }
                    } else {
                        frame.render_stateful_widget(PdfWidget, main_area, pdf_state);
                    }
                }
            }

            // Status bar: session_input > synctex_probe > status_message > search_input > goto_input > normal
            if let Some(ref input) = session_input {
                let prompt = format!(" Session name: {}█  (Enter: save, Esc: cancel) ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Green),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Green))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(ref input) = synctex_probe {
                let prompt = format!(" SyncTeX probe: {}█  (Enter: jump, Esc: cancel) ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Yellow),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Yellow))
                    .render(status_area, frame.buffer_mut());
            } else if let Some((ref msg, _)) = status_message {
                let line = Line::from(vec![Span::styled(
                    format!(" {} ", msg),
                    Style::default().fg(Color::White).bg(Color::Magenta),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Magenta))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(input) = search_input.as_ref() {
                let prompt = format!(" /{}█ ", input);
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Cyan))
                    .render(status_area, frame.buffer_mut());
            } else if let Some(input) = goto_input.as_ref() {
                let prompt = format!(
                    " Go to page (1-{}): {}█ ",
                    pdf_state.page_count(),
                    input,
                );
                let line = Line::from(vec![Span::styled(
                    prompt,
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]);
                Paragraph::new(line)
                    .style(Style::default().bg(Color::Cyan))
                    .render(status_area, frame.buffer_mut());
            } else {
                frame.render_widget(
                    StatusBar {
                        state: &*pdf_state,
                        link_state: Some(&*link_state),
                        search_state: Some(&*search_state),
                    },
                    status_area,
                );
            }

            // Re-emit the status row every frame. The kitty stripes above end
            // their cursor-restore dance with CSI 0 B (height-1 areas), which
            // terminals treat as "down 1", parking the real cursor on this row
            // every frame; bytes from an interrupted write land here, and the
            // diff would never repaint cells it believes are unchanged.
            for x in status_area.left()..status_area.right() {
                if let Some(cell) = frame.buffer_mut().cell_mut((x, status_area.y)) {
                    cell.set_diff_option(CellDiffOption::AlwaysUpdate);
                }
            }
        });
        if let Err(e) = draw_result {
            if e.kind() == io::ErrorKind::WouldBlock {
                // Part of the frame was written, so the screen no longer
                // matches ratatui's diff state, and any kitty transmissions
                // cut mid-stream are gone for good (they are sent only once
                // per protocol). Rebuild the protocols and repaint everything.
                pdf_state.invalidate_protocols();
                let _ = terminal.clear();
                continue;
            }
            return Err(e);
        }

        // Poll for input: short timeout when there's active work, long when idle
        let busy = search_state.searching
            || !pdf_state.prerender_done()
            || status_message.is_some();
        let poll_timeout = if busy {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(1)
        };
        let poll_result = event::poll(poll_timeout);
        if poll_result.is_err() {
            continue;
        }
        if poll_result.unwrap() {
            let ev = match event::read() {
                Ok(ev) => ev,
                Err(_) => continue,
            };

            // A monitor scale-factor change may leave the character grid
            // unchanged while changing the pixel size of every cell. Re-query
            // the image picker on resize, but rebuild page stripes only when
            // those pixel dimensions actually differ.
            if let Event::Resize(_, _) = ev {
                let _ = terminal.autoresize();
                if let Ok(mut picker) = Picker::from_query_stdio() {
                    picker.set_background_color(Some(image::Rgba([0, 0, 0, 255])));
                    match pdf_state.refresh_terminal_geometry(source, picker) {
                        Ok(true) => {
                            free_kitty_images();
                            let _ = terminal.clear();
                        }
                        Ok(false) | Err(_) => {}
                    }
                }
                continue;
            }

            // Handle mouse events
            if let Event::Mouse(mouse) = ev {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) if mouse.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                        if let Some((page, pdf_x, pdf_y)) =
                            pdf_state.terminal_to_pdf(mouse.row, mouse.column)
                        {
                            // Markdown reverse-search uses our source map keyed
                            // on the .md path; LaTeX uses the synctex CLI.
                            let hit = if source.is_markdown() {
                                source
                                    .as_document()
                                    .and_then(|d| d.md_reverse(page, pdf_y))
                                    .map(|line| (source.path_or_url().to_string(), line))
                            } else {
                                synctex_edit(
                                    std::path::Path::new(source.path_or_url()),
                                    page + 1,
                                    pdf_x,
                                    pdf_y,
                                )
                                .map(|r| (r.file, r.line))
                            };
                            match hit {
                                Some((file, line)) => {
                                    if !jump_to_neovim(&file, line) {
                                        status_message = Some((
                                            format!("Source: {}:{}", file, line),
                                            Instant::now(),
                                        ));
                                    }
                                }
                                None => {
                                    status_message = Some((
                                        "No source location at click".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => pdf_state.scroll_up(5),
                    MouseEventKind::ScrollDown => pdf_state.scroll_down(5),
                    _ => {}
                }
                continue;
            }

            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Metadata view mode
                if let Some(ref fields) = metadata_view {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('q') => {
                            metadata_view = None;
                        }
                        KeyCode::Char('c') => {
                            if let Some((_, bib)) = fields.iter().find(|(k, _)| k == "BibTeX") {
                                if copy_to_clipboard(bib).is_ok() {
                                    status_message = Some(("BibTeX copied to clipboard".to_string(), Instant::now()));
                                } else {
                                    status_message = Some(("Failed to copy (install xclip or xsel)".to_string(), Instant::now()));
                                }
                                metadata_view = None;
                            }
                        }
                        KeyCode::Char('u') => {
                            let url = fields.iter()
                                .find(|(k, _)| k == "URL")
                                .map(|(_, v)| v.clone())
                                .or_else(|| {
                                    fields.iter()
                                        .find(|(k, _)| k == "DOI")
                                        .map(|(_, v)| format!("https://doi.org/{}", v))
                                });
                            if let Some(url) = url {
                                return Ok(AppAction::PreviewUrl(url));
                            } else {
                                status_message = Some(("No URL or DOI available".to_string(), Instant::now()));
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Document picker mode
                if let Some(sel) = doc_picker.as_mut() {
                    match key.code {
                        KeyCode::Esc => { doc_picker = None; }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if *sel + 1 < open_docs.len() { *sel += 1; }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            *sel = sel.saturating_sub(1);
                        }
                        KeyCode::Enter => {
                            let idx = *sel;
                            doc_picker = None;
                            if idx != current_idx {
                                return Ok(AppAction::SwitchDoc(idx));
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Session name input mode
                if session_input.is_some() {
                    match key.code {
                        KeyCode::Esc => { session_input = None; }
                        KeyCode::Enter => {
                            if let Some(name) = session_input.take() {
                                let name = name.trim().to_string();
                                if !name.is_empty() {
                                    let sess = build_session(&open_docs, current_idx, &pdf_state, last_browse_dir);
                                    match save_session(&name, &sess) {
                                        Ok(_) => {
                                            status_message = Some((
                                                format!("Session '{}' saved", name),
                                                Instant::now(),
                                            ));
                                            *saved_session_name = Some(name);
                                        }
                                        Err(e) => {
                                            status_message = Some((
                                                format!("Failed to save session: {}", e),
                                                Instant::now(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = session_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(input) = session_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // SyncTeX probe mode
                if synctex_probe.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            synctex_probe = None;
                            last_probe_grid.clear();
                            pdf_state.clear_probe_markers();
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            if let Some(input) = synctex_probe.as_mut() {
                                input.push(c);
                            }
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = synctex_probe.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(input) = synctex_probe.take() {
                                pdf_state.clear_probe_markers();
                                if let Ok(num) = input.parse::<usize>() {
                                    if let Some(cell) = last_probe_grid.iter().find(|c| c.number == num) {
                                        if !jump_to_neovim(&cell.file, cell.line) {
                                            status_message = Some((
                                                format!("SyncTeX: {}:{}", cell.file, cell.line),
                                                Instant::now(),
                                            ));
                                        }
                                    } else {
                                        status_message = Some((
                                            format!("SyncTeX: invalid cell {}", num),
                                            Instant::now(),
                                        ));
                                    }
                                }
                                last_probe_grid.clear();
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Search input mode
                if search_input.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            *search_input = None;
                        }
                        KeyCode::Enter => {
                            if let Some(input) = search_input.as_ref() {
                                if !input.is_empty() {
                                    let query = input.clone();
                                    let current_page = pdf_state.current_page();
                                    search_state.start_search(
                                        &query,
                                        source.page_count(),
                                        current_page,
                                    );
                                }
                            }
                            *search_input = None;
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = search_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if let Some(input) = search_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Go-to-page input mode
                if goto_input.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            *goto_input = None;
                        }
                        KeyCode::Enter => {
                            if let Some(input) = goto_input.as_ref() {
                                if let Ok(page) = input.parse::<usize>() {
                                    if page >= 1 && page <= pdf_state.page_count() {
                                        pdf_state.go_to_page(page - 1);
                                    }
                                }
                            }
                            *goto_input = None;
                        }
                        KeyCode::Backspace => {
                            if let Some(input) = goto_input.as_mut() {
                                input.pop();
                            }
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            if let Some(input) = goto_input.as_mut() {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if toc_state.visible {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            toc_state.visible = false;
                        }
                        KeyCode::Char('t') => toc_state.toggle(),
                        KeyCode::Char('j') | KeyCode::Down => toc_state.next(),
                        KeyCode::Char('k') | KeyCode::Up => toc_state.prev(),
                        KeyCode::Enter => {
                            if let Some(page) = toc_state.selected_page() {
                                pdf_state.go_to_page(page);
                                toc_state.visible = false;
                            }
                        }
                        _ => {}
                    }
                } else if link_state.active {
                    match key.code {
                        KeyCode::Esc => link_state.deactivate(),
                        KeyCode::Char('j') | KeyCode::Down => link_state.next(),
                        KeyCode::Char('k') | KeyCode::Up => link_state.prev(),
                        KeyCode::Enter => {
                            if let Some(link) = link_state.selected_link().cloned() {
                                link_state.push_position(pdf_state.global_scroll);
                                pdf_state.go_to_page(link.target_page);
                                link_state.deactivate();
                                link_state.page = usize::MAX;
                            }
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') => return Ok(AppAction::Quit),
                        KeyCode::Char('x') => return Ok(AppAction::CloseDoc),
                        KeyCode::Char('o') => return Ok(AppAction::OpenZotero),
                        KeyCode::Char('O') => return Ok(AppAction::OpenLatest),
                        KeyCode::Char('e') => return Ok(AppAction::OpenFile),
                        KeyCode::Char('S') => {
                            if let Some(name) = saved_session_name.as_ref().or(session_name.as_ref()) {
                                let name = name.clone();
                                let sess = build_session(&open_docs, current_idx, &pdf_state, last_browse_dir);
                                match save_session(&name, &sess) {
                                    Ok(_) => {
                                        status_message = Some((
                                            format!("Session '{}' saved", name),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        status_message = Some((
                                            format!("Failed to save session: {}", e),
                                            Instant::now(),
                                        ));
                                    }
                                }
                            } else {
                                session_input = Some(String::new());
                            }
                        }
                        KeyCode::Char('R') => {
                            if !is_url(&source.path_or_url()) {
                                let path = source.path_or_url().to_string();
                                match send_one_to_remarkable(&path, pdf_state.current_page(), zotero_dir.as_deref()) {
                                    Ok(_) => {
                                        let name = std::path::Path::new(&path)
                                            .file_name()
                                            .map(|f| f.to_string_lossy().to_string())
                                            .unwrap_or_default();
                                        status_message = Some((
                                            format!("Sent '{}' to reMarkable", name),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        status_message = Some((
                                            format!("reMarkable: {} (USB connected?)", e),
                                            Instant::now(),
                                        ));
                                    }
                                }
                            }
                        }
                        KeyCode::Char('C') => {
                            if !is_url(&source.path_or_url()) {
                                let path = source.path_or_url().to_string();
                                match send_to_remarkable_cloud(&path) {
                                    Ok(_) => {
                                        let name = std::path::Path::new(&path)
                                            .file_name()
                                            .map(|f| f.to_string_lossy().to_string())
                                            .unwrap_or_default();
                                        status_message = Some((
                                            format!("Sent '{}' to reMarkable cloud", name),
                                            Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        status_message = Some((
                                            format!("reMarkable cloud: {}", e),
                                            Instant::now(),
                                        ));
                                    }
                                }
                            }
                        }
                        KeyCode::Tab => {
                            if open_docs.len() > 1 {
                                let next = (current_idx + 1) % open_docs.len();
                                return Ok(AppAction::SwitchDoc(next));
                            }
                        }
                        KeyCode::BackTab => {
                            if open_docs.len() > 1 {
                                let prev = if current_idx == 0 { open_docs.len() - 1 } else { current_idx - 1 };
                                return Ok(AppAction::SwitchDoc(prev));
                            }
                        }
                        KeyCode::Char('d') => {
                            doc_picker = Some(current_idx);
                        }
                        KeyCode::Char('m') => {
                            if source.is_web() {
                                // Show URL info for web pages
                                let fields = vec![
                                    ("URL".to_string(), source.path_or_url().to_string()),
                                ];
                                metadata_view = Some(fields);
                            } else if let Some(dir) = zotero_dir {
                                if let Some(entry) = lookup_by_path(
                                    std::path::Path::new(dir),
                                    std::path::Path::new(source.path_or_url()),
                                ) {
                                    metadata_view = Some(metadata_fields(&entry));
                                } else {
                                    status_message = Some((
                                        "No Zotero metadata found for this file".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            } else {
                                status_message = Some((
                                    "Zotero not configured. Run: tui-pdf --setup-zotero <dir>".to_string(),
                                    Instant::now(),
                                ));
                            }
                        }
                        KeyCode::Char('s') => {
                            if let Some((ax, ay, aw, ah)) = pdf_state.last_render_area {
                                let area = ratatui::layout::Rect::new(ax, ay, aw, ah);
                                let grid = compute_probe_grid(pdf_state, source, area);
                                if grid.is_empty() {
                                    status_message = Some((
                                        "SyncTeX: no results on visible area".to_string(),
                                        Instant::now(),
                                    ));
                                } else {
                                    let markers: Vec<_> = grid.iter()
                                        .map(|c| (c.page, c.pdf_x, c.pdf_y, c.number))
                                        .collect();
                                    pdf_state.apply_probe_markers(&markers);
                                    last_probe_grid = grid;
                                    synctex_probe = Some(String::new());
                                }
                            }
                        }
                        KeyCode::Esc => {
                            if search_state.active {
                                search_state.clear();
                            }
                        }
                        KeyCode::Char('/') => {
                            *search_input = Some(String::new());
                        }
                        KeyCode::Char('g') => {
                            *goto_input = Some(String::new());
                        }
                        KeyCode::Char('t') => {
                            if toc_state.has_entries() {
                                toc_state.toggle();
                            }
                        }
                        KeyCode::Char('l') => {
                            let page = pdf_state.current_page();
                            let _ = link_state.activate(source, page);
                        }
                        KeyCode::Char('b') => {
                            if let Some(pos) = link_state.pop_position() {
                                pdf_state.global_scroll = pos.global_scroll;
                            }
                        }
                        KeyCode::Char('n') => {
                            if search_state.active {
                                search_state.next_hit();
                                if let Some(hit) = search_state.current_hit() {
                                    pdf_state.scroll_to_point(hit.page, hit.y0);
                                }
                            } else {
                                pdf_state.next_page();
                            }
                        }
                        KeyCode::Char('p') => {
                            if search_state.active {
                                search_state.prev_hit();
                                if let Some(hit) = search_state.current_hit() {
                                    pdf_state.scroll_to_point(hit.page, hit.y0);
                                }
                            } else {
                                pdf_state.prev_page();
                            }
                        }
                        KeyCode::Left | KeyCode::PageUp => {
                            pdf_state.prev_page()
                        }
                        KeyCode::Right | KeyCode::PageDown => pdf_state.next_page(),
                        KeyCode::Char('j') | KeyCode::Down => pdf_state.scroll_down(3),
                        KeyCode::Char('k') | KeyCode::Up => pdf_state.scroll_up(3),
                        KeyCode::Char('i') => pdf_state.toggle_invert(source),
                        KeyCode::Char('a') => {
                            if pdf_state.has_annotations() {
                                pdf_state.toggle_annotations(source);
                            }
                        }
                        KeyCode::Char('+') | KeyCode::Char('=') => pdf_state.zoom_in(source),
                        KeyCode::Char('-') => pdf_state.zoom_out(source),
                        KeyCode::Char('w') => pdf_state.fit_width(source),
                        KeyCode::Home => pdf_state.first_page(),
                        KeyCode::End => pdf_state.last_page(),
                        _ => {}
                    }
                }
            }
        } else {
            // Idle: check for file changes (~1s interval) and pre-render
            if last_mtime_check.elapsed() >= Duration::from_secs(1) {
                last_mtime_check = Instant::now();
                if source.is_pdf() {
                    if let Ok(meta) = fs::metadata(std::path::Path::new(source.path_or_url())) {
                        if let Ok(mtime) = meta.modified() {
                            if last_mtime.map_or(true, |prev| mtime != prev) {
                                last_mtime = Some(mtime);
                                if source.reload().is_ok() {
                                    let saved_scroll = pdf_state.global_scroll;
                                    pdf_state.on_reload(source);
                                    let _ = pdf_state.initial_render(source);
                                    pdf_state.global_scroll = saved_scroll;
                                    search_state.clear();
                                    link_state.deactivate();
                                    link_state.page = usize::MAX;
                                    let outlines = source.outlines();
                                    *toc_state = TocState::new(&outlines);
                                    status_message = Some((
                                        "File reloaded".to_string(),
                                        Instant::now(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            if !pdf_state.prerender_done() {
                while pdf_state.prerender_tick(source) {
                    if event::poll(Duration::from_millis(0))? {
                        break;
                    }
                }
            }
        }
    }

    #[allow(unreachable_code)]
    Ok(AppAction::Quit)
}

/// A row in the Zotero browser: either a collection (folder) or a paper.
enum BrowserItem {
    Collection { id: i64, name: String },
    Paper { entry_idx: usize },
}

fn run_zotero_browser(library: &ZoteroLibrary) -> io::Result<Option<std::path::PathBuf>> {
    let mut filter = String::new();
    // Whether we're in search mode (entered via `/`). While searching every key
    // types into `filter`; command keys like `m` only act when not searching.
    let mut searching = false;
    let mut selected: usize = 0;
    // Stack of collection IDs we've navigated into (None = root)
    let mut path_stack: Vec<Option<i64>> = vec![None];
    let mut metadata_view: Option<Vec<(String, String)>> = None;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        let current_collection = *path_stack.last().unwrap();

        // Build the list of items to display
        let mut items: Vec<BrowserItem> = Vec::new();

        if filter.is_empty() {
            // Show subcollections first, then papers in this collection
            let mut children = library.child_collections(current_collection);
            children.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            for c in children {
                items.push(BrowserItem::Collection { id: c.id, name: c.name.clone() });
            }

            let paper_indices = if current_collection.is_none() {
                // At root: show all papers
                (0..library.entries.len()).collect::<Vec<_>>()
            } else {
                library.entries_in_collection(current_collection.unwrap())
            };
            for idx in paper_indices {
                items.push(BrowserItem::Paper { entry_idx: idx });
            }
        } else {
            // When filtering, search across all papers regardless of collection
            let lower = filter.to_lowercase();
            for (i, e) in library.entries.iter().enumerate() {
                if e.title.to_lowercase().contains(&lower)
                    || e.authors.to_lowercase().contains(&lower)
                    || e.year.contains(&lower)
                {
                    items.push(BrowserItem::Paper { entry_idx: i });
                }
            }
        }

        if selected >= items.len() {
            selected = items.len().saturating_sub(1);
        }

        // Build breadcrumb path
        let breadcrumb = {
            let mut parts = vec!["Library".to_string()];
            for cid in path_stack.iter().skip(1) {
                if let Some(id) = cid {
                    if let Some(c) = library.collections.iter().find(|c| c.id == *id) {
                        parts.push(c.name.clone());
                    }
                }
            }
            parts.join(" > ")
        };

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Length(1), // breadcrumb / search
                Constraint::Min(1),   // list
                Constraint::Length(1), // status
            ])
            .split(frame.area());

            // Top bar: breadcrumb or search
            if !searching {
                Paragraph::new(Line::from(vec![Span::styled(
                    format!(" {}", breadcrumb),
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]))
                .style(Style::default().bg(Color::Cyan))
                .render(chunks[0], frame.buffer_mut());
            } else {
                Paragraph::new(Line::from(vec![Span::styled(
                    format!(" /{}█", filter),
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )]))
                .style(Style::default().bg(Color::Cyan))
                .render(chunks[0], frame.buffer_mut());
            }

            if let Some(ref fields) = metadata_view {
                render_metadata_overlay(
                    fields,
                    chunks[1],
                    frame.buffer_mut(),
                );
            } else {
                // List, wrapped in a rounded border to match the file browser.
                let list_block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan));
                let inner = list_block.inner(chunks[1]);
                list_block.render(chunks[1], frame.buffer_mut());

                let list_height = inner.height as usize;
                let scroll_offset = if selected >= list_height {
                    selected - list_height + 1
                } else {
                    0
                };

                for (row, item) in items.iter().skip(scroll_offset).take(list_height).enumerate() {
                    let is_selected = scroll_offset + row == selected;
                    let width = inner.width as usize;

                    let text = match item {
                        BrowserItem::Collection { name, .. } => {
                            format!("[{}]", name)
                        }
                        BrowserItem::Paper { entry_idx } => {
                            let e = &library.entries[*entry_idx];
                            let year_part = if e.year.is_empty() { String::new() } else { format!(" ({})", e.year) };
                            let author_part = if e.authors.is_empty() { String::new() } else { format!(" — {}", e.authors) };
                            format!("  {}{}{}", e.title, author_part, year_part)
                        }
                    };

                    let truncated = if text.len() > width {
                        format!("{}…", &text[..width.saturating_sub(1)])
                    } else {
                        text
                    };

                    let style = match (is_selected, item) {
                        (true, _) => Style::default().fg(Color::Black).bg(Color::White),
                        (false, BrowserItem::Collection { .. }) => Style::default().fg(Color::Yellow),
                        (false, BrowserItem::Paper { .. }) => Style::default().fg(Color::White),
                    };

                    let area = ratatui::layout::Rect {
                        x: inner.x,
                        y: inner.y + row as u16,
                        width: inner.width,
                        height: 1,
                    };
                    Paragraph::new(Span::styled(truncated, style))
                        .style(style)
                        .render(area, frame.buffer_mut());
                }
            }

            // Status bar
            let coll_count = items.iter().filter(|i| matches!(i, BrowserItem::Collection { .. })).count();
            let paper_count = items.len() - coll_count;
            let status = format!(
                " {} collections, {} papers | /: search | m: metadata | Enter: open | Backspace: back | Esc: quit ",
                coll_count, paper_count,
            );
            Paragraph::new(Line::from(vec![Span::styled(
                status,
                Style::default().fg(Color::White).bg(Color::DarkGray),
            )]))
            .style(Style::default().bg(Color::DarkGray))
            .render(chunks[2], frame.buffer_mut());
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Metadata view: c to copy BibTeX, any other key closes
                if let Some(ref fields) = metadata_view {
                    if key.code == KeyCode::Char('c') {
                        if let Some((_, bib)) = fields.iter().find(|(k, _)| k == "BibTeX") {
                            let _ = copy_to_clipboard(bib);
                        }
                    }
                    metadata_view = None;
                    continue;
                }

                match key.code {
                    KeyCode::Esc => {
                        if searching {
                            searching = false;
                            filter.clear();
                            selected = 0;
                        } else {
                            break None;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(item) = items.get(selected) {
                            match item {
                                BrowserItem::Collection { id, .. } => {
                                    path_stack.push(Some(*id));
                                    selected = 0;
                                    searching = false;
                                    filter.clear();
                                }
                                BrowserItem::Paper { entry_idx } => {
                                    break Some(library.entries[*entry_idx].pdf_path.clone());
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if searching {
                            filter.pop();
                            if filter.is_empty() {
                                searching = false;
                            }
                            selected = 0;
                        } else if path_stack.len() > 1 {
                            path_stack.pop();
                            selected = 0;
                        }
                    }
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if !items.is_empty() {
                            selected = (selected + 1).min(items.len() - 1);
                        }
                    }
                    // While searching, every key (including `m` and `/`) types
                    // into the filter.
                    KeyCode::Char(c) if searching => {
                        filter.push(c);
                        selected = 0;
                    }
                    KeyCode::Char('/') => {
                        searching = true;
                        filter.clear();
                        selected = 0;
                    }
                    KeyCode::Char('m') => {
                        if let Some(item) = items.get(selected) {
                            if let BrowserItem::Paper { entry_idx } = item {
                                let e = &library.entries[*entry_idx];
                                metadata_view = Some(metadata_fields(e));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    };

    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    Ok(result)
}

/// File extensions the filesystem browser offers as openable documents.
const BROWSABLE_EXTS: &[&str] = &[
    "pdf", "epub", "xps", "cbz", "fb2", "mobi", "svg",
    "tex", "md", "markdown",
];

fn is_browsable_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BROWSABLE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

/// The user's home directory — the default starting point for the file browser.
fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Suspend the viewer's terminal state, let the user pick a file from the
/// filesystem, then restore it. The browser opens at `last_dir` (the directory
/// this session last browsed) or `$HOME` on first use; `last_dir` is updated to
/// wherever the browser ends so the next open resumes there. Returns the
/// selected path (with `.tex` resolved to its PDF), or `None` if cancelled.
fn browse_for_file(last_dir: &mut Option<String>) -> io::Result<Option<String>> {
    let start = last_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(home_dir);

    let _ = stdout().execute(DisableMouseCapture);
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();

    let (final_dir, picked) = run_file_browser(&start)?;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;

    *last_dir = Some(final_dir.to_string_lossy().to_string());
    Ok(picked.map(|p| resolve_tex_to_pdf(&p.to_string_lossy())))
}

/// A row in the filesystem browser: the parent directory, a subdirectory, or a
/// selectable file.
enum FileItem {
    Parent,
    Dir(std::path::PathBuf),
    File(std::path::PathBuf),
}

/// A simple TUI file browser rooted at `start`. Directories sort before files;
/// only document-like files (see `BROWSABLE_EXTS`) are listed. Navigate with
/// j/k or arrows, Enter to descend or open, Backspace/h to go up, `/` to
/// filter, `.` to toggle hidden files, Esc/q to cancel. Returns the directory
/// the browser ended in (so the caller can resume there) and the picked file
/// (`None` if cancelled).
fn run_file_browser(
    start: &std::path::Path,
) -> io::Result<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let mut cur_dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut selected: usize = 0;
    let mut filter = String::new();
    let mut searching = false;
    let mut show_hidden = false;

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        // Build the item list for the current directory.
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(&cur_dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !show_hidden && name.starts_with('.') {
                    continue;
                }
                let ft = entry.file_type();
                let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    dirs.push(path);
                } else if is_browsable_file(&path) {
                    files.push(path);
                }
            }
        }
        let name_of = |p: &std::path::Path| {
            p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        };
        dirs.sort_by_key(|p| name_of(p).to_lowercase());
        files.sort_by_key(|p| name_of(p).to_lowercase());

        let lower = filter.to_lowercase();
        let matches = |p: &std::path::Path| {
            filter.is_empty() || name_of(p).to_lowercase().contains(&lower)
        };

        let mut items: Vec<FileItem> = Vec::new();
        if cur_dir.parent().is_some() {
            items.push(FileItem::Parent);
        }
        for d in dirs.into_iter().filter(|p| matches(p)) {
            items.push(FileItem::Dir(d));
        }
        for f in files.into_iter().filter(|p| matches(p)) {
            items.push(FileItem::File(f));
        }

        if selected >= items.len() {
            selected = items.len().saturating_sub(1);
        }

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Length(1), // path / search
                Constraint::Min(1),    // list
                Constraint::Length(1), // status
            ])
            .split(frame.area());

            // Top bar: current directory or search input.
            let top = if searching {
                format!(" /{}█", filter)
            } else {
                format!(" {}", cur_dir.display())
            };
            Paragraph::new(Line::from(vec![Span::styled(
                top,
                Style::default().fg(Color::Black).bg(Color::Cyan),
            )]))
            .style(Style::default().bg(Color::Cyan))
            .render(chunks[0], frame.buffer_mut());

            // Draw a rounded border around the list and place rows inside it.
            let list_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan));
            let inner = list_block.inner(chunks[1]);
            list_block.render(chunks[1], frame.buffer_mut());

            let list_height = inner.height as usize;
            let scroll_offset = if selected >= list_height {
                selected - list_height + 1
            } else {
                0
            };

            for (row, item) in items.iter().skip(scroll_offset).take(list_height).enumerate() {
                let is_selected = scroll_offset + row == selected;
                let width = inner.width as usize;
                let (text, base) = match item {
                    FileItem::Parent => ("../".to_string(), Color::Yellow),
                    FileItem::Dir(p) => (format!("{}/", name_of(p)), Color::Yellow),
                    FileItem::File(p) => (format!("  {}", name_of(p)), Color::White),
                };
                let truncated = if text.chars().count() > width {
                    let mut t: String = text.chars().take(width.saturating_sub(1)).collect();
                    t.push('…');
                    t
                } else {
                    text
                };
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default().fg(base)
                };
                let area = ratatui::layout::Rect {
                    x: inner.x,
                    y: inner.y + row as u16,
                    width: inner.width,
                    height: 1,
                };
                Paragraph::new(Span::styled(truncated, style))
                    .style(style)
                    .render(area, frame.buffer_mut());
            }

            let status = format!(
                " {} items | Enter: open/enter | Backspace: up | /: filter | .: {} hidden | Esc: cancel ",
                items.len(),
                if show_hidden { "hide" } else { "show" },
            );
            Paragraph::new(Line::from(vec![Span::styled(
                status,
                Style::default().fg(Color::White).bg(Color::DarkGray),
            )]))
            .style(Style::default().bg(Color::DarkGray))
            .render(chunks[2], frame.buffer_mut());
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        if searching {
                            searching = false;
                            filter.clear();
                            selected = 0;
                        } else {
                            break None;
                        }
                    }
                    KeyCode::Enter => match items.get(selected) {
                        Some(FileItem::Parent) => {
                            if let Some(parent) = cur_dir.parent() {
                                cur_dir = parent.to_path_buf();
                                selected = 0;
                                searching = false;
                                filter.clear();
                            }
                        }
                        Some(FileItem::Dir(p)) => {
                            cur_dir = p.clone();
                            selected = 0;
                            searching = false;
                            filter.clear();
                        }
                        Some(FileItem::File(p)) => break Some(p.clone()),
                        None => {}
                    },
                    KeyCode::Backspace => {
                        if searching {
                            filter.pop();
                            if filter.is_empty() {
                                searching = false;
                            }
                            selected = 0;
                        } else if let Some(parent) = cur_dir.parent() {
                            cur_dir = parent.to_path_buf();
                            selected = 0;
                        }
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        if !items.is_empty() {
                            selected = (selected + 1).min(items.len() - 1);
                        }
                    }
                    // While searching, every character types into the filter.
                    KeyCode::Char(c) if searching => {
                        filter.push(c);
                        selected = 0;
                    }
                    KeyCode::Char('j') => {
                        if !items.is_empty() {
                            selected = (selected + 1).min(items.len() - 1);
                        }
                    }
                    KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Char('h') | KeyCode::Left => {
                        if let Some(parent) = cur_dir.parent() {
                            cur_dir = parent.to_path_buf();
                            selected = 0;
                        }
                    }
                    KeyCode::Char('/') => {
                        searching = true;
                        filter.clear();
                        selected = 0;
                    }
                    KeyCode::Char('.') => {
                        show_hidden = !show_hidden;
                        selected = 0;
                    }
                    KeyCode::Char('q') => break None,
                    _ => {}
                }
            }
        }
    };

    let _ = disable_raw_mode();
    let _ = stdout().execute(LeaveAlternateScreen);
    Ok((cur_dir, result))
}
