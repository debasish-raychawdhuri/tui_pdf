use std::fs;
use std::io;
use std::path::PathBuf;

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

pub fn load_session(name: &str) -> Option<Session> {
    let path = session_path(name);
    let contents = fs::read_to_string(&path).ok()?;
    toml::from_str(&contents).ok()
}

pub fn save_session(name: &str, session: &Session) -> io::Result<()> {
    let dir = sessions_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", name));
    let contents = toml::to_string_pretty(session).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
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
