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
pub const FALLBACK_LTS: u32 = 25;

/// An installed JDK that was considered and not chosen, with the reason.
/// Only ever collected when the caller explicitly asks (`--explain`): the
/// resolution path runs on every shim exec, so it allocates nothing here.
pub struct Rejection {
    pub version: String,
    pub vendor: Option<String>,
    pub home: PathBuf,
    pub reason: String,
}

/// Best home for a spec: filter by major (and distro when the spec names one,
/// e.g. "corretto-21"); exact full-version match wins, else highest build.
/// Managed JDKs come first in `candidates`, matching the sh implementation.
pub fn best_match(
    candidates: &[(String, PathBuf)],
    vendor: Option<&str>,
    major: u32,
    version: &str,
    preferred: Option<&str>,
) -> Option<PathBuf> {
    best_match_inner(candidates, vendor, major, version, preferred, None)
}

/// Same walk as `best_match`, plus every candidate it passed over and why.
/// Deliberately the SAME function rather than a parallel one: an explanation
/// reconstructed by a second code path can drift from the decision it claims
/// to describe, and then you are debugging the explainer.
pub fn explain_match(
    candidates: &[(String, PathBuf)],
    vendor: Option<&str>,
    major: u32,
    version: &str,
    preferred: Option<&str>,
) -> (Option<PathBuf>, Vec<Rejection>) {
    let mut rejected = Vec::new();
    let home = best_match_inner(candidates, vendor, major, version, preferred, Some(&mut rejected));
    (home, rejected)
}

