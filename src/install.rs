//! Fetching and installing JDK builds from vendor download endpoints.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::commands::cmd_reshim;
use crate::download::download;
use crate::jdk::{
    is_exact, jdk_version, major_of, managed_home, tool_bin, version_from_java_output, version_from_name,
    version_of, INSTALLABLE_VENDORS,
};
use crate::paths::jolta_home;
use crate::resolve::resolve;
use crate::ui::{bold, cyan, die, dim, ok_mark, paint};

/// Old majors often ship no macOS arm64 build (JDK 8/11 on Apple Silicon):
/// set during the one-shot x64 retry so URL builders pick the Rosetta build.
static FORCE_X64: AtomicBool = AtomicBool::new(false);

/// Every platform a mirror serves. `mirror sync` iterates these; 0 in
/// PLATFORM_OVERRIDE means "the host platform".
const SYNC_PLATFORMS: [(&'static str, &'static str, &'static str); 5] = [
    ("macos", "aarch64", "tar.gz"),
    ("macos", "x64", "tar.gz"),
    ("linux", "x64", "tar.gz"),
    ("linux", "aarch64", "tar.gz"),
    ("windows", "x64", "zip"),
];
static PLATFORM_OVERRIDE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn platform() -> (&'static str, &'static str, &'static str) {
    let idx = PLATFORM_OVERRIDE.load(Ordering::Relaxed);
    if idx > 0 {
        return SYNC_PLATFORMS[idx - 1];
    }
    let os = match env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = if FORCE_X64.load(Ordering::Relaxed) {
        "x64"
    } else {
        match env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x64",
            other => die(&format!("unsupported architecture: {other}")),
        }
    };
    let ext = if os == "windows" { "zip" } else { "tar.gz" };
    (os, arch, ext)
}

/// Alpine and friends: glibc JDK builds "install" fine and then die in the
/// loader (sdkman #1133, mise #9679). Detect musl so URL builders can ask
/// for musl variants where vendors publish them.
#[cfg(target_os = "linux")]
fn is_musl() -> bool {
    fs::read_dir("/lib")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("ld-musl-"))
}
#[cfg(not(target_os = "linux"))]
fn is_musl() -> bool {
    false
}

/// --fresh (or JOLTA_FRESH=1): drop the remote-metadata cache so this
/// invocation refetches once — and then caches the fresh results.
pub fn set_fresh() {
    let _ = fs::remove_dir_all(jolta_home().join("remote-cache"));
}

/// Mirror bases are filesystem paths pasted into URLs; curl rejects literal
/// spaces, so percent-encode them (the only URL-hostile char paths commonly have).
fn encode_spaces(s: &str) -> String {
    s.replace(' ', "%20")
}

fn cache_key(s: &str) -> String {
    // djb2 — plenty for filename dedup of URLs
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33) ^ b as u64;
    }
    format!("{h:016x}")
}

/// Remote metadata cache: 24h TTL (JOLTA_CACHE_TTL_HOURS to tune), separate
/// from the resolution cache so install/reshim's clear_cache leaves it alone.
fn remote_cache_get(key: &str) -> Option<String> {
    let f = jolta_home().join("remote-cache").join(cache_key(key));
    let ttl_hours: u64 = env::var("JOLTA_CACHE_TTL_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let age = fs::metadata(&f).ok()?.modified().ok()?.elapsed().ok()?;
    let body = fs::read_to_string(&f).ok()?;
    // Negative verdicts (empty body, "0") expire after an hour at most: one
    // transient network blip must not wedge "not published" answers for a day.
    let ttl_hours = if body.is_empty() || body == "0" { ttl_hours.min(1) } else { ttl_hours };
    if age > Duration::from_secs(ttl_hours * 3600) {
        return None;
    }
    Some(body)
}

fn remote_cache_put(key: &str, value: &str) {
    let dir = jolta_home().join("remote-cache");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(cache_key(key)), value);
}

fn url_ok(url: &str) -> bool {
    if let Some(hit) = remote_cache_get(&format!("ok:{url}")) {
        return hit == "1";
    }
    let ok = url_ok_uncached(url);
    remote_cache_put(&format!("ok:{url}"), if ok { "1" } else { "0" });
    ok
}

