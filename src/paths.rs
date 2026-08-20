//! Well-known locations: the jolta home tree and PATH lookups.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ui::die;

pub fn home_dir() -> PathBuf {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| die("neither HOME nor USERPROFILE is set"))
}

pub fn jolta_home() -> PathBuf {
    match env::var("JOLTA_HOME") {
        // A single-quoted export leaves "~" unexpanded; taking it literally
        // would create a directory named "~" in the cwd (volta #484).
        Ok(v) if v == "~" => home_dir(),
        Ok(v) => match v.strip_prefix("~/") {
            Some(rest) => home_dir().join(rest),
            None => PathBuf::from(v),
        },
        Err(_) => home_dir().join(".jolta"),
    }
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

/// Strip the case, separator, trailing-slash and verbatim-prefix differences
/// that make two spellings of one Windows path compare unequal.
#[cfg(windows)]
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s);
    let t = s.trim_end_matches('\\');
    // keep a bare drive root ("c:\") intact
    if t.ends_with(':') { format!("{t}\\") } else { t.to_string() }
}

/// True when `a` and `b` name the same location.
///
/// Windows needs this. PATH entries arrive verbatim from the registry, so they
/// differ from a freshly built PathBuf in case, separator, trailing slash and
/// 8.3 short-name form, while the filesystem treats all of those as one path.
/// `PathBuf::eq` compares components byte-wise and reports false mismatches —
/// which is what made `doctor` call a working install broken.
pub fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    // canonicalize resolves case, short names and symlinks in one go, but only
    // for paths that exist; fall back to a textual compare when they don't.
    if let (Ok(x), Ok(y)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        if x == y {
            return true;
        }
    }
    spelling_fallback(a, b)
}

/// Case-folded compare for paths that do not exist, so canonicalize cannot
/// help — a stale PATH entry is the common case. Windows only: on Unix two
/// different spellings really are two different paths.
#[cfg(windows)]
fn spelling_fallback(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

#[cfg(not(windows))]
fn spelling_fallback(_a: &Path, _b: &Path) -> bool {
    false
}

/// True when `child` is `parent` itself or lies inside it, with the same
/// Windows spelling tolerance as [`same_path`].
pub fn path_starts_with(child: &Path, parent: &Path) -> bool {
    child.starts_with(parent) || child.ancestors().any(|a| same_path(a, parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path always equals itself, and a real directory equals a spelling of
    /// itself that only canonicalize can resolve.
    #[test]
    fn same_path_reflexive_and_canonical() {
        let base = env::temp_dir().join("jolta-samepath-test");
        let sub = base.join("sub");
        fs::create_dir_all(&sub).expect("create temp dirs");
        assert!(same_path(&base, &base));
        // only canonicalize can see through the ".." indirection
        assert!(same_path(&sub.join(".."), &base));
        assert!(!same_path(&base, &sub));
        let _ = fs::remove_dir_all(&base);
    }

    /// path_starts_with must accept the directory itself and anything under
    /// it, and reject a sibling whose name merely shares a prefix — the
    /// "shims" vs "shims-old" case that a raw string compare gets wrong.
    #[test]
    fn path_starts_with_covers_self_and_children() {
        let shims = PathBuf::from("/home/u/.jolta/shims");
        assert!(path_starts_with(&shims, &shims));
        assert!(path_starts_with(&shims.join("java"), &shims));
        assert!(!path_starts_with(Path::new("/home/u/.jolta/shims-old/java"), &shims));
        assert!(!path_starts_with(Path::new("/usr/bin/java"), &shims));
    }

    /// Windows spells one path many ways: PATH comes verbatim out of the
    /// registry while shims_dir() builds a fresh PathBuf, so doctor compared
    /// mismatched spellings and called a working install broken.
    #[cfg(windows)]
    #[test]
    fn windows_spellings_of_one_path_agree() {
        let want = r"c:\users\dave\.jolta\shims";
        for spelling in [
            r"C:\Users\Dave\.jolta\shims",
            r"C:\Users\dave\.jolta\shims\",
            r"\\?\C:\Users\dave\.jolta\shims",
            "C:/Users/dave/.jolta/shims",
        ] {
            assert_eq!(norm(Path::new(spelling)), want, "normalizing {spelling}");
        }
        assert_eq!(norm(Path::new(r"C:\")), r"c:\", "bare drive root");
    }

    /// The case-folded fallback must still apply to paths that do not exist,
    /// which is the common case for a stale PATH entry.
    #[cfg(windows)]
    #[test]
    fn nonexistent_windows_paths_still_compare() {
        assert!(same_path(Path::new(r"C:\Nope\Shims"), Path::new(r"c:\nope\shims\")));
        assert!(path_starts_with(Path::new(r"C:\Nope\Shims\java.exe"), Path::new(r"c:\nope\shims")));
    }
}