fn best_match_inner(
    candidates: &[(String, PathBuf)],
    vendor: Option<&str>,
    major: u32,
    version: &str,
    preferred: Option<&str>,
    mut explain: Option<&mut Vec<Rejection>>,
) -> Option<PathBuf> {
    // Records a skipped candidate; a no-op (and no allocation) when the caller
    // didn't ask for an explanation.
    macro_rules! reject {
        ($v:expr, $home:expr, $reason:expr) => {
            if let Some(out) = explain.as_deref_mut() {
                out.push(Rejection {
                    version: $v.clone(),
                    vendor: vendor_of($home).map(str::to_string),
                    home: $home.clone(),
                    reason: $reason,
                });
            }
        };
    }
    // Everything that survived filtering, to label after a winner is known.
    let mut considered: Vec<(&String, &PathBuf)> = Vec::new();
    // Precedence for vendorless specs, ONE ladder shared by every consumer
    // (shims, which/home/current, list, prune, auto-install):
    //   preferred-vendor GA > any GA > preferred-vendor EA > any EA,
    // highest build within each tier. The preferred vendor beats even a
    // HIGHER build of another vendor — a corretto shop pinning "11" gets
    // corretto 11.0.31 over temurin 11.0.32. Explicit spec vendors filter
    // hard and make the preference irrelevant. EA gates as before
    // (mise #6907: never satisfy an EA request with GA or vice versa).
    let want_ea = version.contains("-ea");
    let mut best: Option<(u64, &PathBuf)> = None; // GA, any vendor
    let mut best_pref: Option<(u64, &PathBuf)> = None; // GA, preferred vendor
    let mut best_ea: Option<(u64, &PathBuf)> = None;
    let mut best_pref_ea: Option<(u64, &PathBuf)> = None;
    let mut exact: Option<&PathBuf> = None;
    let mut exact_pref: Option<&PathBuf> = None;
    for (v, home) in candidates {
        if major_of(v) != Some(major) {
            reject!(v, home, format!("Java {} — spec asks for {major}", major_of(v).map_or("?".into(), |m| m.to_string())));
            continue;
        }
        if let Some(want) = vendor {
            if vendor_of(home) != Some(want) {
                reject!(v, home, format!("distro is not '{want}' (spec names a distro)"));
                continue;
            }
        }
        let is_ea = v.contains("-ea");
        if want_ea && !is_ea {
            reject!(v, home, "spec asks for an early-access build; this is GA".to_string());
            continue;
        }
        if explain.is_some() {
            considered.push((v, home));
        }
        let is_pref = vendor.is_none() && preferred.is_some() && vendor_of(home) == preferred;
        // Exact matches compare numerically, so "21.0.2+13" == "21.0.2" and
        // "21.0.0" == "21" (vendors publish both notations, mise #839/#2726).
        if numkey(v) == numkey(version) {
            if is_pref && exact_pref.is_none() {
                exact_pref = Some(home);
            }
            if exact.is_none() {
                exact = Some(home);
            }
        }
        let k = numkey(v);
        let slot = match (is_pref, is_ea) {
            (true, false) => &mut best_pref,
            (false, false) => &mut best,
            (true, true) => &mut best_pref_ea,
            (false, true) => &mut best_ea,
        };
        // strict >: ties keep the EARLIER candidate — managed JDKs come first
        // in `candidates`, so a managed JDK beats a same-version system one
        if slot.map_or(true, |(bk, _)| k > bk) {
            *slot = Some((k, home));
        }
    }
    // An exact spec means exact: never satisfy "21.0.2" with some other 21.x
    let winner = if is_exact(version) {
        exact_pref.or(exact).cloned()
    } else {
        // A major pin picks the highest build within the winning tier; the
        // bare-GA "21" release must not shadow 21.0.x (mise #1887).
        best_pref.or(best).or(best_pref_ea).or(best_ea).map(|(_, h)| h.clone())
    };
    // Label the survivors that lost. The near-miss case matters most: an exact
    // pin against a JDK one point release away resolves to nothing, and "there
    // is a 17.0.19 here" is the sentence the user actually needs.
    if explain.is_some() {
        let win_vendor = winner.as_deref().and_then(vendor_of);
        for (v, home) in considered {
            if Some(home) == winner.as_ref() {
                continue;
            }
            let reason = if is_exact(version) && numkey(v) != numkey(version) {
                format!("not {version} — spec is exact, so a different build of {major} cannot satisfy it")
            } else if winner.is_none() {
                "no candidate satisfied the spec".to_string()
            } else if vendor.is_none()
                && preferred.is_some()
                && win_vendor == preferred
                && vendor_of(home) != preferred
            {
                format!(
                    "distro '{}' loses to preferred distro '{}' (JOLTA_VENDOR / jolta vendor)",
                    vendor_of(home).unwrap_or("?"),
                    preferred.unwrap_or("?")
                )
            } else if let Some(w) = winner.as_ref().and_then(|h| version_at(candidates, h)) {
                if numkey(v) == numkey(w) {
                    // Same build, different distro: `best_match` keeps the
                    // EARLIER candidate, and managed JDKs are listed first.
                    format!(
                        "same build as the selected {} {w} — ties go to the earlier candidate (jolta-managed JDKs are considered first)",
                        win_vendor.unwrap_or("?")
                    )
                } else {
                    format!("build {v} is older than the selected {w}")
                }
            } else {
                "outranked by the selected build".to_string()
            };
            reject!(v, home, reason);
        }
    }
    winner
}

/// The version string `candidates` carries for a home — used only to name the
/// winner in an explanation.
fn version_at<'a>(candidates: &'a [(String, PathBuf)], home: &PathBuf) -> Option<&'a str> {
    candidates.iter().find(|(_, h)| h == home).map(|(v, _)| v.as_str())
}

fn cache_path(spec: &str) -> PathBuf {
    let safe: String = spec
        .chars()
        .map(|c| if c == '/' || c == ' ' { '_' } else { c })
        .collect();
    // the preference changes what a vendorless spec resolves to, so it is
    // part of the cache key — else toggling JOLTA_VENDOR serves stale answers
    let pref = crate::jdk::preferred_vendor().map(|v| format!("-p-{v}")).unwrap_or_default();
    jolta_home().join("cache").join(format!("v-{safe}{pref}"))
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
    let home = best_match(&list_all(), vendor, major, &version, crate::jdk::preferred_vendor())?;
    if !has_java(&home) {
        return None;
    }
    if home.starts_with(&managed_root) {
        let _ = fs::create_dir_all(jolta_home().join("cache"));
        let _ = fs::write(&cache, format!("{}\n", home.display()));
    }
    Some(home)
}

