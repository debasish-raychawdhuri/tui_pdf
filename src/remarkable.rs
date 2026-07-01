//! reMarkable (Paper Pro, Developer Mode) sync over SSH.
//!
//! The device stores each item under `~/.local/share/remarkable/xochitl/` as
//! sidecar files keyed by a v4 UUID. A PDF document is `<uuid>.pdf` plus
//! `<uuid>.metadata` (JSON: visibleName, parent, type, `lastOpenedPage` = the
//! reading position, ms-epoch `lastOpened`/`lastModified`), `<uuid>.content`
//! (JSON: fileType/pageCount/page-UUID list) and `<uuid>.pagedata`. A folder is
//! a `CollectionType` with just `.metadata` and an empty `.content`.
//!
//! We never touch the device store directly while `xochitl` runs — callers wrap
//! the mutation phase in [`stop_xochitl`]/[`start_xochitl`].

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub const XOCHITL_DIR: &str = "/home/root/.local/share/remarkable/xochitl";

/// Common SSH options: key-only auth, fail fast, auto-trust the device key.
fn ssh_opts() -> Vec<&'static str> {
    vec![
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        "-o", "StrictHostKeyChecking=accept-new",
    ]
}

fn target(host: &str) -> String {
    format!("root@{}", host)
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Current unix time in whole seconds.
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Run a shell command on the device and return its stdout.
pub fn rm_run(host: &str, remote_cmd: &str) -> io::Result<String> {
    let output = Command::new("ssh")
        .args(ssh_opts())
        .arg(target(host))
        .arg(remote_cmd)
        .output()
        .map_err(|e| err(format!("failed to launch ssh: {}", e)))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(err(format!(
            "ssh command failed ({}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )))
    }
}

/// Verify the device is reachable over SSH with key auth.
pub fn preflight(host: &str) -> io::Result<()> {
    rm_run(host, "true").map(|_| ()).map_err(|_| {
        err(format!(
            "cannot reach reMarkable at {host} over SSH.\n  \
             - connect the USB cable and ensure Developer Mode is on\n  \
             - install your key once with: ssh-copy-id root@{host}"
        ))
    })
}

/// Read a remote file's contents (`ssh … cat <path>`).
pub fn rm_read_file(host: &str, remote_path: &str) -> io::Result<String> {
    rm_run(host, &format!("cat {}", shell_quote(remote_path)))
}

/// Write `contents` to a remote file by piping over `ssh … 'cat > path'`.
pub fn rm_write_file(host: &str, remote_path: &str, contents: &str) -> io::Result<()> {
    let mut child = Command::new("ssh")
        .args(ssh_opts())
        .arg(target(host))
        .arg(format!("cat > {}", shell_quote(remote_path)))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| err(format!("failed to launch ssh: {}", e)))?;
    child
        .stdin
        .take()
        .ok_or_else(|| err("no ssh stdin"))?
        .write_all(contents.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(err(format!("failed to write {}", remote_path)))
    }
}

/// Copy a local file to the device with `scp`.
pub fn rm_scp_to(host: &str, local: &str, remote_path: &str) -> io::Result<()> {
    let output = Command::new("scp")
        .args(ssh_opts())
        .arg(local)
        .arg(format!("{}:{}", target(host), remote_path))
        .output()
        .map_err(|e| err(format!("failed to launch scp: {}", e)))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(err(format!(
            "scp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Copy a remote file or directory from the device to `local` with `scp -r`.
/// The inverse of [`rm_scp_to`]; used to pull annotation `.rm` files down.
pub fn rm_scp_from(host: &str, remote_path: &str, local: &str) -> io::Result<()> {
    let output = Command::new("scp")
        .args(ssh_opts())
        .arg("-r")
        .arg(format!("{}:{}", target(host), remote_path))
        .arg(local)
        .output()
        .map_err(|e| err(format!("failed to launch scp: {}", e)))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(err(format!(
            "scp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// The device's stroke-canvas width in px — the reference the annotation
/// transform normalizes by. reMarkable stores strokes in framebuffer pixels and
/// fits PDFs to this width; it differs by model (rM1/rM2 = 1404, Paper Pro is
/// larger), so we ask the device rather than assume. Falls back to 1404.
pub fn device_stroke_width(host: &str) -> f32 {
    // rM1/rM2 expose the framebuffer as "<w>,<h>" (or "<w>x<h>").
    if let Ok(out) = rm_run(host, "cat /sys/class/graphics/fb0/virtual_size 2>/dev/null") {
        if let Some(w) = out.split(|c| c == ',' || c == 'x').next() {
            if let Ok(n) = w.trim().parse::<f32>() {
                if n > 0.0 {
                    return n;
                }
            }
        }
    }
    // Newer devices (Paper Pro) may not expose fb0 — fall back to model name.
    let model = rm_run(
        host,
        "cat /sys/devices/soc0/machine 2>/dev/null; cat /proc/device-tree/model 2>/dev/null",
    )
    .unwrap_or_default()
    .to_lowercase();
    if model.contains("ferrari") || model.contains("pro") {
        1620.0 // reMarkable Paper Pro
    } else {
        1404.0 // rM1 / rM2
    }
}

/// UUIDs of documents that carry handwritten annotations, i.e. have a
/// `<uuid>/` directory containing at least one `<page-uuid>.rm` file. One SSH
/// round-trip for the whole store, so callers can cheaply test membership.
pub fn list_annotated_uuids(host: &str) -> io::Result<std::collections::HashSet<String>> {
    // For each subdirectory, emit its name if it holds any .rm file.
    let cmd = format!(
        "cd {dir} && for d in */; do u=${{d%/}}; \
         if ls \"$d\"*.rm >/dev/null 2>&1; then echo \"$u\"; fi; done",
        dir = XOCHITL_DIR
    );
    let out = rm_run(host, &cmd)?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Restart xochitl so it rescans the document store and shows newly added
/// files. This briefly blanks the screen, so callers should do it once at the
/// very end and only when something actually changed. We deliberately never
/// *stop* xochitl for the duration of a sync — that tears down the UI and the
/// USB/SSH link. Writing sidecar files while it runs is fine; new documents
/// (fresh UUIDs) are simply picked up on the next restart.
pub fn restart_xochitl(host: &str) -> io::Result<()> {
    rm_run(host, "systemctl restart xochitl").map(|_| ())
}

/// Single-quote a string for safe use in a remote shell command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Generate a v4 UUID from `/dev/urandom` (no external crate).
pub fn new_uuid() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    // Read exactly 16 bytes — `/dev/urandom` never reaches EOF, so reading the
    // whole "file" would loop forever.
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    // Fold in the clock so we never repeat even if /dev/urandom is unreadable.
    let t = now_ms() as u64;
    for i in 0..8 {
        bytes[i] ^= (t >> (i * 8)) as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    let h: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]
    )
}

/// A document/folder on the device, parsed from its `.metadata`.
#[derive(Debug, Clone)]
pub struct RmItem {
    pub uuid: String,
    pub visible_name: String,
    pub parent: String,
    pub is_collection: bool,
    pub last_opened_page: i64,
    /// Most recent activity (ms): `lastOpened`, falling back to `lastModified`.
    pub activity_ms: i64,
    /// `lastModified` (ms), which bumps specifically when the document is edited
    /// (annotated). Used to decide whether pulled annotations are stale.
    pub last_modified_ms: i64,
}

/// Parse a ms-epoch field that may be a JSON string or number; 0 if absent/empty.
fn parse_ms(v: &Value) -> i64 {
    match v {
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Read every `*.metadata` on the device in one SSH round-trip and parse them.
pub fn read_index(host: &str) -> io::Result<Vec<RmItem>> {
    // Emit a marker line + the JSON for each metadata file.
    let cmd = format!(
        "cd {dir} && for f in *.metadata; do echo \"@@@${{f%.metadata}}\"; cat \"$f\"; echo; done",
        dir = XOCHITL_DIR
    );
    let out = rm_run(host, &cmd)?;
    let mut items = Vec::new();
    for chunk in out.split("@@@").skip(1) {
        let mut lines = chunk.splitn(2, '\n');
        let uuid = lines.next().unwrap_or("").trim().to_string();
        let json_text = lines.next().unwrap_or("");
        if uuid.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(json_text.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_collection = v.get("type").and_then(|t| t.as_str()) == Some("CollectionType");
        let last_opened = parse_ms(v.get("lastOpened").unwrap_or(&Value::Null));
        let last_modified = parse_ms(v.get("lastModified").unwrap_or(&Value::Null));
        items.push(RmItem {
            uuid,
            visible_name: v
                .get("visibleName")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
            parent: v.get("parent").and_then(|p| p.as_str()).unwrap_or("").to_string(),
            is_collection,
            last_opened_page: v
                .get("lastOpenedPage")
                .and_then(|p| p.as_i64())
                .unwrap_or(0),
            activity_ms: if last_opened > 0 { last_opened } else { last_modified },
            last_modified_ms: last_modified,
        });
    }
    Ok(items)
}

fn remote(uuid: &str, ext: &str) -> String {
    format!("{}/{}.{}", XOCHITL_DIR, uuid, ext)
}

/// Create a `CollectionType` (folder) on the device, returning its UUID.
pub fn create_collection(host: &str, name: &str, parent: &str) -> io::Result<String> {
    let uuid = new_uuid();
    let now = now_ms().to_string();
    let meta = json!({
        "deleted": false,
        "lastModified": now,
        "createdTime": now,
        "lastOpened": "",
        "lastOpenedPage": 0,
        "metadatamodified": false,
        "modified": false,
        "parent": parent,
        "pinned": false,
        "synced": false,
        "type": "CollectionType",
        "version": 0,
        "visibleName": name,
    });
    rm_write_file(host, &remote(&uuid, "metadata"), &meta.to_string())?;
    rm_write_file(host, &remote(&uuid, "content"), "{}")?;
    Ok(uuid)
}

/// Upload a PDF as a new `DocumentType`, opening at `page`. Returns the new UUID.
/// Never overwrites an existing document (caller checks for existence first).
pub fn upload_pdf(
    host: &str,
    local_pdf: &str,
    visible_name: &str,
    parent: &str,
    page_count: usize,
    page: usize,
) -> io::Result<String> {
    let uuid = new_uuid();
    let now = now_ms().to_string();
    let size = std::fs::metadata(local_pdf).map(|m| m.len()).unwrap_or(0);

    rm_scp_to(host, local_pdf, &remote(&uuid, "pdf"))?;

    let page_uuids: Vec<String> = (0..page_count).map(|_| new_uuid()).collect();
    let redirection: Vec<usize> = (0..page_count).collect();
    let content = json!({
        "coverPageNumber": -1,
        "documentMetadata": {},
        "dummyDocument": false,
        "extraMetadata": {},
        "fileType": "pdf",
        "fontName": "",
        "formatVersion": 1,
        "lineHeight": -1,
        "margins": 125,
        "orientation": "portrait",
        "originalPageCount": page_count,
        "pageCount": page_count,
        "pageTags": [],
        "pages": page_uuids,
        "redirectionPageMap": redirection,
        "sizeInBytes": size.to_string(),
        "tags": [],
        "textAlignment": "left",
        "textScale": 1,
        "zoomMode": "bestFit",
    });
    rm_write_file(host, &remote(&uuid, "content"), &content.to_string())?;

    let meta = json!({
        "createdTime": now,
        "deleted": false,
        "lastModified": now,
        "lastOpened": now,
        "lastOpenedPage": page,
        "metadatamodified": false,
        "modified": false,
        "new": false,
        "parent": parent,
        "pinned": false,
        "source": "",
        "synced": false,
        "type": "DocumentType",
        "version": 0,
        "visibleName": visible_name,
    });
    rm_write_file(host, &remote(&uuid, "metadata"), &meta.to_string())?;

    // pagedata: one template line per page (the device tolerates "Blank").
    let pagedata = std::iter::repeat("Blank").take(page_count.max(1)).collect::<Vec<_>>().join("\n");
    rm_write_file(host, &remote(&uuid, "pagedata"), &pagedata)?;

    Ok(uuid)
}

/// Overwrite an existing document's reading position by editing its `.metadata`
/// in place (preserves all other fields, including annotation state).
pub fn set_position(host: &str, uuid: &str, page: usize) -> io::Result<()> {
    let text = rm_read_file(host, &remote(uuid, "metadata"))?;
    let mut v: Value = serde_json::from_str(text.trim())
        .map_err(|e| err(format!("bad metadata for {}: {}", uuid, e)))?;
    let now = now_ms().to_string();
    if let Value::Object(map) = &mut v {
        map.insert("lastOpenedPage".into(), json!(page));
        map.insert("lastModified".into(), json!(now));
        map.insert("lastOpened".into(), json!(now));
    }
    rm_write_file(host, &remote(uuid, "metadata"), &v.to_string())
}
