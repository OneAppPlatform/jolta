//! Well-known locations: the jolta home tree and PATH lookups.

use std::env;
use std::path::{Path, PathBuf};

use crate::ui::die;

pub fn home_dir() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| die("HOME is not set")))
}

pub fn jolta_home() -> PathBuf {
    env::var("JOLTA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".jolta"))
}

pub fn shims_dir() -> PathBuf {
    jolta_home().join("shims")
}

/// First executable named `name` on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = env::var("PATH").ok()?;
    for dir in path.split(':') {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
