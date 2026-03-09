use std::path::Path;
use std::process::Command;

pub struct SyncTexResult {
    pub file: String,
    pub line: usize,
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
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
