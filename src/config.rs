use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub zotero_dir: Option<String>,
    pub sessions_dir: Option<String>,
    pub remarkable_host: Option<String>,
}

impl Config {
    /// SSH host of the reMarkable (USB default `10.11.99.1`).
    pub fn remarkable_host(&self) -> String {
        self.remarkable_host.clone().unwrap_or_else(|| "10.11.99.1".to_string())
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SessionDoc {
    pub path: String,
    pub scroll: usize,
    pub zoom: f32,
    /// Cross-device reading position (0-based page index). `None` for legacy
    /// sessions written before page tracking existed.
    #[serde(default)]
    pub page: Option<usize>,
    /// Unix epoch seconds when this doc's position last changed on the computer.
    /// Used for latest-wins reconciliation against the reMarkable.
    #[serde(default)]
    pub modified: i64,
    /// reMarkable document UUID assigned on first sync; the dedup key.
    #[serde(default)]
    pub remarkable_uuid: Option<String>,
    /// Locally-generated backing PDF to display instead of `path` — a merged
    /// PDF (original pages + blank inserted pages) or a notebook's blank pages.
    /// `path` stays the sync source of truth; this is view-only.
    #[serde(default)]
    pub render_path: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Session {
    pub docs: Vec<SessionDoc>,
    pub current: usize,
    /// Directory the filesystem browser (`e`) last visited in this session, so
    /// it reopens where the user left off. `None` until the browser is used.
    #[serde(default)]
    pub last_browse_dir: Option<String>,
}

fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("tui-pdf")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

fn sessions_dir() -> PathBuf {
    let config = load_config();
    if let Some(dir) = config.sessions_dir {
        PathBuf::from(dir)
    } else {
        config_dir().join("sessions")
    }
}

/// Directory holding pulled reMarkable annotations, one `<uuid>.json` per doc.
/// Lives *inside* the sessions dir so it travels with sessions (e.g. when the
/// sessions dir is cloud-synced across computers). It is never uploaded to the
/// reMarkable — `--sync-sessions` only pushes documents referenced by a
/// session's docs, and annotations are not among them.
pub fn annotations_dir() -> PathBuf {
    sessions_dir().join("annotations")
}

/// Directory holding blank backing PDFs generated for pulled reMarkable
/// notebooks (which have no PDF of their own). One `<uuid>/<name>.pdf` per
/// notebook; the strokes are drawn over it via the annotation overlay. Also
/// under the sessions dir (see [`annotations_dir`]) so it cloud-syncs with
/// sessions; the tablet is authoritative for notebooks, so these are never
/// pushed back to it.
pub fn notebooks_dir() -> PathBuf {
    sessions_dir().join("notebooks")
}

/// One-time relocation of annotation/notebook storage from the old location
/// under the config dir (`<config>/annotations`, `<config>/notebooks`) into the
/// sessions dir, where they now live. Moves each only when the destination
/// doesn't already exist — if the sessions dir already has them (e.g. arrived
/// via cloud sync), that copy wins and the old one is left untouched.
pub fn migrate_storage_into_sessions_dir() {
    for name in ["annotations", "notebooks"] {
        let old = config_dir().join(name);
        let new = sessions_dir().join(name);
        if old.exists() && !new.exists() && old != new {
            if let Some(parent) = new.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = move_path(&old, &new);
        }
    }
}

pub fn move_sessions_dir(new_dir: &str) -> io::Result<()> {
    let new_path = PathBuf::from(new_dir);
    fs::create_dir_all(&new_path)?;
    let old_path = {
        let config = load_config();
        if let Some(dir) = config.sessions_dir {
            PathBuf::from(dir)
        } else {
            config_dir().join("sessions")
        }
    };
    // Move existing session files *and* the annotations/ and notebooks/ subdirs
    // that now live alongside them.
    if old_path.exists() && old_path != new_path {
        if let Ok(entries) = fs::read_dir(&old_path) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dest = new_path.join(entry.file_name());
                move_path(&src, &dest)?;
            }
        }
        // Remove old dir if empty
        let _ = fs::remove_dir(&old_path);
    }
    // Update config
    let mut config = load_config();
    config.sessions_dir = Some(new_dir.to_string());
    save_config(&config)
}

/// Move `src` to `dest`, handling both files and directories. `rename` is one
/// syscall when the two are on the same filesystem; across filesystems it fails,
/// so fall back to a recursive copy + remove.
fn move_path(src: &Path, dest: &Path) -> io::Result<()> {
    if fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    if src.is_dir() {
        copy_dir_all(src, dest)?;
        fs::remove_dir_all(src)
    } else {
        fs::copy(src, dest)?;
        fs::remove_file(src)
    }
}

/// Recursively copy the contents of directory `src` into `dest` (created if
/// needed). Used as the cross-filesystem fallback for [`move_path`].
fn copy_dir_all(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub fn session_path(name: &str) -> PathBuf {
    sessions_dir().join(format!("{}.toml", name))
}

pub fn load_config() -> Config {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save_config(config: &Config) -> io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, contents)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// Resolve a path to an absolute path, even if the file no longer exists.
/// Uses `canonicalize` when possible, otherwise joins with the current dir.
fn absolutize(path: &str) -> PathBuf {
    let p = Path::new(path);
    if let Ok(abs) = p.canonicalize() {
        return abs;
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Convert an absolute path under Zotero storage to `zotero://KEY/file.pdf`.
/// Otherwise absolutize the path and, if it lives under the user's home
/// directory, store it home-relative as `~/file.pdf` so sessions don't depend
/// on the directory they were opened from.
fn to_portable_path(path: &str, zotero_dir: Option<&str>) -> String {
    if let Some(zdir) = zotero_dir {
        let storage = Path::new(zdir).join("storage");
        if let Ok(storage) = storage.canonicalize() {
            if let Ok(abs) = Path::new(path).canonicalize() {
                if let Ok(rel) = abs.strip_prefix(&storage) {
                    return format!("zotero://{}", rel.display());
                }
            }
        }
    }
    let abs = absolutize(path);
    if let Some(home) = home_dir() {
        if let Ok(rel) = abs.strip_prefix(&home) {
            return Path::new("~").join(rel).to_string_lossy().to_string();
        }
    }
    abs.to_string_lossy().to_string()
}

/// Resolve a portable path back to an absolute path.
/// `zotero://KEY/file.pdf` becomes `<zotero_dir>/storage/KEY/file.pdf`, and a
/// leading `~` expands to the user's home directory.
fn from_portable_path(path: &str, zotero_dir: Option<&str>) -> String {
    if let Some(rest) = path.strip_prefix("zotero://") {
        if let Some(zdir) = zotero_dir {
            return Path::new(zdir).join("storage").join(rest)
                .to_string_lossy().to_string();
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    } else if path == "~" {
        if let Some(home) = home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Locate the locally-generated backing PDF for a device doc by its UUID,
/// resolved against the *current* notebooks dir rather than a stored path — so
/// it keeps working after the sessions dir is moved or cloud-synced to another
/// machine. Prefers `merged.pdf` (a PDF with pages inserted on the tablet);
/// otherwise the sole PDF in the dir (a notebook's blank page). `None` if none.
fn backing_pdf(uuid: &str) -> Option<PathBuf> {
    let dir = notebooks_dir().join(uuid);
    let merged = dir.join("merged.pdf");
    if merged.is_file() {
        return Some(merged);
    }
    fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("pdf"))
}

pub fn load_session(name: &str) -> Option<Session> {
    let path = session_path(name);
    let contents = fs::read_to_string(&path).ok()?;
    let mut session: Session = toml::from_str(&contents).ok()?;
    let config = load_config();
    let zotero_dir = config.zotero_dir.as_deref();
    session.last_browse_dir = session
        .last_browse_dir
        .as_deref()
        .map(|p| from_portable_path(p, zotero_dir));
    for doc in &mut session.docs {
        doc.path = from_portable_path(&doc.path, zotero_dir);
        doc.render_path = doc.render_path.as_ref().map(|p| from_portable_path(p, zotero_dir));

        // Backing PDFs (merged / notebook blanks) live *inside* the sessions dir,
        // so a stored path goes stale the moment that dir moves. Re-resolve them
        // from the current notebooks dir by UUID instead of trusting the path.
        if let Some(uuid) = doc.remarkable_uuid.clone() {
            match &doc.render_path {
                Some(rp) if !Path::new(rp).exists() => {
                    if let Some(p) = backing_pdf(&uuid) {
                        doc.render_path = Some(p.to_string_lossy().into_owned());
                    }
                }
                // A notebook has no separate source: its blank backing PDF *is*
                // the doc path, so heal that when it's the thing gone missing.
                None if !Path::new(&doc.path).exists() => {
                    if let Some(p) = backing_pdf(&uuid) {
                        doc.path = p.to_string_lossy().into_owned();
                    }
                }
                _ => {}
            }
        }
    }
    Some(session)
}

pub fn save_session(name: &str, session: &Session) -> io::Result<()> {
    let dir = sessions_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", name));
    let config = load_config();
    let zotero_dir = config.zotero_dir.as_deref();
    let portable = Session {
        docs: session.docs.iter().map(|d| SessionDoc {
            path: to_portable_path(&d.path, zotero_dir),
            scroll: d.scroll,
            zoom: d.zoom,
            page: d.page,
            modified: d.modified,
            remarkable_uuid: d.remarkable_uuid.clone(),
            render_path: d.render_path.as_ref().map(|p| to_portable_path(p, zotero_dir)),
        }).collect(),
        current: session.current,
        last_browse_dir: session.last_browse_dir.as_deref().map(|p| to_portable_path(p, zotero_dir)),
    };
    let contents = toml::to_string_pretty(&portable).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, contents)
}

pub fn list_sessions() -> Vec<String> {
    let dir = sessions_dir();
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Sessions are `<name>.toml` files; ignore the annotations/ and
            // notebooks/ subdirs (and anything else) that share this dir.
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Some(name) = path.file_stem() {
                    names.push(name.to_string_lossy().to_string());
                }
            }
        }
    }
    names.sort();
    names
}
