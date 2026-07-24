//! Spec -> JDK home resolution: pin lookup, matching, caching, auto-install.

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::install::install_vendor_major;
use crate::jdk::{
    has_java, list_all, major_of, numkey, parse_spec, system_default, vendor_of,
    INSTALLABLE_VENDORS,
};
use crate::paths::jolta_home;
use crate::ui::{die, paint};

/// Best home for a spec: filter by major (and distro when the spec names one,
/// e.g. "corretto-21"); exact full-version match wins, else highest build.
/// Managed JDKs come first in `candidates`, matching the sh implementation.
pub fn best_match(
    candidates: &[(String, PathBuf)],
    vendor: Option<&str>,
    major: u32,
    version: &str,
) -> Option<PathBuf> {
    let mut best: Option<(u64, &PathBuf)> = None;
    let mut exact: Option<&PathBuf> = None;
    for (v, home) in candidates {
        if major_of(v) != Some(major) {
            continue;
        }
        if let Some(want) = vendor {
            if vendor_of(home) != Some(want) {
                continue;
            }
        }
        if v == version {
            exact = Some(home);
        }
        let k = numkey(v);
        if best.map_or(true, |(bk, _)| k >= bk) {
            best = Some((k, home));
        }
    }
    exact.or(best.map(|(_, h)| h)).cloned()
}

fn cache_path(spec: &str) -> PathBuf {
    let safe: String = spec
        .chars()
        .map(|c| if c == '/' || c == ' ' { '_' } else { c })
        .collect();
    jolta_home().join("cache").join(format!("v-{safe}"))
}

pub fn clear_cache() {
    let _ = fs::remove_dir_all(jolta_home().join("cache"));
}

/// Resolve a version spec to a JDK home.
/// NOTE: java_home -v is unusable as a fallback — it exits 0 and prints its
/// default JDK even when nothing matches — so matching is strict, list-based.
pub fn resolve(spec: &str) -> Option<PathBuf> {
    let (vendor, version) = parse_spec(spec);
    let major = major_of(&version)?;
    let cache = cache_path(spec);
    if let Ok(cached) = fs::read_to_string(&cache) {
        let home = PathBuf::from(cached.trim());
        if has_java(&home) {
            return Some(home);
        }
        let _ = fs::remove_file(&cache);
    }
    let home = best_match(&list_all(), vendor, major, &version)?;
    if !has_java(&home) {
        return None;
    }
    let _ = fs::create_dir_all(jolta_home().join("cache"));
    let _ = fs::write(&cache, format!("{}\n", home.display()));
    Some(home)
}

pub struct Pin {
    pub spec: Option<String>,
    pub source: String,
}

/// Find the nearest .java-version walking up from cwd; then default file.
pub fn read_pin() -> Pin {
    if let Ok(v) = env::var("JOLTA_JAVA_VERSION") {
        if !v.trim().is_empty() {
            return Pin {
                spec: Some(v.trim().to_string()),
                source: "JOLTA_JAVA_VERSION environment variable".into(),
            };
        }
    }
    let mut dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    loop {
        let f = dir.join(".java-version");
        if f.is_file() {
            if let Ok(text) = fs::read_to_string(&f) {
                let spec = text.lines().next().unwrap_or("").trim().to_string();
                if !spec.is_empty() {
                    return Pin {
                        spec: Some(spec),
                        source: f.display().to_string(),
                    };
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }
    let default = jolta_home().join("default");
    if let Ok(text) = fs::read_to_string(&default) {
        let spec = text.lines().next().unwrap_or("").trim().to_string();
        if !spec.is_empty() {
            return Pin {
                spec: Some(spec),
                source: format!("jolta default ({})", default.display()),
            };
        }
    }
    Pin {
        spec: None,
        source: "system default (no .java-version found, no jolta default set)".into(),
    }
}

/// Download the pinned JDK on demand, Volta-style. `auto_install` is true only
/// for shims / exec / env — never the cd hook — and JOLTA_NO_AUTO_INSTALL wins.
fn try_auto_install(spec: &str) -> bool {
    if env::var("JOLTA_NO_AUTO_INSTALL").is_ok() {
        return false;
    }
    let (vendor, version) = parse_spec(spec);
    let vendor = vendor.unwrap_or("temurin");
    let Some(major) = major_of(&version) else { return false };
    if !INSTALLABLE_VENDORS.contains(&vendor) {
        eprintln!(
            "{} cannot auto-install '{vendor}' builds (downloadable distros: {})",
            paint("33", "jolta:", true),
            INSTALLABLE_VENDORS.join(", ")
        );
        return false;
    }
    eprintln!(
        "{} Java {spec} is pinned here but not installed — fetching {vendor} {major} {}",
        paint("33", "jolta:", true),
        paint("2", "(set JOLTA_NO_AUTO_INSTALL=1 to disable)", true)
    );
    install_vendor_major(vendor, major).is_ok()
}

pub struct Resolved {
    pub home: PathBuf,
    pub source: String,
}

pub fn resolve_current(auto_install: bool) -> Resolved {
    let pin = read_pin();
    match pin.spec {
        Some(spec) => {
            let mut home = resolve(&spec);
            if home.is_none() && auto_install && try_auto_install(&spec) {
                home = resolve(&spec);
            }
            let home = home.unwrap_or_else(|| {
                let installed: Vec<String> = {
                    let mut vs: Vec<String> = list_all()
                        .into_iter()
                        .map(|(v, h)| match vendor_of(&h) {
                            Some(ven) => format!("{ven}-{v}"),
                            None => v,
                        })
                        .collect();
                    vs.sort();
                    vs.dedup();
                    vs
                };
                die(&format!(
                    "no installed JDK matches '{spec}' (pinned by {})\n  installed JDKs: {}\n  \
                     run 'jolta install {spec}' to download it, or 'jolta list' to see what's available",
                    pin.source,
                    installed.join(" ")
                ));
            });
            Resolved { home, source: pin.source }
        }
        None => {
            let home = system_default().unwrap_or_else(|| {
                die(
                    "no Java version pinned and no system JDK found\n  pin one with 'jolta pin <version>' \
                     or set a global default with 'jolta default <version>'",
                )
            });
            Resolved { home, source: pin.source }
        }
    }
}