/// Resolution for the current directory, plus every installed JDK the walk
/// passed over and why. Opt-in only (`--explain`): it deliberately BYPASSES
/// the resolution cache, because a cache hit performs no walk — and an
/// explanation of a walk that never happened is fiction, not evidence.
/// Identity of the candidate set a decision was made against. A rejection is
/// only evidence if the universe it was rejected from is named: install or
/// remove a JDK and the same spec can produce a different — equally correct —
/// explanation, with nothing in the output to mark that the ground moved.
/// (Raised by @cwahq on Moltbook: "rejected candidates are evidence only when
/// the universe they were rejected from is named too.")
pub struct Inventory {
    pub count: usize,
    pub digest: String,
    /// What entered and left since the last --explain. A digest tells you the
    /// footing moved; only a diff tells you which way. (@groutboy on Moltbook:
    /// "the hash is the gate, the diff is the thing an operator can act on.")
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// True the first time we ever look. Nothing to diff against, and calling
    /// every JDK on the machine "added" would be a lie dressed as a report.
    pub first_seen: bool,
}

fn inventory_keys(candidates: &[(String, PathBuf)]) -> Vec<String> {
    let mut keys: Vec<String> = candidates
        .iter()
        .map(|(v, h)| match vendor_of(h) {
            Some(ven) => format!("{ven}-{v}"),
            None => v.clone(),
        })
        .collect();
    keys.sort(); // order of discovery must not change the identity
    keys.dedup();
    keys
}

/// Identity plus drift. The snapshot lives beside the caches and is refreshed
/// here, so a change is reported ONCE and the new state becomes the baseline —
/// re-running --explain immediately after shows no diff. That is a tripwire,
/// not a log; if you need the history, keep the output.
fn inventory_id(candidates: &[(String, PathBuf)]) -> Inventory {
    let keys = inventory_keys(candidates);
    let mut h: u64 = 5381;
    for k in &keys {
        for b in k.bytes() {
            h = h.wrapping_mul(33) ^ b as u64;
        }
    }
    let snap = jolta_home().join("inventory");
    let previous: Option<Vec<String>> = fs::read_to_string(&snap)
        .ok()
        .map(|t| t.lines().filter(|l| !l.is_empty()).map(str::to_string).collect());
    let (added, removed, first_seen) = match &previous {
        None => (Vec::new(), Vec::new(), true),
        Some(prev) => (
            keys.iter().filter(|k| !prev.contains(k)).cloned().collect(),
            prev.iter().filter(|k| !keys.contains(k)).cloned().collect(),
            false,
        ),
    };
    if previous.as_deref() != Some(keys.as_slice()) {
        let _ = fs::create_dir_all(jolta_home());
        let _ = fs::write(&snap, format!("{}\n", keys.join("\n")));
    }
    Inventory { count: candidates.len(), digest: format!("{h:016x}"), added, removed, first_seen }
}

pub struct Explanation {
    pub pin: Pin,
    pub home: Option<PathBuf>,
    pub rejected: Vec<Rejection>,
    pub inventory: Inventory,
    pub resolver: &'static str,
}

pub fn explain_current() -> Explanation {
    let pin = read_pin();
    let candidates = list_all();
    let inventory = inventory_id(&candidates);
    let resolver = env!("CARGO_PKG_VERSION");
    let Some(spec) = pin.spec.clone() else {
        return Explanation {
            pin,
            home: system_default(),
            rejected: Vec::new(),
            inventory,
            resolver,
        };
    };
    let (vendor, version) = parse_spec(&spec);
    let Some(major) = major_of(&version) else {
        return Explanation { pin, home: None, rejected: Vec::new(), inventory, resolver };
    };
    let (home, rejected) =
        explain_match(&candidates, vendor, major, &version, crate::jdk::preferred_vendor());
    Explanation { pin, home: home.filter(|h| has_java(h)), rejected, inventory, resolver }
}

