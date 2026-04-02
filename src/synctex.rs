use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SyncTexResult {
    pub file: String,
    pub line: usize,
}

/// Result of a forward search: page (0-indexed) and y position in PDF points.
pub struct SyncTexForward {
    pub page: usize,
    pub y: f32,
}

/// Run `synctex edit` to find the source location for a PDF position.
/// `page` is 1-based. `x` and `y` are in PDF points.
pub fn synctex_edit(pdf_path: &Path, page: usize, x: f32, y: f32) -> Option<SyncTexResult> {
    let pdf_str = pdf_path.to_str()?;
    let input = format!("{page}:{x}:{y}:{pdf_str}");
    let output = Command::new("synctex")
        .args(["edit", "-o", &input])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut file = None;
    let mut line = None;

    for l in stdout.lines() {
        if let Some(rest) = l.strip_prefix("Input:") {
            file = Some(rest.to_string());
        } else if let Some(rest) = l.strip_prefix("Line:") {
            line = rest.trim().parse::<usize>().ok();
        }
    }

    match (file, line) {
        (Some(file), Some(line)) if line > 0 => Some(SyncTexResult { file, line }),
        _ => None,
    }
}

/// Jump to a file:line in a running neovim instance via `$NVIM` socket.
/// Returns false if `$NVIM` is not set or the command fails.
pub fn jump_to_neovim(file: &str, line: usize) -> bool {
    let nvim_socket = match std::env::var("NVIM") {
        Ok(s) if !s.is_empty() => s,
        _ => return false,
    };

    let cmd = format!(":e +{line} {file}\r");
    Command::new("nvim")
        .args(["--server", &nvim_socket, "--remote-send", &cmd])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Run `synctex view` to find the PDF position for a source location.
/// Returns page (0-indexed) and y coordinate in PDF points.
pub fn synctex_view(
    pdf_path: &Path,
    tex_file: &str,
    line: usize,
    col: usize,
) -> Option<SyncTexForward> {
    let pdf_str = pdf_path.to_str()?;
    let input = format!("{line}:{col}:{tex_file}");
    let output = Command::new("synctex")
        .args(["view", "-i", &input, "-o", pdf_str])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut page = None;
    let mut y = None;

    for l in stdout.lines() {
        if let Some(rest) = l.strip_prefix("Page:") {
            page = rest.trim().parse::<usize>().ok();
        } else if let Some(rest) = l.strip_prefix("y:") {
            y = rest.trim().parse::<f32>().ok();
        }
    }

    match (page, y) {
        (Some(p), Some(y_val)) if p > 0 => Some(SyncTexForward {
            page: p - 1,
            y: y_val,
        }),
        _ => None,
    }
}

/// A synctex position record: file, line, page (0-indexed), x, y in PDF points.
pub struct SyncTexPosition {
    pub file: String,
    pub line: usize,
    pub page: usize,
    pub x: f32,
    pub y: f32,
}

/// Parse the synctex.gz (or .synctex) file for a PDF and return all unique
/// (file, line) positions with their page and coordinates.
/// Filters to user source files (skips system .sty/.cls etc under /usr/).
/// Coordinates are converted from scaled points to PDF points (÷65536).
pub fn synctex_positions(pdf_path: &Path) -> Vec<SyncTexPosition> {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Read as _;

    // Find the synctex file: try .synctex.gz then .synctex
    let stem = pdf_path.with_extension("");
    let gz_path = stem.with_extension("synctex.gz");
    let plain_path = stem.with_extension("synctex");

    let data = if gz_path.exists() {
        let file = match File::open(&gz_path) { Ok(f) => f, Err(_) => return Vec::new() };
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut s = String::new();
        if decoder.read_to_string(&mut s).is_err() { return Vec::new(); }
        s
    } else if plain_path.exists() {
        match std::fs::read_to_string(&plain_path) { Ok(s) => s, Err(_) => return Vec::new() }
    } else {
        return Vec::new();
    };

    // Parse Input tags
    let mut inputs: HashMap<u32, String> = HashMap::new();
    let pdf_dir = pdf_path.parent().unwrap_or(Path::new("."));

    for line in data.lines() {
        if let Some(rest) = line.strip_prefix("Input:") {
            // Format: tag:filepath
            if let Some((tag_str, filepath)) = rest.split_once(':') {
                if let Ok(tag) = tag_str.parse::<u32>() {
                    // Skip system files
                    if !filepath.starts_with("/usr/") {
                        // Resolve relative paths against PDF directory
                        let resolved = if filepath.starts_with("./") || !filepath.starts_with('/') {
                            pdf_dir.join(filepath).to_string_lossy().to_string()
                        } else {
                            filepath.to_string()
                        };
                        inputs.insert(tag, resolved);
                    }
                }
            }
        }
    }

    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current_page: usize = 0;

    for line in data.lines() {
        // Track current page: {N starts page, }N ends page
        if line.starts_with('{') {
            if let Ok(p) = line[1..].parse::<usize>() {
                current_page = p;
            }
            continue;
        }

        // Parse position records: ( [ h v k g all have format tag,line:x,y:...
        let record_char = line.as_bytes().first().copied().unwrap_or(0);
        if !matches!(record_char, b'(' | b'[' | b'h' | b'v' | b'k' | b'g') {
            continue;
        }

        let rest = &line[1..];
        // Format: tag,line:x,y:...
        let Some((tag_line, coords)) = rest.split_once(':') else { continue };
        let Some((tag_str, line_str)) = tag_line.split_once(',') else { continue };
        let Ok(tag) = tag_str.parse::<u32>() else { continue };
        let Some(file) = inputs.get(&tag) else { continue };
        let Ok(src_line) = line_str.parse::<usize>() else { continue };
        if src_line == 0 { continue; }

        // Parse x,y from coords (before next colon)
        let coord_part = coords.split_once(':').map(|(c, _)| c).unwrap_or(coords);
        let Some((x_str, y_str)) = coord_part.split_once(',') else { continue };
        let Ok(x_sp) = x_str.parse::<f64>() else { continue };
        let Ok(y_sp) = y_str.parse::<f64>() else { continue };

        let key = (file.clone(), src_line);
        if !seen.insert(key) { continue; }

        // Convert scaled points to PDF points (1 PDF point = 65536 scaled points)
        let x_pt = (x_sp / 65536.0) as f32;
        let y_pt = (y_sp / 65536.0) as f32;

        results.push(SyncTexPosition {
            file: file.clone(),
            line: src_line,
            page: current_page.saturating_sub(1), // synctex pages are 1-based
            x: x_pt,
            y: y_pt,
        });
    }

    results
}

/// Compute the socket path for a given PDF, based on its canonical path.
pub fn socket_path(pdf_path: &Path) -> PathBuf {
    let canonical = pdf_path.canonicalize().unwrap_or_else(|_| pdf_path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/tui-pdf-{:016x}.sock", hash))
}

/// Send a forward search command to a running tui-pdf instance.
pub fn send_forward(sock_path: &Path, message: &str) -> bool {
    if let Ok(mut stream) = UnixStream::connect(sock_path) {
        let _ = writeln!(stream, "{}", message);
        // Read acknowledgement
        let mut reader = BufReader::new(&stream);
        let mut response = String::new();
        let _ = reader.read_line(&mut response);
        true
    } else {
        false
    }
}
