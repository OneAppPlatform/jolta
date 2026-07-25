//! Spec -> JDK home resolution: pin lookup, matching, caching, auto-install.

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::install::{install_vendor_spec, release_universe};
use crate::jdk::{
    has_java, is_exact, list_all, major_of, numkey, parse_spec, system_default, vendor_of,
    INSTALLABLE_VENDORS,
};
use crate::paths::jolta_home;
use crate::ui::{die, paint};

/// Newest LTS major, used only when the Adoptium metadata endpoint is
/// unreachable but downloads still work (e.g. JOLTA_DOWNLOAD_BASE mirrors).
const FALLBACK_LTS: u32 = 25;

/// Best home for a spec: filter by major (and distro when the spec names one,
/// e.g. "corretto-21"); exact full-version match wins, else highest build.
/// Managed JDKs come first in `candidates`, matching the sh implementation.
pub fn best_match(
    candidates: &[(String, PathBuf)],
    vendor: Option<&str>,
    major: u32,
    version: &str,
) -> Option<PathBuf> {
    // Pins ending in -ea accept only EA builds; GA pins prefer GA and fall
    // back to EA only when nothing else provides the major (mise #6907:
    // never satisfy an EA request silently with a GA build or vice versa).
    let want_ea = version.contains("-ea");
    let mut best: Option<(u64, &PathBuf)> = None; // GA candidates
    let mut best_ea: Option<(u64, &PathBuf)> = None;
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
        let is_ea = v.contains("-ea");
        if want_ea && !is_ea {
            continue;
        }
        // Exact matches compare numerically, so "21.0.2+13" == "21.0.2" and
        // "21.0.0" == "21" (vendors publish both notations, mise #839/#2726).
        if exact.is_none() && numkey(v) == numkey(version) {
            exact = Some(home);
        }
        let k = numkey(v);
        let slot = if is_ea { &mut best_ea } else { &mut best };
        // strict >: ties keep the EARLIER candidate — managed JDKs come first
        // in `candidates`, so a managed JDK beats a same-version system one
        if slot.map_or(true, |(bk, _)| k > bk) {
            *slot = Some((k, home));
        }
    }
    // An exact spec means exact: never satisfy "21.0.2" with some other 21.x
    if is_exact(version) {
        return exact.cloned();
    }
    // A major pin picks the highest build; the bare-GA "21" release that
    // happens to string-match the pin must not shadow 21.0.x (mise #1887).
    best.or(best_ea).map(|(_, h)| h.clone())
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
/// Only jolta-managed homes are cached: system/env-registered JDKs can be
/// deregistered without the directory disappearing (e.g. unsetting a
/// JAVA_HOME_<major> variable), which a has_java check can't detect.
pub fn resolve(spec: &str) -> Option<PathBuf> {
    let (vendor, version) = parse_spec(spec);
    let major = major_of(&version)?;
    let managed_root = jolta_home().join("jdks");
    let cache = cache_path(spec);
    if let Ok(cached) = fs::read_to_string(&cache) {
        let home = PathBuf::from(cached.trim());
        if home.starts_with(&managed_root) && has_java(&home) {
            return Some(home);
        }
        let _ = fs::remove_file(&cache);
    }
    let home = best_match(&list_all(), vendor, major, &version)?;
    if !has_java(&home) {
        return None;
    }
    if home.starts_with(&managed_root) {
        let _ = fs::create_dir_all(jolta_home().join("cache"));
        let _ = fs::write(&cache, format!("{}\n", home.display()));
    }
    Some(home)
}

/// "java=21.0.2-tem" -> "temurin@21.0.2". Unknown/absent suffixes become
/// vendorless specs (best matching installed distro).
/// Parsing matches sdkman's own normalisation: strip from the first '#' to
/// end of line, then delete ALL whitespace — so "java = 21.0.2-tem # ci"
/// is legal there and must be here too (CRLF falls out for free).
fn parse_sdkmanrc(text: &str) -> Option<String> {
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line: String = line
            .split('#')
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let Some(value) = line.strip_prefix("java=") else { continue };
        if value.is_empty() {
            continue;
        }
        let (version, suffix) = match value.rsplit_once('-') {
            Some((v, s)) => (v, s),
            None => (value, ""),
        };
        let vendor = crate::jdk::sdkman_suffix_vendor(suffix);
        return Some(if vendor.is_empty() {
            // unknown distro suffix: match on version across any distro
            version.to_string()
        } else {
            format!("{vendor}@{version}")
        });
    }
    None
}

pub struct Pin {
    pub spec: Option<String>,
    pub source: String,
}

