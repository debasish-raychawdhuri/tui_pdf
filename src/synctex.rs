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