/// "java=21.0.2-tem" -> ("temurin@21.0.2", "21.0.2-tem"): the jolta spec, plus
/// the raw sdkman identifier, which names sdkman's own install directory
/// exactly and so resolves builds whose version the ID spells differently.
/// Unknown/absent suffixes become vendorless specs (best matching installed
/// distro). Parsing matches sdkman's own normalisation: strip from the first
/// '#' to end of line, then delete ALL whitespace — so "java = 21.0.2-tem # ci"
/// is legal there and must be here too (CRLF falls out for free).
fn parse_sdkmanrc(text: &str) -> Option<(String, String)> {
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
        let spec = if vendor.is_empty() {
            // unknown distro suffix: match on version across any distro
            version.to_string()
        } else {
            format!("{vendor}@{version}")
        };
        return Some((spec, value.to_string()));
    }
    None
}

pub struct Pin {
    pub spec: Option<String>,
    pub source: String,
    /// The raw sdkman identifier when the pin came from `.sdkmanrc`, so
    /// resolution can fall back to sdkman's own install directory instead of
    /// downloading a JDK that is already on the machine.
    pub sdkman_id: Option<String>,
}

fn pin_key(path: &str) -> String {
    let mut h: u64 = 5381;
    for b in path.bytes() {
        h = h.wrapping_mul(33) ^ b as u64;
    }
    format!("{h:016x}")
}

/// Remember file pins ($JOLTA_HOME/pins/, one entry per pin file) so prune
/// can protect exact-pinned builds. Refreshed only when changed — the happy
/// path costs one small read.
fn remember_pin(source: &str, spec: &str) {
    if !(source.starts_with('/') || source.get(1..3) == Some(":\\")) {
        return; // env-var / default sources aren't files
    }
    let f = jolta_home().join("pins").join(pin_key(source));
    let entry = format!("{source}\n{spec}\n");
    if fs::read_to_string(&f).is_ok_and(|cur| cur == entry) {
        return;
    }
    let _ = fs::create_dir_all(jolta_home().join("pins"));
    let _ = fs::write(&f, entry);
}