fn url_ok_uncached(url: &str) -> bool {
    Command::new("curl")
        .args(["-sIL", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15"])
        .arg(url)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
        .unwrap_or(false)
}

fn curl_text(url: &str) -> Option<String> {
    if let Some(hit) = remote_cache_get(&format!("body:{url}")) {
        return if hit.is_empty() { None } else { Some(hit) };
    }
    let body = curl_text_uncached(url);
    remote_cache_put(&format!("body:{url}"), body.as_deref().unwrap_or(""));
    body
}

fn curl_text_uncached(url: &str) -> Option<String> {
    let o = Command::new("curl").args(["-fsSL"]).arg(url).output().ok()?;
    if o.status.success() {
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    }
}

/// SHA-256 of a file via the system tools (shasum on macOS, sha256sum on
/// Linux) — no hash crate, matching the no-dependency ethos.
fn sha256_file(path: &Path) -> Option<String> {
    for (cmd, args) in [("shasum", &["-a", "256"][..]), ("sha256sum", &[][..])] {
        if let Ok(o) = Command::new(cmd).args(args).arg(path).output() {
            if o.status.success() {
                return String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Every string value for `key` in a JSON blob. Primitive scraping, no parser
/// dependency — fine for the flat vendor metadata we consume.
fn json_strings(text: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(k) = text[idx..].find(&pat) {
        let after = idx + k + pat.len();
        let rest = text[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    out.push(rest[..end].to_string());
                }
            }
        }
        idx = after;
    }
    out
}

/// First bare string inside the array value of `key` ({"releases":["x", ...]}).
fn json_first_array_string(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let start = text.find(&pat)? + pat.len();
    let rest = &text[start..];
    let bracket = rest.find('[')?;
    let rest = &rest[bracket + 1..];
    let q1 = rest.find('"')?;
    let rest = &rest[q1 + 1..];
    let q2 = rest.find('"')?;
    Some(rest[..q2].to_string())
}

/// "21.0.2" -> "21.0.3": bump the last component (for API range queries).
fn bump_last(version: &str) -> String {
    let mut parts: Vec<u64> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    if let Some(last) = parts.last_mut() {
        *last += 1;
    }
    parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
}

/// Pure URL formatters for the GitHub-released and constructible vendors —
/// separated from the network paths so each vendor's URL shape is unit-tested.
pub fn sapmachine_url(version: &str, os: &str, arch: &str, ext: &str) -> String {
    format!(
        "https://github.com/SAP/SapMachine/releases/download/sapmachine-{version}/sapmachine-jdk-{version}_{os}-{arch}_bin.{ext}"
    )
}

pub fn graalce_url(version: &str, os: &str, arch: &str, ext: &str) -> String {
    format!(
        "https://github.com/graalvm/graalvm-ce-builds/releases/download/jdk-{version}/graalvm-community-jdk-{version}_{os}-{arch}_bin.{ext}"
    )
}

/// Liberica names x86_64 "amd64" and appends "-musl" for Alpine builds; the
/// `full` version must include the +build (the API supplies it).
pub fn liberica_url(full: &str, os: &str, arch: &str, ext: &str, musl: bool) -> String {
    let arch = if arch == "x64" { "amd64" } else { arch };
    let musl = if musl && os == "linux" { "-musl" } else { "" };
    format!("https://github.com/bell-sw/Liberica/releases/download/{full}/bellsoft-jdk{full}-{os}-{arch}{musl}.{ext}")
}

/// Liberica full versions ("21.0.4+9") for a major, newest first, via the
/// BellSoft API.
fn liberica_versions(major: u32) -> Vec<String> {
    let (os, _, ext) = platform();
    let os = if os == "linux" && is_musl() { "linux-musl" } else { os };
    let Some(body) = curl_text(&format!(
        "https://api.bell-sw.com/v1/liberica/releases?version-feature={major}&bundle-type=jdk&os={os}&package-type={ext}&bitness=64"
    )) else {
        return Vec::new();
    };
    let mut out = json_strings(&body, "version");
    out.sort_by_key(|v| std::cmp::Reverse(crate::jdk::numkey(v)));
    out.dedup();
    out
}

/// Latest GA tag for a GitHub-released vendor (SapMachine, GraalVM CE):
/// strip the tag prefix, skip EA/+build tags, take the highest for the major.
fn github_latest(repo: &str, tag_prefix: &str, major: u32) -> Option<String> {
    let mut tags = Vec::new();
    for page in 1..=2 {
        let body = curl_text(&format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"))?;
        let t = json_strings(&body, "tag_name");
        let n = t.len();
        tags.extend(t);
        if n < 100 {
            break;
        }
    }
    tags.iter()
        .filter_map(|t| t.strip_prefix(tag_prefix))
        .filter(|v| !v.contains('+') && !v.contains("ea"))
        .filter(|v| major_of(v) == Some(major))
        .max_by_key(|v| crate::jdk::numkey(v))
        .map(str::to_string)
}

/// Zulu has no constructible URLs; ask the Azul metadata API. Works for both
/// latest-of-major and exact versions. Filters out JavaFX/CRaC variant builds.
fn zulu_url(version: &str, latest: bool) -> Option<String> {
    let (os, arch, ext) = platform();
    let os = if os == "linux" && is_musl() { "linux-musl" } else { os };
    let latest_q = if latest { "&latest=true" } else { "" };
    let url = format!(
        "https://api.azul.com/metadata/v1/zulu/packages/?java_version={version}&os={os}&arch={arch}&archive_type={ext}&java_package_type=jdk&javafx_bundled=false&release_status=ga{latest_q}"
    );
    let body = curl_text(&url)?;
    json_strings(&body, "download_url")
        .into_iter()
        .find(|u| u.contains("-ca-jdk"))
}

/// Download URL for an exact point release ("21.0.2") of a distro.
fn exact_url(vendor: &str, version: &str) -> Option<String> {
    let (os, arch, ext) = platform();
    let major = major_of(version)?;
    match vendor {
        "temurin" => {
            // resolve "21.0.2" -> release name "jdk-21.0.2+13", then fetch it
            let range = format!("%5B{version}%2C{}%29", bump_last(version));
            let names = curl_text(&format!(
                "https://api.adoptium.net/v3/info/release_names?release_type=ga&version={range}"
            ))?;
            let release = json_first_array_string(&names, "releases")?;
            let os = if os == "macos" {
                "mac"
            } else if os == "linux" && is_musl() {
                "alpine-linux"
            } else {
                os
            };
            Some(format!(
                "https://api.adoptium.net/v3/binary/version/{}/{os}/{arch}/jdk/hotspot/normal/eclipse",
                release.replace('+', "%2B")
            ))
        }
        "corretto" => {
            // corretto tags are 5-part (21.0.2.13.1); some tagged builds never
            // reach the CDN, so probe every matching tag until one answers
            let (osname, suffix) = match os {
                "macos" => ("macosx", format!(".{ext}")),
                "windows" => ("windows", format!("-jdk.{ext}")),
                _ => ("linux", format!(".{ext}")),
            };
            let mk = |tag: &str| {
                format!(
                    "https://corretto.aws/downloads/resources/{tag}/amazon-corretto-{tag}-{osname}-{arch}{suffix}"
                )
            };
            // paginate: exact pins of older releases fall past the first 100
            // tags (sdkman #1300 class)
            let mut tags = Vec::new();
            for page in 1..=3 {
                let Some(body) = curl_text(&format!(
                    "https://api.github.com/repos/corretto/corretto-{major}/releases?per_page=100&page={page}"
                )) else {
                    break;
                };
                let t = json_strings(&body, "tag_name");
                let n = t.len();
                tags.extend(t);
                if n < 100 {
                    break;
                }
            }
            tags.iter()
                .filter(|t| *t == version || t.starts_with(&format!("{version}.")))
                .map(|t| mk(t))
                .find(|u| url_ok(u))
                // ancient releases age out of even paginated listings; a full
                // 4/5-part version IS the tag — probe it directly
                .or_else(|| {
                    if version.split('.').count() >= 4 {
                        let u = mk(version);
                        url_ok(&u).then_some(u)
                    } else {
                        None
                    }
                })
        }
        "oracle" => Some(format!(
            "https://download.oracle.com/java/{major}/archive/jdk-{version}_{os}-{arch}_bin.{ext}"
        )),
        "graalvm" => Some(format!(
            "https://download.oracle.com/graalvm/{major}/archive/graalvm-jdk-{version}_{os}-{arch}_bin.{ext}"
        )),
        "zulu" => zulu_url(version, false),
        "sapmachine" => Some(sapmachine_url(version, os, arch, ext)),
        "graalce" => Some(graalce_url(version, os, arch, ext)),
        "liberica" => {
            // resolve "21.0.4" -> full "21.0.4+9" via the API; +build pins pass through
            let full = if version.contains('+') {
                version.to_string()
            } else {
                liberica_versions(major)
                    .into_iter()
                    .find(|v| v == version || v.starts_with(&format!("{version}+")))?
            };
            Some(liberica_url(&full, os, arch, ext, is_musl()))
        }
        _ => None,
    }
}

/// Download URL for the latest build of a vendor+major on this platform.
pub fn latest_download_url(vendor: &str, major: u32) -> Option<String> {
    match vendor {
        "zulu" => zulu_url(&major.to_string(), true),
        "liberica" | "sapmachine" | "graalce" => {
            let v = latest_remote_version(vendor, major)?;
            exact_url(vendor, &v)
        }
        _ => Some(vendor_url(vendor, major)),
    }
}

/// Download URL for the latest GA build of a distro + major on this platform.
fn vendor_url(vendor: &str, major: u32) -> String {
    let (os, arch, ext) = platform();
    let _ = ext;
    // Restricted networks: JOLTA_DOWNLOAD_BASE points all downloads at an
    // internal mirror using a flat, predictable layout instead of the three
    // different vendor URL schemes (see issue #2).
    if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
        let base = encode_spaces(base.trim_end_matches('/'));
        return format!("{base}/{vendor}/{major}/{os}-{arch}.{ext}");
    }
    match vendor {
        "temurin" => {
            let os = if os == "macos" {
                "mac"
            } else if os == "linux" && is_musl() {
                "alpine-linux"
            } else {
                os
            };
            format!("https://api.adoptium.net/v3/binary/latest/{major}/ga/{os}/{arch}/jdk/hotspot/normal/eclipse")
        }
        "corretto" => format!("https://corretto.aws/downloads/latest/amazon-corretto-{major}-{arch}-{os}-jdk.{ext}"),
        "oracle" => format!("https://download.oracle.com/java/{major}/latest/jdk-{major}_{os}-{arch}_bin.{ext}"),
        "graalvm" => format!("https://download.oracle.com/graalvm/{major}/latest/graalvm-jdk-{major}_{os}-{arch}_bin.{ext}"),
        other => die(&format!(
            "don't know how to download '{other}' builds (downloadable distros: {})",
            INSTALLABLE_VENDORS.join(", ")
        )),
    }
}

/// Latest available point release for a distro+major, learned from the
/// versioned filename the vendor's "latest" URL redirects to — no download.
/// None when undeterminable (mirrors, vendors that don't redirect).
pub fn latest_remote_version(vendor: &str, major: u32) -> Option<String> {
    if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
        // Optional mirror metadata ({base}/{vendor}/{major}/latest, written by
        // 'jolta mirror sync'): gives update/upgrade/catalog full precision.
        // Absent -> None, the blind-but-working behavior mirrors always had.
        let base = encode_spaces(base.trim_end_matches('/'));
        return curl_text_uncached(&format!("{base}/{vendor}/{major}/latest"))
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
            .filter(|v| major_of(v) == Some(major));
    }
    let ck = format!("latest:{vendor}:{major}");
    if let Some(hit) = remote_cache_get(&ck) {
        return if hit.is_empty() { None } else { Some(hit) };
    }
    let result = latest_remote_version_uncached(vendor, major);
    remote_cache_put(&ck, result.as_deref().unwrap_or(""));
    result
}

fn latest_remote_version_uncached(vendor: &str, major: u32) -> Option<String> {
    match vendor {
        "liberica" => return liberica_versions(major).into_iter().next(),
        "sapmachine" => return github_latest("SAP/SapMachine", "sapmachine-", major),
        "graalce" => return github_latest("graalvm/graalvm-ce-builds", "jdk-", major),
        _ => {}
    }
    if vendor == "zulu" {
        // no redirect chain to inspect; the API filename carries the version
        let url = zulu_url(&major.to_string(), true)?;
        let start = url.find("-ca-jdk")? + "-ca-jdk".len();
        let v: String = url[start..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let v = v.trim_end_matches('.').to_string();
        return if v.is_empty() { None } else { Some(v) };
    }
    let url = vendor_url(vendor, major);
    // Scan the redirect chain's Location headers: the versioned filename shows
    // up in an intermediate hop (Temurin's final hop is an unversioned signed
    // blob URL, so %{url_effective} is useless there).
    let o = Command::new("curl").args(["-sIL"]).arg(&url).output().ok()?;
    let headers = String::from_utf8_lossy(&o.stdout);
    let needle = format!("{major}.");
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("location") {
            continue;
        }
        if let Some(start) = line.find(&needle) {
            let version: String = line[start..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let version = version.trim_end_matches('.').to_string();
            if version.len() > needle.len() {
                return Some(version);
            }
        }
    }
    None
}

/// All quoted strings inside the array value of `key`.
fn json_array_strings(text: &str, key: &str) -> Vec<String> {
    let pat = format!("\"{key}\"");
    let Some(start) = text.find(&pat) else { return Vec::new() };
    let rest = &text[start + pat.len()..];
    let Some(open) = rest.find('[') else { return Vec::new() };
    let Some(close) = rest[open..].find(']') else { return Vec::new() };
    let body = &rest[open + 1..open + close];
    let mut out = Vec::new();
    let mut it = body.split('"');
    it.next(); // before first quote
    while let (Some(s), Some(_)) = (it.next(), it.next()) {
        out.push(s.to_string());
    }
    out
}

/// Numeric array value of `key` ("available_releases": [8, 11, ...]).
fn json_number_array(text: &str, key: &str) -> Vec<u32> {
    let pat = format!("\"{key}\"");
    let Some(start) = text.find(&pat) else { return Vec::new() };
    let rest = &text[start + pat.len()..];
    let Some(open) = rest.find('[') else { return Vec::new() };
    let Some(close) = rest[open..].find(']') else { return Vec::new() };
    rest[open + 1..open + close]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect()
}

fn json_number(text: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\"");
    let start = text.find(&pat)? + pat.len();
    let rest = text[start..].trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The Java release universe: (available majors, LTS majors, most recent LTS,
/// most recent feature release). Under a mirror this comes from the mirror's
/// own metadata ({base}/lts + per-vendor index.txt) so catalog, update, and
/// the fresh-machine LTS bootstrap all work air-gapped; otherwise Adoptium.
pub fn release_universe() -> Option<(Vec<u32>, Vec<u32>, u32, u32)> {
    if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
        let base = encode_spaces(base.trim_end_matches('/'));
        let lts: u32 = curl_text_uncached(&format!("{base}/lts"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        let mut available = vec![lts];
        for vendor in INSTALLABLE_VENDORS {
            if let Some(idx) = curl_text_uncached(&format!("{base}/{vendor}/index.txt")) {
                available.extend(idx.lines().filter_map(|l| major_of(l.trim())));
            }
        }
        available.sort_unstable();
        available.dedup();
        let feature = *available.iter().max().unwrap_or(&lts);
        return Some((available, vec![lts], lts, feature));
    }
    let body = curl_text("https://api.adoptium.net/v3/info/available_releases")?;
    let available = json_number_array(&body, "available_releases");
    let lts = json_number_array(&body, "available_lts_releases");
    let recent_lts = json_number(&body, "most_recent_lts")?;
    let recent_feature = json_number(&body, "most_recent_feature_release")?;
    Some((available, lts, recent_lts, recent_feature))
}

/// All published GA versions of a distro for one major, newest first.
/// Oracle/GraalVM have no listing API: returns just the latest (flagged by
/// the caller). Empty on network failure.
pub fn vendor_versions(vendor: &str, major: u32) -> Vec<String> {
    let mut versions: Vec<String> = match vendor {
        "temurin" => {
            let range = format!("%5B{major}%2C{}%29", major + 1);
            let mut out = Vec::new();
            for page in 0..3 {
                let Some(body) = curl_text(&format!(
                    "https://api.adoptium.net/v3/info/release_names?release_type=ga&version={range}&page_size=100&page={page}"
                )) else { break };
                let names = json_array_strings(&body, "releases");
                if names.is_empty() {
                    break;
                }
                let n = names.len();
                out.extend(names.into_iter().map(|n| {
                    n.trim_start_matches("jdk-").trim_start_matches("jdk").to_string()
                }));
                if n < 100 {
                    break;
                }
            }
            out
        }
        "corretto" => {
            let Some(body) = curl_text(&format!(
                "https://api.github.com/repos/corretto/corretto-{major}/releases?per_page=100"
            )) else { return Vec::new() };
            json_strings(&body, "tag_name")
        }
        "zulu" => {
            let (os, arch, ext) = platform();
            let Some(body) = curl_text(&format!(
                "https://api.azul.com/metadata/v1/zulu/packages/?java_version={major}&os={os}&arch={arch}&archive_type={ext}&java_package_type=jdk&javafx_bundled=false&release_status=ga&page_size=100"
            )) else { return Vec::new() };
            let mut out: Vec<String> = json_strings(&body, "name")
                .into_iter()
                .filter(|n| n.contains("-ca-jdk"))
                .filter_map(|n| {
                    let i = n.find("-ca-jdk")? + "-ca-jdk".len();
                    let v: String = n[i..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                    let v = v.trim_end_matches('.').to_string();
                    if v.is_empty() { None } else { Some(v) }
                })
                .collect();
            out.dedup();
            out
        }
        "liberica" => liberica_versions(major),
        "sapmachine" | "graalce" => latest_remote_version(vendor, major).into_iter().collect(),
        "oracle" | "graalvm" => latest_remote_version(vendor, major).into_iter().collect(),
        _ => Vec::new(),
    };
    versions.sort_by_key(|v| std::cmp::Reverse(crate::jdk::numkey(v)));
    versions.dedup();
    versions
}

/// Does the vendor's latest-URL respond for this major? (For distros that
/// serve directly with no redirect, the version is only known post-download.)
pub fn probe_latest(vendor: &str, major: u32) -> bool {
    let url = match vendor {
        // these have listing APIs; never need URL probing
        "zulu" | "liberica" | "sapmachine" | "graalce" => return false,
        _ => vendor_url(vendor, major),
    };
    url_ok(&url)
}

/// Remove jolta-managed installs of this distro+major other than `keep`.
/// Builds that a remembered pin still exact-references are kept — upgrading
/// must never break a project pinned to the superseded build.
pub fn prune_superseded(vendor: &str, major: u32, keep: &str) {
    let pins = crate::resolve::remembered_pin_specs();
    let jdks = jolta_home().join("jdks");
    let Ok(entries) = fs::read_dir(&jdks) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(full) = name.strip_prefix(&format!("{vendor}-")) else { continue };
        if full != keep && crate::jdk::major_of(full) == Some(major) {
            if let Some((_, src)) = pins.iter().find(|(s, _)| crate::resolve::pin_protects(s, vendor, full)) {
                println!("  {} kept {name} {}", ok_mark(), dim(&format!("(pinned by {src})")));
                continue;
            }
            if fs::remove_dir_all(entry.path()).is_ok() {
                println!("  {} pruned {name}", ok_mark());
            }
        }
    }
}

pub fn install_vendor_major(vendor: &str, major: u32) -> Result<String, ()> {
    install_vendor_spec(vendor, &major.to_string())
}

/// JAVA_VERSION from inside an archive without installing it (tar reads
/// foreign-platform tar.gz and — via bsdtar — zip alike).
fn archive_version(archive: &Path) -> Option<String> {
    let tmp = jolta_home().join("tmp").join(format!("mirror-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::create_dir_all(&tmp);
    let ok = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&tmp)
        .status()
        .is_ok_and(|s| s.success());
    let v = if ok {
        fs::read_dir(&tmp)
            .ok()
            .and_then(|mut e| e.find_map(|x| x.ok().map(|x| x.path()).filter(|p| p.is_dir())))
            .and_then(|top| version_of(&managed_home(&top)))
    } else {
        None
    };
    let _ = fs::remove_dir_all(&tmp);
    v
}

/// jolta mirror sync <dir> [--from <base>] [--vendors a,b] [--majors 21,17]
/// jolta mirror verify <dir>
/// Builds/refreshes an offline mirror in the exact layout JOLTA_DOWNLOAD_BASE
/// consumes — assets for every platform, .sha256 sidecars, and the metadata
/// files (per-major `latest`, per-vendor `index.txt`, top-level `lts`) that
/// give update/upgrade/catalog/bootstrap full precision offline.
pub fn cmd_mirror(rest: &[String]) {
    match rest.first().map(String::as_str) {
        Some("sync") => mirror_sync(&rest[1..]),
        Some("verify") => mirror_verify(&rest[1..]),
        _ => die(
            "usage: jolta mirror sync <dir> [--from <base>] [--vendors temurin,corretto] [--majors 21,17]\n       \
             jolta mirror verify <dir>",
        ),
    }
}

fn mirror_sync(args: &[String]) {
    let mut dir: Option<PathBuf> = None;
    let mut from: Option<String> = None;
    let mut vendors: Vec<String> = vec![crate::jdk::default_vendor().to_string()];
    let mut majors: Vec<u32> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" => from = it.next().map(|s| encode_spaces(s.trim_end_matches('/'))),
            "--vendors" => {
                vendors = it.next().map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default()
            }
            "--majors" => {
                majors = it
                    .next()
                    .map(|s| s.split(',').filter_map(|m| m.trim().parse().ok()).collect())
                    .unwrap_or_default()
            }
            other if !other.starts_with('-') && dir.is_none() => dir = Some(PathBuf::from(other)),
            other => die(&format!("mirror sync: unknown argument '{other}'")),
        }
    }
    let dir = dir.unwrap_or_else(|| die("usage: jolta mirror sync <dir> [--from <base>] [--vendors ...] [--majors ...]"));
    for v in &vendors {
        if !INSTALLABLE_VENDORS.contains(&v.as_str()) {
            die(&format!("unknown vendor '{v}' (downloadable distros: {})", INSTALLABLE_VENDORS.join(", ")));
        }
    }
    // Syncing FROM the vendors must not route through a configured mirror.
    if from.is_none() {
        env::remove_var("JOLTA_DOWNLOAD_BASE");
    }
    // LTS marker: copy the source mirror's, else ask the release universe.
    let lts: Option<u32> = match &from {
        Some(base) => curl_text_uncached(&format!("{base}/lts"))
            .and_then(|s| s.split_whitespace().next()?.parse().ok()),
        None => release_universe().map(|(_, _, lts, _)| lts),
    };
    if majors.is_empty() {
        majors = match (&from, release_universe()) {
            (None, Some((_, lts_set, _, feature))) => {
                let mut m = lts_set;
                m.push(feature);
                m.sort_unstable();
                m.dedup();
                m
            }
            _ => lts.into_iter().collect(),
        };
        if majors.is_empty() {
            die("cannot determine majors to sync — pass --majors (e.g. --majors 8,11,17,21)");
        }
    }
    println!(
        "{} syncing {} for majors {} into {}",
        bold("mirror"),
        cyan(&vendors.join(", ")),
        majors.iter().map(u32::to_string).collect::<Vec<_>>().join(", "),
        dir.display()
    );

    let mut synced = 0u32;
    let mut skipped = 0u32;
    for vendor in &vendors {
        let mut index: Vec<String> = Vec::new();
        for major in &majors {
            let out_dir = dir.join(vendor).join(major.to_string());
            let _ = fs::create_dir_all(&out_dir);
            let mut version_meta: Option<String> = None;
            for (i, (os, arch, ext)) in SYNC_PLATFORMS.iter().enumerate() {
                PLATFORM_OVERRIDE.store(i + 1, Ordering::Relaxed);
                let url = match &from {
                    Some(base) => Some(format!("{base}/{vendor}/{major}/{os}-{arch}.{ext}")),
                    None => latest_download_url(vendor, *major),
                };
                PLATFORM_OVERRIDE.store(0, Ordering::Relaxed);
                let dest = out_dir.join(format!("{os}-{arch}.{ext}"));
                let label = format!("{vendor}@{major} {os}-{arch}");
                let ok = url.as_deref().is_some_and(|u| download(u, &dest, &label));
                if !ok {
                    let _ = fs::remove_file(&dest);
                    eprintln!("  {} {label} not available — skipped", paint("33", "!", true));
                    skipped += 1;
                    continue;
                }
                if let Some(sha) = sha256_file(&dest) {
                    // full-name suffix: with_extension would eat ".gz"
                    let _ = fs::write(
                        PathBuf::from(format!("{}.sha256", dest.display())),
                        format!("{sha}\n"),
                    );
                }
                synced += 1;
                if version_meta.is_none() {
                    version_meta = archive_version(&dest);
                }
            }
            if let Some(v) = &version_meta {
                let _ = fs::write(out_dir.join("latest"), format!("{v}\n"));
                index.push(v.clone());
            }
        }
        if !index.is_empty() {
            let idx_path = dir.join(vendor).join("index.txt");
            if let Ok(existing) = fs::read_to_string(&idx_path) {
                index.extend(existing.lines().map(str::to_string));
            }
            index.sort_by_key(|v| std::cmp::Reverse(crate::jdk::numkey(v)));
            index.dedup();
            let _ = fs::write(&idx_path, index.join("\n") + "\n");
        }
    }
    if let Some(l) = lts {
        let _ = fs::write(dir.join("lts"), format!("{l}\n"));
    }
    println!(
        "{} {synced} assets synced, {skipped} skipped — serve this directory and set JOLTA_DOWNLOAD_BASE",
        ok_mark()
    );
    if skipped > 0 && synced == 0 {
        std::process::exit(1);
    }
}

fn mirror_verify(args: &[String]) {
    let dir = args
        .first()
        .filter(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| die("usage: jolta mirror verify <dir>"));
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.to_string_lossy().ends_with(".tar.gz") || p.to_string_lossy().ends_with(".zip") {
                out.push(p);
            }
        }
    }
    let mut assets = Vec::new();
    walk(&dir, &mut assets);
    if assets.is_empty() {
        die(&format!("no mirror assets found under {}", dir.display()));
    }
    let (mut ok, mut missing, mut bad) = (0u32, 0u32, 0u32);
    for asset in assets {
        let sidecar = PathBuf::from(format!("{}.sha256", asset.display()));
        let Ok(want) = fs::read_to_string(&sidecar) else {
            println!("  {} {} (no .sha256 sidecar)", paint("33", "!", true), asset.display());
            missing += 1;
            continue;
        };
        let want = want.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        match sha256_file(&asset) {
            Some(got) if got == want => {
                println!("  {} {}", ok_mark(), asset.display());
                ok += 1;
            }
            _ => {
                println!("  {} {} CHECKSUM MISMATCH", paint("31", "✗", true), asset.display());
                bad += 1;
            }
        }
    }
    println!("{ok} verified, {missing} without sidecars, {bad} corrupt");
    if bad > 0 {
        std::process::exit(1);
    }
}

pub fn install_vendor_spec(vendor: &str, version: &str) -> Result<String, ()> {
    if !matches!(env::consts::OS, "macos" | "linux" | "windows") {
        die(&format!("unsupported OS: {}", env::consts::OS));
    }

    let major = major_of(version).unwrap_or_else(|| die(&format!("cannot parse version '{version}'")));
    // Validate before taking the install lock: vendor_url() dies on unknown
    // vendors (e.g. "openjdk", pinnable but not downloadable).
    if !INSTALLABLE_VENDORS.contains(&vendor) {
        die(&format!(
            "don't know how to download '{vendor}' builds (downloadable distros: {})",
            INSTALLABLE_VENDORS.join(", ")
        ));
    }
    // musl systems: only temurin and zulu publish musl builds; a glibc JDK
    // would install fine and then die in the loader on first use.
    if is_musl() && !matches!(vendor, "temurin" | "zulu" | "liberica") && env::var("JOLTA_DOWNLOAD_BASE").is_err() {
        die(&format!(
            "'{vendor}' publishes no musl builds (this looks like Alpine) — use temurin, zulu, or liberica"
        ));
    }
    // Serialize concurrent installs of the same distro+version (parallel
    // builds can fire several shims at once); losers wait, then re-check.
    let _ = fs::create_dir_all(jolta_home().join("cache"));
    let safe_v: String = version.chars().map(|c| if c == '/' { '_' } else { c }).collect();
    let lock = jolta_home().join("cache").join(format!("install-{vendor}-{safe_v}.lock"));
    struct LockGuard(PathBuf);
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.0);
        }
    }
    struct TmpGuard(PathBuf);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _lock_guard = match fs::create_dir(&lock) {
        Ok(()) => LockGuard(lock.clone()),
        Err(_) => {
            println!("jolta: another install of {vendor} {version} is running; waiting...");
            let mut waited = 0;
            while lock.is_dir() && waited < 600 {
                std::thread::sleep(Duration::from_secs(2));
                waited += 2;
            }
            if let Some(h) = resolve(&format!("{vendor}-{version}")) {
                println!("{} {vendor} {version} installed by the other process", ok_mark());
                return Ok(version_of(&h).unwrap_or_default());
            }
            if lock.is_dir() {
                die(&format!("timed out waiting for concurrent install (remove {} if stale)", lock.display()));
            }
            match fs::create_dir(&lock) {
                Ok(()) => LockGuard(lock.clone()),
                Err(_) => die(&format!("could not acquire install lock {}", lock.display())),
            }
        }
    };

    // An exact version already on disk needs no download; non-exact specs
    // still hit the network to learn the latest build for their major.
    if is_exact(version) {
        if let Some(h) = resolve(&format!("{vendor}-{version}")) {
            println!("  {} {vendor} {version} is already installed", ok_mark());
            return Ok(version_of(&h).unwrap_or_default());
        }
    }

    // From here to the end the lock is held: report errors and return Err
    // instead of die()ing — exit() skips Drop, which would leak the lock and
    // stall the next install of this spec for up to ten minutes.
    let fail = |msg: &str| {
        eprintln!("{} {msg}", paint("31", "jolta:", true));
    };
    // URL construction is re-runnable so the arm64→x64 fallback below can
    // rebuild every vendor's URL against the alternate arch.
    let build_url = || -> Option<String> {
        if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
            let (os, arch, ext) = platform();
            Some(format!("{}/{vendor}/{version}/{os}-{arch}.{ext}", encode_spaces(base.trim_end_matches('/'))))
        } else if is_exact(version) {
            exact_url(vendor, version)
        } else {
            latest_download_url(vendor, major)
        }
    };
    // Extract inside JOLTA_HOME so the final rename into jdks/ never crosses
    // filesystems (/tmp is tmpfs on many Linux distros; rename(2) can't span
    // devices).
    let tmp = jolta_home().join("tmp").join(format!("install-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let _tmp_guard = TmpGuard(tmp.clone());

    let tarball = tmp.join("jdk.archive");
    let label = format!("{vendor}@{version}");
    let mut url = build_url();
    let mut downloaded = match &url {
        Some(u) => download(u, &tarball, &label),
        None => false,
    };
    // Old majors ship no macOS arm64 build (JDK 8/11 on Apple Silicon, volta
    // #1860 family): retry once as x64, which runs fine under Rosetta.
    if !downloaded && env::consts::OS == "macos" && env::consts::ARCH == "aarch64" {
        FORCE_X64.store(true, Ordering::Relaxed);
        let retry = build_url();
        if retry.is_some() && retry != url {
            eprintln!(
                "{} no arm64 build found — retrying with the x64 build (runs under Rosetta)",
                paint("33", "jolta:", true)
            );
            url = retry;
            downloaded = download(url.as_deref().unwrap(), &tarball, &label);
        }
        FORCE_X64.store(false, Ordering::Relaxed);
    }
    let Some(url) = url.filter(|_| downloaded) else {
        fail(&format!(
            "cannot fetch {vendor} {version} — is it published for {}/{}?",
            env::consts::OS,
            env::consts::ARCH
        ));
        return Err(());
    };

    // Optional integrity sidecar: an "<asset>.sha256" published next to the
    // asset is verified when present (volta #2075 — Volta verifies nothing).
    if let Some(sums) = curl_text_uncached(&format!("{url}.sha256")) {
        let want = sums.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if want.len() == 64 {
            match sha256_file(&tarball) {
                Some(got) if got == want => {
                    eprintln!("  {} checksum verified", paint("32", "✓", true));
                }
                Some(got) => {
                    fail(&format!(
                        "checksum mismatch for {url}\n  expected {want}\n  got      {got} — refusing to install"
                    ));
                    return Err(());
                }
                None => {}
            }
        }
    }

    let extract = tmp.join("x");
    let _ = fs::create_dir_all(&extract);
    let st = Command::new("tar")
        .arg("-xf")
        .arg(&tarball)
        .arg("-C")
        .arg(&extract)
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("  {} extraction failed", paint("31", "✗", true));
        return Err(());
    }
    eprintln!("  {} extracted", paint("32", "✓", true));
    let Some(top) = fs::read_dir(&extract)
        .ok()
        .and_then(|mut e| e.find_map(|x| x.ok().map(|x| x.path()).filter(|p| p.is_dir())))
    else {
        fail("unexpected archive layout (no top-level directory)");
        return Err(());
    };

    let home = managed_home(&top);
    // Zip archives carry no unix modes and some tars ship 644 tool binaries
    // (volta #350): make sure everything in bin/ is executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(entries) = fs::read_dir(home.join("bin")) {
            for e in entries.flatten() {
                let p = e.path();
                if let Ok(md) = p.metadata() {
                    if md.is_file() && md.permissions().mode() & 0o111 == 0 {
                        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(md.permissions().mode() | 0o755));
                    }
                }
            }
        }
    }
    // A wrong-libc/arch build "installs" cleanly and then every shim dies in
    // the loader with a cryptic ENOENT (mise #9679 on Alpine): prove the JVM
    // actually runs before promoting it. Spawn errors AND nonzero exits both
    // count — macOS "helpfully" execs unknown formats via sh, which then fails.
    let probe = Command::new(tool_bin(&home, "java")).arg("-version").output();
    let Ok(probe) = probe.and_then(|o| if o.status.success() { Ok(o) } else { Err(std::io::ErrorKind::Other.into()) })
    else {
        fail("extracted java cannot execute on this machine (wrong architecture or C library?)");
        return Err(());
    };
    // Not every build ships a release file — Corretto 8 on macOS ships none —
    // so ask the JVM that just proved it runs, and only then fall back to the
    // archive's directory name (which for those builds is bare "8").
    let banner = String::from_utf8_lossy(if probe.stderr.is_empty() { &probe.stdout } else { &probe.stderr });
    let Some(full) = jdk_version(&home)
        .or_else(|| version_from_java_output(&banner))
        .or_else(|| version_from_name(&top))
    else {
        fail("cannot determine the version of the extracted JDK (no release file, and java -version said nothing usable)");
        return Err(());
    };

    let dest = jolta_home().join("jdks").join(format!("{vendor}-{full}"));
    if dest.is_dir() {
        println!("  {} {vendor} {full} is already installed", ok_mark());
    } else {
        let _ = fs::create_dir_all(jolta_home().join("jdks"));
        if let Err(e) = fs::rename(&top, &dest) {
            fail(&format!("cannot move JDK into place: {e}"));
            return Err(());
        }
        println!(
            "  {} installed {} {} {}",
            ok_mark(),
            cyan(vendor),
            bold(&full),
            dim(&format!("-> {}", dest.display()))
        );
    }
    cmd_reshim();
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL shapes verified against the vendors' real published assets —
    /// one test per constructible vendor.
    #[test]
    fn url_sapmachine() {
        assert_eq!(
            sapmachine_url("21.0.4", "linux", "aarch64", "tar.gz"),
            "https://github.com/SAP/SapMachine/releases/download/sapmachine-21.0.4/sapmachine-jdk-21.0.4_linux-aarch64_bin.tar.gz"
        );
    }

    #[test]
    fn url_graalce() {
        assert_eq!(
            graalce_url("21.0.2", "macos", "aarch64", "tar.gz"),
            "https://github.com/graalvm/graalvm-ce-builds/releases/download/jdk-21.0.2/graalvm-community-jdk-21.0.2_macos-aarch64_bin.tar.gz"
        );
    }

    #[test]
    fn url_liberica() {
        assert_eq!(
            liberica_url("21.0.12+10", "macos", "aarch64", "tar.gz", false),
            "https://github.com/bell-sw/Liberica/releases/download/21.0.12+10/bellsoft-jdk21.0.12+10-macos-aarch64.tar.gz"
        );
        // x86_64 is "amd64" in liberica's naming; musl gets a suffix
        assert_eq!(
            liberica_url("21.0.12+10", "linux", "x64", "tar.gz", true),
            "https://github.com/bell-sw/Liberica/releases/download/21.0.12+10/bellsoft-jdk21.0.12+10-linux-amd64-musl.tar.gz"
        );
    }
}