/// First spec token of a version file: skips blank and `#`-comment lines,
/// strips a UTF-8 BOM, and takes the first whitespace-separated token (so
/// "21 # team standard" pins 21). Windows editors add BOMs and humans add
/// blank lines and comments — none of that should silently unpin a project.
/// A `#` glued to the version ("21#x") is kept and fails resolution loudly.
fn first_spec_line(text: &str) -> Option<String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .find(|t| !t.starts_with('#'))
        .map(str::to_string)
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
                if let Some(spec) = first_spec_line(&text) {
                    return Pin {
                        spec: Some(spec),
                        source: f.display().to_string(),
                    };
                }
            }
        }
        // SDKMAN migration: honor .sdkmanrc (java=21.0.2-tem) when no
        // .java-version claims this directory
        let f = dir.join(".sdkmanrc");
        if f.is_file() {
            if let Ok(text) = fs::read_to_string(&f) {
                if let Some(spec) = parse_sdkmanrc(&text) {
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
        if let Some(spec) = first_spec_line(&text) {
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
    let Some(major) = major_of(&version) else { return false };
    // Vendorless spec: stay with a vendor that already provides this major
    // (a corretto shop pinning "21.0.9" should not silently gain a temurin).
    let vendor = vendor
        .or_else(|| {
            list_all()
                .iter()
                .filter(|(v, _)| major_of(v) == Some(major))
                .find_map(|(_, h)| vendor_of(h))
                .filter(|v| INSTALLABLE_VENDORS.contains(v))
        })
        .unwrap_or("temurin");
    if !INSTALLABLE_VENDORS.contains(&vendor) {
        eprintln!(
            "{} cannot auto-install '{vendor}' builds (downloadable distros: {})",
            paint("33", "jolta:", true),
            INSTALLABLE_VENDORS.join(", ")
        );
        return false;
    }
    let _ = major;
    eprintln!(
        "{} Java {spec} is pinned here but not installed — fetching {vendor} {version} {}",
        paint("33", "jolta:", true),
        paint("2", "(set JOLTA_NO_AUTO_INSTALL=1 to disable)", true)
    );
    install_vendor_spec(vendor, &version).is_ok()
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
            if let Some(home) = system_default() {
                return Resolved { home, source: pin.source };
            }
            // Fresh machine: no pin, no default, no JDK anywhere. Hands-off
            // means `java` still works — fetch the latest LTS and make it
            // the global default.
            if auto_install && env::var("JOLTA_NO_AUTO_INSTALL").is_err() {
                let major = release_universe().map_or(FALLBACK_LTS, |(_, _, lts, _)| lts);
                let spec = major.to_string();
                eprintln!(
                    "{} no Java pinned and none installed — fetching Temurin {major} (latest LTS) and setting it as your default {}",
                    paint("33", "jolta:", true),
                    paint("2", "(set JOLTA_NO_AUTO_INSTALL=1 to disable)", true)
                );
                if install_vendor_spec("temurin", &spec).is_ok() {
                    let _ = fs::create_dir_all(jolta_home());
                    let _ = fs::write(jolta_home().join("default"), format!("{spec}\n"));
                    if let Some(home) = resolve(&spec) {
                        return Resolved {
                            home,
                            source: format!("jolta default ({spec}, latest LTS, installed on first run)"),
                        };
                    }
                }
            }
            die(
                "no Java version pinned and no system JDK found\n  pin one with 'jolta pin <version>' \
                 or set a global default with 'jolta default <version>'",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_sdkmanrc;

    /// Per-vendor .sdkmanrc suffix contract — one assertion per vendor.
    #[test]
    fn sdkmanrc_vendor_suffixes() {
        for (suffix, vendor) in [
            ("tem", "temurin"),
            ("amzn", "corretto"),
            ("zulu", "zulu"),
            ("oracle", "oracle"),
            ("graal", "graalvm"),
            ("graalce", "graalce"),
            ("librca", "liberica"),
            ("sapmchn", "sapmachine"),
            ("sem", "semeru"),
            ("ms", "microsoft"),
            ("albba", "dragonwell"),
            ("open", "openjdk"),
        ] {
            assert_eq!(
                parse_sdkmanrc(&format!("java=21.0.4-{suffix}\n")),
                Some(format!("{vendor}@21.0.4")),
                "suffix -{suffix}"
            );
        }
    }

    #[test]
    fn sdkmanrc_unknown_suffix_is_vendorless() {
        assert_eq!(parse_sdkmanrc("java=21.0.4-wibble\n"), Some("21.0.4".into()));
    }
}
