//! Fetching and installing JDK builds from vendor download endpoints.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::commands::cmd_reshim;
use crate::download::download;
use crate::jdk::{is_exact, jdk_version, major_of, managed_home, INSTALLABLE_VENDORS};
use crate::paths::jolta_home;
use crate::resolve::resolve;
use crate::ui::{bold, cyan, die, dim, ok_mark, paint};

fn platform() -> (&'static str, &'static str, &'static str) {
    let os = match env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = match env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x64",
        other => die(&format!("unsupported architecture: {other}")),
    };
    let ext = if os == "windows" { "zip" } else { "tar.gz" };
    (os, arch, ext)
}

use std::sync::OnceLock;

static FRESH: OnceLock<bool> = OnceLock::new();

/// --fresh: bypass the remote-metadata cache for this invocation.
pub fn set_fresh() {
    let _ = FRESH.set(true);
}

fn fresh() -> bool {
    *FRESH.get().unwrap_or(&false) || env::var("JOLTA_FRESH").is_ok()
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
    if fresh() {
        return None;
    }
    let f = jolta_home().join("remote-cache").join(cache_key(key));
    let ttl_hours: u64 = env::var("JOLTA_CACHE_TTL_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let age = fs::metadata(&f).ok()?.modified().ok()?.elapsed().ok()?;
    if age > Duration::from_secs(ttl_hours * 3600) {
        return None;
    }
    fs::read_to_string(&f).ok()
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
    let o = Command::new("curl").args(["-fsSL"]).arg(url).output().ok()?;
    let body = if o.status.success() {
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    };
    remote_cache_put(&format!("body:{url}"), body.as_deref().unwrap_or(""));
    body
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

/// Zulu has no constructible URLs; ask the Azul metadata API. Works for both
/// latest-of-major and exact versions. Filters out JavaFX/CRaC variant builds.
fn zulu_url(version: &str, latest: bool) -> Option<String> {
    let (os, arch, ext) = platform();
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
            let os = if os == "macos" { "mac" } else { os };
            Some(format!(
                "https://api.adoptium.net/v3/binary/version/{}/{os}/{arch}/jdk/hotspot/normal/eclipse",
                release.replace('+', "%2B")
            ))
        }
        "corretto" => {
            // corretto tags are 5-part (21.0.2.13.1); some tagged builds never
            // reach the CDN, so probe every matching tag until one answers
            let tags = curl_text(&format!(
                "https://api.github.com/repos/corretto/corretto-{major}/releases?per_page=100"
            ))?;
            let (osname, suffix) = match os {
                "macos" => ("macosx", format!(".{ext}")),
                "windows" => ("windows", format!("-jdk.{ext}")),
                _ => ("linux", format!(".{ext}")),
            };
            json_strings(&tags, "tag_name")
                .into_iter()
                .filter(|t| t == version || t.starts_with(&format!("{version}.")))
                .map(|tag| {
                    format!(
                        "https://corretto.aws/downloads/resources/{tag}/amazon-corretto-{tag}-{osname}-{arch}{suffix}"
                    )
                })
                .find(|u| url_ok(u))
        }
        "oracle" => Some(format!(
            "https://download.oracle.com/java/{major}/archive/jdk-{version}_{os}-{arch}_bin.{ext}"
        )),
        "graalvm" => Some(format!(
            "https://download.oracle.com/graalvm/{major}/archive/graalvm-jdk-{version}_{os}-{arch}_bin.{ext}"
        )),
        "zulu" => zulu_url(version, false),
        _ => None,
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
        let base = base.trim_end_matches('/');
        return format!("{base}/{vendor}/{major}/{os}-{arch}.{ext}");
    }
    match vendor {
        "temurin" => {
            let os = if os == "macos" { "mac" } else { os };
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
    if env::var("JOLTA_DOWNLOAD_BASE").is_ok() {
        return None; // mirrors serve a stable path; nothing to learn from it
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

/// The Java release universe per Adoptium: (available majors, LTS majors,
/// most recent LTS, most recent feature release).
pub fn release_universe() -> Option<(Vec<u32>, Vec<u32>, u32, u32)> {
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
        "zulu" => return false, // has a metadata API; never needs probing
        _ => vendor_url(vendor, major),
    };
    Command::new("curl")
        .args(["-sIL", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15"])
        .arg(&url)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
        .unwrap_or(false)
}

/// Remove jolta-managed installs of this distro+major other than `keep`.
pub fn prune_superseded(vendor: &str, major: u32, keep: &str) {
    let jdks = jolta_home().join("jdks");
    let Ok(entries) = fs::read_dir(&jdks) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(full) = name.strip_prefix(&format!("{vendor}-")) else { continue };
        if full != keep && crate::jdk::major_of(full) == Some(major) {
            if fs::remove_dir_all(entry.path()).is_ok() {
                println!("  {} pruned {name}", ok_mark());
            }
        }
    }
}

pub fn install_vendor_major(vendor: &str, major: u32) -> Result<String, ()> {
    install_vendor_spec(vendor, &major.to_string())
}

pub fn install_vendor_spec(vendor: &str, version: &str) -> Result<String, ()> {
    if !matches!(env::consts::OS, "macos" | "linux" | "windows") {
        die(&format!("unsupported OS: {}", env::consts::OS));
    }

    let major = major_of(version).unwrap_or_else(|| die(&format!("cannot parse version '{version}'")));
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
                return Ok(jdk_version(&h).unwrap_or_default());
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

    let url = if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
        let (os, arch, ext) = platform();
        format!("{}/{vendor}/{version}/{os}-{arch}.{ext}", base.trim_end_matches('/'))
    } else if is_exact(version) {
        exact_url(vendor, version).unwrap_or_else(|| {
            die(&format!(
                "cannot find {vendor} {version} at the vendor (is that point release published for this platform?)"
            ))
        })
    } else if vendor == "zulu" {
        zulu_url(version, true)
            .unwrap_or_else(|| die(&format!("Azul metadata API returned nothing for zulu {version}")))
    } else {
        vendor_url(vendor, major)
    };
    let tmp = env::temp_dir().join(format!("jolta-install-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let _tmp_guard = TmpGuard(tmp.clone());

    let tarball = tmp.join("jdk.archive");
    let label = format!("{vendor}@{version}");
    if !download(&url, &tarball, &label) {
        eprintln!(
            "{} is {vendor} {version} published for {}/{}? ({url})",
            paint("2", "hint:", true),
            env::consts::OS,
            env::consts::ARCH
        );
        return Err(());
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
    let top = fs::read_dir(&extract)
        .ok()
        .and_then(|mut e| e.find_map(|x| x.ok().map(|x| x.path()).filter(|p| p.is_dir())))
        .unwrap_or_else(|| die("unexpected archive layout"));

    let home = managed_home(&top);
    let full = jdk_version(&home).unwrap_or_else(|| die("no release file in extracted JDK"));

    let dest = jolta_home().join("jdks").join(format!("{vendor}-{full}"));
    if dest.is_dir() {
        println!("  {} {vendor} {full} is already installed", ok_mark());
    } else {
        let _ = fs::create_dir_all(jolta_home().join("jdks"));
        fs::rename(&top, &dest).unwrap_or_else(|e| die(&format!("cannot move JDK into place: {e}")));
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
