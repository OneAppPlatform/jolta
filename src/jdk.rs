//! Version/spec parsing and JDK discovery (managed + system).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::{home_dir, jolta_home};

/// JDK distros jolta knows how to download. The default is temurin.
pub const INSTALLABLE_VENDORS: [&str; 5] = ["temurin", "corretto", "graalvm", "oracle", "zulu"];
/// Vendors recognized for pinning/matching (superset of the installable ones).
pub const KNOWN_VENDORS: [&str; 6] = ["temurin", "corretto", "graalvm", "oracle", "zulu", "openjdk"];

/// Split a spec into (vendor, version): "corretto@21" / "corretto-21" ->
/// (Some("corretto"), "21"); "21.0.4" -> (None, "21.0.4").
pub fn parse_spec(spec: &str) -> (Option<&'static str>, String) {
    let s = spec.trim();
    for sep in ['@', '-'] {
        if let Some((head, tail)) = s.split_once(sep) {
            let head_lc = head.to_ascii_lowercase();
            if let Some(v) = KNOWN_VENDORS.iter().find(|v| **v == head_lc) {
                return (Some(v), tail.to_string());
            }
        }
    }
    (None, s.to_string())
}

/// Major version of a version string: "21"->21, "21.0.4"->21, "1.8"->8.
pub fn major_of(version: &str) -> Option<u32> {
    let (_, v) = parse_spec(version); // tolerate full specs too
    let first: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
    let first: u32 = first.parse().ok()?;
    if first == 1 {
        // legacy "1.8" style
        let rest = v.split('.').nth(1)?;
        let second: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return second.parse().ok();
    }
    Some(first)
}

/// Is this version an exact point release ("21.0.2") rather than a major
/// ("21")? Legacy "1.8" counts as major 8, not exact.
pub fn is_exact(version: &str) -> bool {
    !version.starts_with("1.") && version.contains('.')
}

/// Sortable key from "21.0.4+7" -> 21_000_004 etc.
pub fn numkey(version: &str) -> u64 {
    let v = version
        .split(|c| c == '+' || c == '_' || c == '-')
        .next()
        .unwrap_or("");
    let mut parts = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let a = parts.next().unwrap_or(0);
    let b = parts.next().unwrap_or(0);
    let c = parts.next().unwrap_or(0);
    a * 1_000_000 + b * 1_000 + c
}

/// Path to a tool inside a JDK home (`.exe`-suffixed on Windows).
pub fn tool_bin(home: &Path, tool: &str) -> PathBuf {
    home.join("bin").join(format!("{tool}{}", std::env::consts::EXE_SUFFIX))
}

/// Does this home actually contain a runnable java?
pub fn has_java(home: &Path) -> bool {
    tool_bin(home, "java").is_file()
}

/// Full JAVA_VERSION from a JDK home's release file.
pub fn jdk_version(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("JAVA_VERSION=\"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

/// Which distro a JDK home is, from its release IMPLEMENTOR or its path.
pub fn vendor_of(home: &Path) -> Option<&'static str> {
    let release = fs::read_to_string(home.join("release")).unwrap_or_default();
    let mut implementor = String::new();
    for line in release.lines() {
        if line.starts_with("GRAALVM_VERSION") {
            return Some("graalvm");
        }
        if let Some(rest) = line.strip_prefix("IMPLEMENTOR=\"") {
            implementor = rest.trim_end_matches('"').to_string();
        }
    }
    let imp = implementor.to_ascii_lowercase();
    if imp.contains("amazon") {
        return Some("corretto");
    }
    if imp.contains("adoptium") || imp.contains("temurin") || imp.contains("eclipse") {
        return Some("temurin");
    }
    if imp.contains("azul") {
        return Some("zulu");
    }
    if imp.contains("homebrew") {
        return Some("openjdk");
    }
    if imp.contains("oracle") {
        return Some("oracle");
    }
    let path = home.to_string_lossy().to_ascii_lowercase();
    for v in ["corretto", "temurin", "zulu", "oracle"] {
        if path.contains(v) {
            return KNOWN_VENDORS.iter().find(|k| **k == v).copied();
        }
    }
    if path.contains("graal") {
        return Some("graalvm");
    }
    if path.contains("openjdk") {
        return Some("openjdk");
    }
    None
}

/// The usable home inside an install dir (handles macOS Contents/Home bundles).
pub fn managed_home(dir: &Path) -> PathBuf {
    let bundled = dir.join("Contents/Home");
    if bundled.is_dir() {
        bundled
    } else {
        dir.to_path_buf()
    }
}

/// jolta-managed JDKs as (fullversion, home) pairs.
pub fn list_managed() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let jdks = jolta_home().join("jdks");
    let Ok(entries) = fs::read_dir(&jdks) else { return out };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let home = managed_home(&dir);
        if let Some(v) = jdk_version(&home) {
            out.push((v, home));
        }
    }
    out
}

/// System JDKs as (fullversion, home) pairs (macOS java_home + common dirs).
pub fn list_system() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if Path::new("/usr/libexec/java_home").exists() {
        // -V prints lines like:  21.0.11 (arm64) "Homebrew" - "OpenJDK" /path
        if let Ok(o) = Command::new("/usr/libexec/java_home").arg("-V").output() {
            let text = String::from_utf8_lossy(&o.stderr);
            for line in text.lines() {
                if !line.contains(" (") || !line.contains(") ") {
                    continue;
                }
                let fields: Vec<&str> = line.split_whitespace().collect();
                if let (Some(first), Some(last)) = (fields.first(), fields.last()) {
                    if last.starts_with('/') {
                        out.push(((*first).to_string(), PathBuf::from(last)));
                    }
                }
            }
        }
    }
    let mut bases = vec![
        PathBuf::from("/usr/lib/jvm"),
        home_dir().join(".sdkman/candidates/java"),
    ];
    // Windows installers drop JDKs under Program Files vendor directories
    for pf_var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(pf) = std::env::var(pf_var) {
            for sub in ["Java", "Eclipse Adoptium", "Amazon Corretto", "Microsoft", "Zulu"] {
                bases.push(PathBuf::from(&pf).join(sub));
            }
        }
    }
    for base in bases {
        let Ok(entries) = fs::read_dir(&base) else { continue };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() || dir.file_name().is_some_and(|n| n == "current") {
                continue;
            }
            if let Some(v) = jdk_version(&dir) {
                out.push((v, dir));
            }
        }
    }
    // CI images (and some setups) export JAVA_HOME_<major>_<arch>=<home>,
    // sometimes with a trailing separator — normalize or paths won't compare
    for (key, value) in std::env::vars() {
        if key.starts_with("JAVA_HOME_") {
            let dir = PathBuf::from(value.trim_end_matches(['/', '\\']));
            if let Some(v) = jdk_version(&dir) {
                out.push((v, dir));
            }
        }
    }
    out
}

pub fn list_all() -> Vec<(String, PathBuf)> {
    let mut all = list_managed();
    all.extend(list_system());
    all
}

/// System default JDK home, used when nothing is pinned and no jolta default
/// set: macOS asks java_home; elsewhere fall back to the newest JDK found.
pub fn system_default() -> Option<PathBuf> {
    if Path::new("/usr/libexec/java_home").exists() {
        if let Ok(o) = Command::new("/usr/libexec/java_home").output() {
            if o.status.success() {
                let home = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
                if !home.as_os_str().is_empty() {
                    return Some(home);
                }
            }
        }
        return None;
    }
    list_all()
        .into_iter()
        .filter(|(_, h)| has_java(h))
        .max_by_key(|(v, _)| numkey(v))
        .map(|(_, h)| h)
}
