//! Version/spec parsing and JDK discovery (managed + system).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::jolta_home;

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

/// Sortable key from up to four dotted components ("21.0.4+7", corretto's
/// five-part "21.0.2.13.1" — where only the 4th distinguishes point builds).
/// Legacy update numbers ("1.8.0_392", "8u392") count as components — they
/// are the ONLY thing distinguishing JDK 8 builds, so splitting them away
/// would make every 8 build compare equal and upgrades no-op (mise #839).
/// Build metadata (+13) and prerelease tags (-ea) are insignificant.
pub fn numkey(version: &str) -> u64 {
    let v = version.split(['+', '-']).next().unwrap_or("").replace(['u', '_'], ".");
    let mut parts = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0).min(999));
    let a = parts.next().unwrap_or(0);
    let b = parts.next().unwrap_or(0);
    let c = parts.next().unwrap_or(0);
    let d = parts.next().unwrap_or(0);
    ((a * 1_000 + b) * 1_000 + c) * 1_000 + d
}

/// Path to a tool inside a JDK home (`.exe`-suffixed on Windows).
pub fn tool_bin(home: &Path, tool: &str) -> PathBuf {
    home.join("bin").join(format!("{tool}{}", std::env::consts::EXE_SUFFIX))
}

/// Does this home actually contain a runnable java?
pub fn has_java(home: &Path) -> bool {
    tool_bin(home, "java").is_file()
}

/// Full JAVA_VERSION from a JDK home's release file. Values are usually
/// quoted, but not always (jenv #385) — tolerate both.
pub fn jdk_version(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("JAVA_VERSION=") {
            let v = rest.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Version guess from an install-dir name ("8.0.392-tem" -> "8.0.392",
/// "java-1.8.0-openjdk-amd64" -> "1.8.0", "jdk1.8.0_392" -> "1.8.0_392").
/// Fallback for JDKs that ship no release file — notably many JDK 8 builds.
pub fn version_from_name(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let start = name.find(|c: char| c.is_ascii_digit())?;
    let v: String = name[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '_')
        .collect();
    let v = v.trim_end_matches(['.', '_']).to_string();
    if v.is_empty() { None } else { Some(v) }
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
    // sdkman install dirs encode the vendor as a suffix ("8.0.392-tem");
    // without this, .sdkmanrc pins like "java=8.0.392-tem" can see the JDK
    // sdkman installed but never vendor-match it.
    if let Some(name) = home.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()) {
        if let Some((_, suffix)) = name.rsplit_once('-') {
            let v = match suffix {
                "tem" => "temurin",
                "amzn" => "corretto",
                "zulu" => "zulu",
                "graal" | "graalce" => "graalvm",
                "oracle" => "oracle",
                "open" => "openjdk",
                _ => "",
            };
            if !v.is_empty() {
                return KNOWN_VENDORS.iter().find(|k| **k == v).copied();
            }
        }
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

/// The usable home inside an install dir (handles macOS Contents/Home
/// bundles, including Zulu's nested "zulu-17.jdk/Contents/Home" wrapper —
/// vendors reshuffle these layouts across releases, mise #9337).
pub fn managed_home(dir: &Path) -> PathBuf {
    let bundled = dir.join("Contents/Home");
    if bundled.is_dir() {
        return bundled;
    }
    if !has_java(dir) {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let nested = e.path().join("Contents/Home");
                if nested.is_dir() && has_java(&nested) {
                    return nested;
                }
            }
        }
    }
    dir.to_path_buf()
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
        // Hand-placed JDKs may lack a release file (common for JDK 8):
        // fall back to the "vendor-version" dir name.
        let v = jdk_version(&home)
            .or_else(|| if has_java(&home) { version_from_name(&dir) } else { None });
        if let Some(v) = v {
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
    let mut bases = vec![PathBuf::from("/usr/lib/jvm")];
    // The .sdkman scan is best-effort: shims must keep working in HOME-less
    // environments (cron, containers), so never die over a missing HOME here.
    if let Ok(h) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        bases.push(PathBuf::from(h).join(".sdkman/candidates/java"));
    }
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
            // macOS bundle layouts (dir/Contents/Home) appear in these scan
            // dirs too, not just managed installs (sdkman #1490 family)
            let home = managed_home(&dir);
            // No release file (many JDK 8 builds): derive from the dir name
            let v = jdk_version(&home)
                .or_else(|| if has_java(&home) { version_from_name(&dir) } else { None });
            if let Some(v) = v {
                out.push((v, home));
            }
        }
    }
    // CI images (and some setups) export JAVA_HOME_<major>_<arch>=<home>,
    // sometimes with a trailing separator — normalize or paths won't compare
    for (key, value) in std::env::vars() {
        if key.starts_with("JAVA_HOME_") {
            let dir = PathBuf::from(value.trim_end_matches(['/', '\\']));
            let v = jdk_version(&dir)
                .or_else(|| if has_java(&dir) { version_from_name(&dir) } else { None });
            if let Some(v) = v {
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
/// set: macOS asks java_home first, but managed JDKs are invisible to it, so
/// when it has nothing (or off macOS) fall back to the newest JDK found.
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
    }
    list_all()
        .into_iter()
        .filter(|(_, h)| has_java(h))
        .max_by_key(|(v, _)| numkey(v))
        .map(|(_, h)| h)
}