/// Live (spec, pin-file-path) pairs from every remembered pin. The registry
/// is only a hint: each pin file is re-read NOW, and entries whose file is
/// gone or no longer pins anything are dropped.
pub fn remembered_pin_specs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(jolta_home().join("pins")) else { return out };
    for e in entries.flatten() {
        let Some(path) = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| t.lines().next().map(str::to_string))
        else {
            let _ = fs::remove_file(e.path());
            continue;
        };
        let spec = fs::read_to_string(&path).ok().and_then(|text| {
            if path.ends_with(".sdkmanrc") {
                parse_sdkmanrc(&text).map(|(spec, _)| spec)
            } else {
                first_spec_line(&text)
            }
        });
        match spec {
            Some(s) => out.push((s, path)),
            None => {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    out
}

/// Does this pin spec protect an installed (vendor, version) build from
/// pruning? Only exact pins do — a major pin resolves to the newest build
/// anyway, which prune always keeps.
pub fn pin_protects(spec: &str, vendor: &str, version: &str) -> bool {
    let (pin_vendor, pin_version) = parse_spec(spec);
    if !is_exact(&pin_version) {
        return false;
    }
    if pin_vendor.is_some_and(|pv| pv != vendor) {
        return false;
    }
    numkey(&pin_version) == numkey(version) && major_of(&pin_version) == major_of(version)
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
/// The project pin is authoritative — there is deliberately no env-var
/// override (Volta-style): an exported override in some forgotten profile
/// would silently beat every pin on the machine.
pub fn read_pin() -> Pin {
    let mut dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    loop {
        let f = dir.join(".java-version");
        if f.is_file() {
            if let Ok(text) = fs::read_to_string(&f) {
                if let Some(spec) = first_spec_line(&text) {
                    return Pin {
                        spec: Some(spec),
                        source: f.display().to_string(),
                        sdkman_id: None,
                    };
                }
            }
        }
        // SDKMAN migration: honor .sdkmanrc (java=21.0.2-tem) when no
        // .java-version claims this directory
        let f = dir.join(".sdkmanrc");
        if f.is_file() {
            if let Ok(text) = fs::read_to_string(&f) {
                if let Some((spec, id)) = parse_sdkmanrc(&text) {
                    return Pin {
                        spec: Some(spec),
                        source: f.display().to_string(),
                        sdkman_id: Some(id),
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
                sdkman_id: None,
            };
        }
    }
    Pin {
        spec: None,
        source: "system default (no .java-version found, no jolta default set)".into(),
        sdkman_id: None,
    }
}

/// Download the pinned JDK on demand, Volta-style. `auto_install` is true only
/// for shims / exec / env — never the shell hook — and JOLTA_NO_AUTO_INSTALL wins.
fn try_auto_install(spec: &str) -> bool {
    if env::var("JOLTA_NO_AUTO_INSTALL").is_ok() {
        return false;
    }
    let (vendor, version) = parse_spec(spec);
    let Some(major) = major_of(&version) else { return false };
    // Vendorless spec: stay with a vendor that already provides this major
    // (a corretto shop pinning "21.0.9" should not silently gain a temurin).
    // Order: explicit spec > configured preference > vendor already providing
    // this major > temurin.
    let vendor = vendor
        .or_else(|| crate::jdk::preferred_vendor().filter(|v| INSTALLABLE_VENDORS.contains(v)))
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
            // An sdkman ID does not always spell the version the JDK reports —
            // Corretto's "21.0.5.11.1-amzn" against a release file that says
            // 21.0.5 — so a version-based match can miss a JDK sdkman already
            // installed. Its install directory is named after the ID, which is
            // exact. Tried only after normal resolution, so a jolta-managed JDK
            // still wins when it satisfies the pin, and always before
            // downloading a second copy of a JDK that is already here.
            if home.is_none() {
                home = pin.sdkman_id.as_deref().and_then(crate::jdk::sdkman_candidate);
            }
            if home.is_none() && auto_install && try_auto_install(&spec) {
                home = resolve(&spec);
            }
            if home.is_some() {
                remember_pin(&pin.source, &spec);
            }
            let home = home.unwrap_or_else(|| {
                let all = list_all();
                let installed: Vec<String> = {
                    let mut vs: Vec<String> = all
                        .iter()
                        .map(|(v, h)| match vendor_of(h) {
                            Some(ven) => format!("{ven}-{v}"),
                            None => v.clone(),
                        })
                        .collect();
                    vs.sort();
                    vs.dedup();
                    vs
                };
                // The near miss is the sentence the reader actually needs, and
                // it belongs in the DEFAULT failure — not behind --explain,
                // which nobody types before they know they need it. An exact
                // pin against a build one release away is the case where
                // "here is what's installed" reads as a taunt rather than help.
                let (want_vendor, want_version) = parse_spec(&spec);
                let near: Vec<String> = major_of(&want_version)
                    .filter(|_| is_exact(&want_version))
                    .map(|major| {
                        let mut n: Vec<String> = all
                            .iter()
                            // A spec that names a distro is not "one build off"
                            // from another distro — that one missed for a
                            // different reason, and saying otherwise misleads.
                            .filter(|(_, h)| want_vendor.is_none_or(|w| vendor_of(h) == Some(w)))
                            .filter(|(v, _)| major_of(v) == Some(major) && numkey(v) != numkey(&want_version))
                            .map(|(v, h)| match vendor_of(h) {
                                Some(ven) => format!("{ven}-{v}"),
                                None => v.clone(),
                            })
                            .collect();
                        n.sort();
                        n.dedup();
                        n
                    })
                    .unwrap_or_default();
                let closest = if near.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n  same major, different build: {} — '{spec}' is exact, so none of these can satisfy it",
                        near.join(" ")
                    )
                };
                die(&format!(
                    "no installed JDK matches '{spec}' (pinned by {})\n  installed JDKs: {}{closest}\n  \
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
                let vendor = crate::jdk::default_vendor();
                eprintln!(
                    "{} no Java pinned and none installed — fetching {vendor} {major} (latest LTS) and setting it as your default {}",
                    paint("33", "jolta:", true),
                    paint("2", "(set JOLTA_NO_AUTO_INSTALL=1 to disable)", true)
                );
                if install_vendor_spec(vendor, &spec).is_ok() {
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
                Some((format!("{vendor}@21.0.4"), format!("21.0.4-{suffix}"))),
                "suffix -{suffix}"
            );
        }
    }

    #[test]
    fn sdkmanrc_unknown_suffix_is_vendorless() {
        assert_eq!(
            parse_sdkmanrc("java=21.0.4-wibble\n"),
            Some(("21.0.4".into(), "21.0.4-wibble".into()))
        );
    }

    /// The raw ID survives sdkman's whitespace/comment normalisation — it has
    /// to name the install directory byte for byte to be usable as a lookup.
    #[test]
    fn sdkmanrc_keeps_raw_identifier() {
        assert_eq!(
            parse_sdkmanrc("java = 21.0.5.11.1-amzn # ci\r\n"),
            Some(("corretto@21.0.5.11.1".into(), "21.0.5.11.1-amzn".into()))
        );
    }
}

#[cfg(test)]
mod precedence_tests {
    use super::*;

    fn fake(name: &str, implementor: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jolta-prec-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("release"), format!("IMPLEMENTOR=\"{implementor}\"\n")).unwrap();
        dir
    }

    /// The user's example verbatim: preferred-vendor 11.0.31 beats another
    /// vendor's 11.0.32; without a preference the higher build wins.
    #[test]
    fn preferred_vendor_beats_higher_version() {
        let corretto = fake("corretto-11.0.31", "Amazon.com Inc.");
        let temurin = fake("temurin-11.0.32", "Eclipse Adoptium");
        let candidates = vec![
            ("11.0.31".to_string(), corretto.clone()),
            ("11.0.32".to_string(), temurin.clone()),
        ];
        assert_eq!(best_match(&candidates, None, 11, "11", Some("corretto")), Some(corretto.clone()));
        assert_eq!(best_match(&candidates, None, 11, "11", None), Some(temurin.clone()));
        // explicit spec vendor overrides the preference entirely
        assert_eq!(best_match(&candidates, Some("temurin"), 11, "11", Some("corretto")), Some(temurin.clone()));
        // preference for a vendor with no candidates falls back to highest
        assert_eq!(best_match(&candidates, None, 11, "11", Some("zulu")), Some(temurin.clone()));
        let _ = fs::remove_dir_all(&corretto);
        let _ = fs::remove_dir_all(&temurin);
    }

    #[test]
    fn preferred_vendor_wins_exact_ties() {
        let corretto = fake("corretto-17.0.9", "Amazon.com Inc.");
        let temurin = fake("temurin-17.0.9", "Eclipse Adoptium");
        let candidates = vec![
            ("17.0.9".to_string(), temurin.clone()),
            ("17.0.9".to_string(), corretto.clone()),
        ];
        assert_eq!(best_match(&candidates, None, 17, "17.0.9", Some("corretto")), Some(corretto.clone()));
        let _ = fs::remove_dir_all(&corretto);
        let _ = fs::remove_dir_all(&temurin);
    }

    #[test]
    fn pin_protection_rules() {
        assert!(pin_protects("21.0.1", "temurin", "21.0.1"));
        assert!(pin_protects("temurin@21.0.1", "temurin", "21.0.1"));
        assert!(pin_protects("21.0.1+9", "temurin", "21.0.1"));
        assert!(!pin_protects("corretto@21.0.1", "temurin", "21.0.1")); // other vendor
        assert!(!pin_protects("21", "temurin", "21.0.1")); // major pins don't protect old builds
        assert!(!pin_protects("21.0.2", "temurin", "21.0.1"));
    }
}
