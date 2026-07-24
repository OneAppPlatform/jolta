//! Well-known locations: the jolta home tree and PATH lookups.

use std::env;
use std::path::PathBuf;

use crate::ui::die;

pub fn home_dir() -> PathBuf {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| die("neither HOME nor USERPROFILE is set"))
}

pub fn jolta_home() -> PathBuf {
    env::var("JOLTA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".jolta"))
}

pub fn shims_dir() -> PathBuf {
    jolta_home().join("shims")
}

/// First executable named `name` on PATH (tries `name.exe` too on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let p = dir.join(format!("{name}{}", env::consts::EXE_SUFFIX));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
