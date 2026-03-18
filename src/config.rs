use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub zotero_dir: Option<String>,
    pub sessions_dir: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SessionDoc {
    pub path: String,
    pub scroll: usize,
    pub zoom: f32,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Session {
    pub docs: Vec<SessionDoc>,
    pub current: usize,
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
    // Move existing session files
    if old_path.exists() && old_path != new_path {
        if let Ok(entries) = fs::read_dir(&old_path) {
            for entry in entries.flatten() {
                let src = entry.path();
                if src.is_file() {
                    let dest = new_path.join(entry.file_name());
                    fs::rename(&src, &dest).or_else(|_| {
                        // rename fails across filesystems, fall back to copy+remove
                        fs::copy(&src, &dest)?;
                        fs::remove_file(&src)
                    })?;
                }
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

/// Convert an absolute path under Zotero storage to `zotero://KEY/file.pdf`,
/// or return the path unchanged if it's not under the Zotero storage dir.
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
    path.to_string()
}

/// Resolve a portable path back to an absolute path.
/// `zotero://KEY/file.pdf` becomes `<zotero_dir>/storage/KEY/file.pdf`.
fn from_portable_path(path: &str, zotero_dir: Option<&str>) -> String {
    if let Some(rest) = path.strip_prefix("zotero://") {
        if let Some(zdir) = zotero_dir {
            return Path::new(zdir).join("storage").join(rest)
                .to_string_lossy().to_string();
        }
    }
    path.to_string()
}

pub fn load_session(name: &str) -> Option<Session> {
    let path = session_path(name);
    let contents = fs::read_to_string(&path).ok()?;
    let mut session: Session = toml::from_str(&contents).ok()?;
    let config = load_config();
    let zotero_dir = config.zotero_dir.as_deref();
    for doc in &mut session.docs {
        doc.path = from_portable_path(&doc.path, zotero_dir);
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
        }).collect(),
        current: session.current,
    };
    let contents = toml::to_string_pretty(&portable).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, contents)
}

pub fn list_sessions() -> Vec<String> {
    let dir = sessions_dir();
    let mut names = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem() {
                names.push(name.to_string_lossy().to_string());
            }
        }
    }
    names.sort();
    names
}
