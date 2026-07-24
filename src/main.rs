//! jolta — automatic per-project JDK switching, Volta-style.
//!
//! One binary, two personalities: invoked as `jolta` it is the CLI; invoked
//! through a symlink named after a JDK tool (`java`, `javac`, ...) it acts as
//! that tool's shim — resolve the pinned JDK, set JAVA_HOME, exec the real
//! binary. Behavior is kept in lockstep with the reference sh implementation
//! on main; test/smoke.sh is the conformance suite.

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// JDK distros jolta knows how to download. The default is temurin.
const INSTALLABLE_VENDORS: [&str; 4] = ["temurin", "corretto", "graalvm", "oracle"];
/// Vendors recognized for pinning/matching (superset of the installable ones).
const KNOWN_VENDORS: [&str; 6] = ["temurin", "corretto", "graalvm", "oracle", "zulu", "openjdk"];

// ------------------------------------------------------------------ ui

fn tty(err: bool) -> bool {
    static OUT: OnceLock<bool> = OnceLock::new();
    static ERR: OnceLock<bool> = OnceLock::new();
    let plain = env::var_os("NO_COLOR").is_some()
        || env::var("TERM").is_ok_and(|t| t == "dumb");
    if err {
        *ERR.get_or_init(|| io::stderr().is_terminal() && !plain)
    } else {
        *OUT.get_or_init(|| io::stdout().is_terminal() && !plain)
    }
}

fn paint(code: &str, s: &str, err: bool) -> String {
    if tty(err) {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn bold(s: &str) -> String { paint("1", s, false) }
fn dim(s: &str) -> String { paint("2", s, false) }
fn green(s: &str) -> String { paint("32", s, false) }
fn red(s: &str) -> String { paint("31", s, false) }
fn yellow(s: &str) -> String { paint("33", s, false) }
fn cyan(s: &str) -> String { paint("36", s, false) }

fn ok_mark() -> String { green("✓") }
fn bad_mark() -> String { red("✗") }
fn warn_mark() -> String { yellow("!") }

fn die(msg: &str) -> ! {
    eprintln!("{} {msg}", paint("31", "jolta:", true));
    exit(1);
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1e9)
    } else if b >= 1_000_000 {
        format!("{:.1} MB", b as f64 / 1e6)
    } else {
        format!("{} kB", b / 1000)
    }
}

// ------------------------------------------------------------------ paths

fn home_dir() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| die("HOME is not set")))
}

fn jolta_home() -> PathBuf {
    env::var("JOLTA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".jolta"))
}

fn shims_dir() -> PathBuf {
    jolta_home().join("shims")
}

// ------------------------------------------------------------------ versions

/// Split a spec into (vendor, version): "corretto@21" / "corretto-21" ->
/// (Some("corretto"), "21"); "21.0.4" -> (None, "21.0.4").
fn parse_spec(spec: &str) -> (Option<&'static str>, String) {
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
fn major_of(version: &str) -> Option<u32> {
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

/// Sortable key from "21.0.4+7" -> 21_000_004 etc.
fn numkey(version: &str) -> u64 {
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

// ------------------------------------------------------------------ discovery

/// Full JAVA_VERSION from a JDK home's release file.
fn jdk_version(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("JAVA_VERSION=\"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

/// Which distro a JDK home is, from its release IMPLEMENTOR or its path.
fn vendor_of(home: &Path) -> Option<&'static str> {
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
fn managed_home(dir: &Path) -> PathBuf {
    let bundled = dir.join("Contents/Home");
    if bundled.is_dir() {
        bundled
    } else {
        dir.to_path_buf()
    }
}

/// jolta-managed JDKs as (fullversion, home) pairs.
fn list_managed() -> Vec<(String, PathBuf)> {
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
fn list_system() -> Vec<(String, PathBuf)> {
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
    for base in [
        PathBuf::from("/usr/lib/jvm"),
        home_dir().join(".sdkman/candidates/java"),
    ] {
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
    out
}

fn list_all() -> Vec<(String, PathBuf)> {
    let mut all = list_managed();
    all.extend(list_system());
    all
}

// ------------------------------------------------------------------ resolution

/// Best home for a spec: filter by major (and distro when the spec names one,
/// e.g. "corretto-21"); exact full-version match wins, else highest build.
/// Managed JDKs come first in `candidates`, matching the sh implementation.
fn best_match(
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

fn clear_cache() {
    let _ = fs::remove_dir_all(jolta_home().join("cache"));
}

/// Resolve a version spec to a JDK home.
/// NOTE: java_home -v is unusable as a fallback — it exits 0 and prints its
/// default JDK even when nothing matches — so matching is strict, list-based.
fn resolve(spec: &str) -> Option<PathBuf> {
    let (vendor, version) = parse_spec(spec);
    let major = major_of(&version)?;
    let cache = cache_path(spec);
    if let Ok(cached) = fs::read_to_string(&cache) {
        let home = PathBuf::from(cached.trim());
        if home.join("bin/java").is_file() {
            return Some(home);
        }
        let _ = fs::remove_file(&cache);
    }
    let home = best_match(&list_all(), vendor, major, &version)?;
    if !home.join("bin/java").is_file() {
        return None;
    }
    let _ = fs::create_dir_all(jolta_home().join("cache"));
    let _ = fs::write(&cache, format!("{}\n", home.display()));
    Some(home)
}

struct Pin {
    spec: Option<String>,
    source: String,
}

/// Find the nearest .java-version walking up from cwd; then default file.
fn read_pin() -> Pin {
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

fn system_default() -> Option<PathBuf> {
    let o = Command::new("/usr/libexec/java_home").output().ok()?;
    if !o.status.success() {
        return None;
    }
    let home = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    if home.as_os_str().is_empty() {
        None
    } else {
        Some(home)
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

struct Resolved {
    home: PathBuf,
    source: String,
}

fn resolve_current(auto_install: bool) -> Resolved {
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

// ------------------------------------------------------------------ shim mode

fn run_shim(tool: &str, args: Vec<String>) -> ! {
    let r = resolve_current(true);
    let bin = r.home.join("bin").join(tool);
    if !bin.is_file() {
        die(&format!(
            "'{tool}' is not provided by the resolved JDK ({})\n  version was selected by: {}",
            r.home.display(),
            r.source
        ));
    }
    let err = Command::new(&bin)
        .args(&args)
        .env("JAVA_HOME", &r.home)
        .exec();
    die(&format!("failed to exec {}: {err}", bin.display()));
}

// ------------------------------------------------------------------ download

/// Content length of the final target of `url`, if the server reports one.
/// (Last Content-Length header across the redirect chain wins.)
fn content_length(url: &str) -> Option<u64> {
    let o = Command::new("curl").args(["-sIL"]).arg(url).output().ok()?;
    let headers = String::from_utf8_lossy(&o.stdout);
    let mut len = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if let Ok(v) = value.trim().parse::<u64>() {
                    len = Some(v);
                }
            }
        }
    }
    len
}

/// Download with an animated progress bar (spinner + bar + MB + throughput)
/// when stderr is a terminal; plain single-line logging otherwise.
fn download(url: &str, dest: &Path, label: &str) -> bool {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const BAR: usize = 24;

    let total = content_length(url).filter(|t| *t > 0);
    let mut child = match Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} cannot run curl: {e}", paint("31", "jolta:", true));
            return false;
        }
    };

    let started = Instant::now();
    let fancy = tty(true);
    if !fancy {
        eprintln!(
            "jolta: downloading {label}{}",
            total.map(|t| format!(" ({})", fmt_bytes(t))).unwrap_or_default()
        );
    }
    let mut frame = 0usize;
    let mut last_bytes = 0u64;
    let mut last_t = started;
    let mut speed = 0f64;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => die(&format!("curl: {e}")),
        }
        if fancy {
            let bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            let now = Instant::now();
            let dt = now.duration_since(last_t).as_secs_f64();
            if dt > 0.5 {
                speed = (bytes.saturating_sub(last_bytes)) as f64 / dt;
                last_bytes = bytes;
                last_t = now;
            }
            let (bar, pct) = match total {
                Some(t) => {
                    let done = ((bytes as f64 / t as f64) * BAR as f64) as usize;
                    let done = done.min(BAR);
                    (
                        format!("{}{}", "█".repeat(done), "░".repeat(BAR - done)),
                        format!("{:>3.0}%", bytes as f64 / t as f64 * 100.0),
                    )
                }
                None => ("░".repeat(BAR), "  ?%".into()),
            };
            let speed_s = if speed > 0.0 {
                format!("  {}/s", fmt_bytes(speed as u64))
            } else {
                String::new()
            };
            eprint!(
                "\r\x1b[2K  \x1b[36m{}\x1b[0m {label}  \x1b[36m{bar}\x1b[0m {pct}  {}{}\x1b[2m{speed_s}\x1b[0m ",
                FRAMES[frame % FRAMES.len()],
                fmt_bytes(bytes),
                total.map(|t| format!(" / {}", fmt_bytes(t))).unwrap_or_default()
            );
            let _ = io::stderr().flush();
            frame += 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if fancy {
        eprint!("\r\x1b[2K");
    }
    if status.success() {
        eprintln!(
            "  {} downloaded {label}  {}",
            paint("32", "✓", true),
            paint(
                "2",
                &format!("{} in {:.1}s", fmt_bytes(bytes), started.elapsed().as_secs_f64()),
                true
            )
        );
        true
    } else {
        eprintln!("  {} download failed for {label}", paint("31", "✗", true));
        false
    }
}

// ------------------------------------------------------------------ commands

fn usage() {
    print!(
        "{} — automatic per-project JDK switching (like Volta, for Java)

{}: jolta <command> [args]

  setup                  Install shims and add jolta to your shell profile
  pin <spec>             Pin a Java version for this project (.java-version)
  default <spec>         Set the global fallback Java version
  install <spec>         Download a JDK (e.g. 21, corretto@21, graalvm@25)
  uninstall <name>       Remove a jolta-managed JDK (see 'jolta list' for names)
  list, ls               List installed JDKs and where they come from
  jdks                   Machine-readable list: major<TAB>version<TAB>distro<TAB>home
  current                Show the Java version resolved for this directory
  which [tool]           Show the full path the shim would exec (default: java)
  exec <cmd> [args]      Run a command with JAVA_HOME and PATH set for this project
  env                    Print export statements for eval in scripts
  home                   Print the resolved JAVA_HOME for this directory
  hook [zsh|bash]        Print shell hook code that keeps JAVA_HOME in sync on cd
  reshim                 Regenerate shims from installed JDKs
  doctor                 Diagnose common setup problems
  implode                Uninstall jolta completely (~/.jolta + shell profile lines)
  version                Print jolta's version

A <spec> is a version with an optional distro {}:
  21   21.0.4   1.8   corretto@21   graalvm-25   temurin@8
Downloadable distros: {} (default temurin).
Distro-less pins match any installed JDK of that major version.

Version resolution order:
  JOLTA_JAVA_VERSION env var  >  nearest .java-version (walking up)  >
  jolta default  >  system default JDK
",
        bold("jolta"),
        bold("Usage"),
        dim("(distro@version or distro-version)"),
        cyan(&INSTALLABLE_VENDORS.join(", "))
    );
}

fn cmd_reshim() {
    let shims = shims_dir();
    let _ = fs::create_dir_all(&shims);
    if let Ok(entries) = fs::read_dir(&shims) {
        for entry in entries.flatten() {
            if entry.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    let mut count = 0u32;
    let mut homes: Vec<PathBuf> = list_all().into_iter().map(|(_, h)| h).collect();
    homes.sort();
    homes.dedup();
    for home in homes {
        let Ok(entries) = fs::read_dir(home.join("bin")) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "jolta" {
                continue;
            }
            let link = shims.join(&name);
            if !link.exists() && symlink(&me, &link).is_ok() {
                count += 1;
            }
        }
    }
    clear_cache();
    println!("{} {count} shims in {}", ok_mark(), shims.display());
}

fn profile_file() -> PathBuf {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let shell = Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("zsh").to_string();
    match shell.as_str() {
        "zsh" => home_dir().join(".zshrc"),
        "bash" => {
            let bp = home_dir().join(".bash_profile");
            if bp.is_file() { bp } else { home_dir().join(".bashrc") }
        }
        _ => home_dir().join(".profile"),
    }
}

fn shell_name() -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("zsh").to_string()
}

fn cmd_setup() {
    let home = jolta_home();
    for sub in ["bin", "jdks", "cache", "shims"] {
        let _ = fs::create_dir_all(home.join(sub));
    }
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    let installed = home.join("bin/jolta");
    if me != installed {
        // Install a self-contained copy so the build/checkout can be deleted
        let _ = fs::remove_file(&installed);
        fs::copy(&me, &installed).unwrap_or_else(|e| die(&format!("cannot install to {}: {e}", installed.display())));
        let _ = fs::set_permissions(&installed, fs::Permissions::from_mode(0o755));
        println!(
            "{} installed to {} {}",
            ok_mark(),
            home.display(),
            dim("(this checkout is no longer needed at runtime)")
        );
        // reshim via the installed copy so shims point at it
        let st = Command::new(&installed).arg("reshim").status();
        if !st.is_ok_and(|s| s.success()) {
            die("reshim via installed copy failed");
        }
    } else {
        cmd_reshim();
    }

    let profile = profile_file();
    let existing = fs::read_to_string(&profile).unwrap_or_default();
    let mut additions = String::new();
    if existing.contains(">>> jolta >>>") {
        println!("{} PATH setup already in {}", ok_mark(), profile.display());
    } else {
        additions.push_str(
            "\n# >>> jolta >>>\nexport JOLTA_HOME=\"$HOME/.jolta\"\nexport PATH=\"$JOLTA_HOME/shims:$JOLTA_HOME/bin:$PATH\"\n# <<< jolta <<<\n",
        );
        println!("{} added PATH setup to {}", ok_mark(), profile.display());
    }
    if existing.contains("jolta hook") {
        println!("{} JAVA_HOME hook already in {}", ok_mark(), profile.display());
    } else {
        additions.push_str(&format!(
            "\n# >>> jolta hook (keeps JAVA_HOME in sync with your cwd) >>>\neval \"$(jolta hook {})\"\n# <<< jolta hook <<<\n",
            shell_name()
        ));
        println!("{} added JAVA_HOME hook to {}", ok_mark(), profile.display());
    }
    if !additions.is_empty() {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&profile)
            .unwrap_or_else(|e| die(&format!("cannot open {}: {e}", profile.display())));
        f.write_all(additions.as_bytes())
            .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", profile.display())));
    }
    println!("{} setup complete {}", ok_mark(), dim("(open a new shell to activate)"));
}

fn cmd_pin(spec: &str) {
    if resolve(spec).is_none() {
        let (vendor, version) = parse_spec(spec);
        let vendor = vendor.unwrap_or("temurin");
        if env::var("JOLTA_NO_AUTO_INSTALL").is_err()
            && INSTALLABLE_VENDORS.contains(&vendor)
            && major_of(&version).is_some()
        {
            eprintln!(
                "{} no installed JDK matches '{spec}' — fetching it now",
                paint("33", "jolta:", true)
            );
            let major = major_of(&version).unwrap();
            if install_vendor_major(vendor, major).is_err() {
                eprintln!(
                    "{} install failed; pin written anyway ('jolta install {spec}' to retry)",
                    paint("33", "jolta: warning:", true)
                );
            }
        } else {
            eprintln!(
                "{} no installed JDK currently matches '{spec}' ('jolta install {spec}' to fetch it)",
                paint("33", "jolta: warning:", true)
            );
        }
    }
    fs::write(".java-version", format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write .java-version: {e}")));
    let cwd = env::current_dir().unwrap_or_default();
    println!("{} pinned Java {} in {}/.java-version", ok_mark(), bold(spec), cwd.display());
}

fn cmd_default(spec: &str) {
    if resolve(spec).is_none() {
        eprintln!(
            "{} no installed JDK currently matches '{spec}'",
            paint("33", "jolta: warning:", true)
        );
    }
    let _ = fs::create_dir_all(jolta_home());
    fs::write(jolta_home().join("default"), format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write default: {e}")));
    println!("{} default Java version set to {}", ok_mark(), bold(spec));
}

fn cmd_list() {
    let pin = read_pin();
    let current = match &pin.spec {
        Some(spec) => resolve(spec),
        None => system_default(),
    };
    let row = |v: &str, h: &PathBuf| {
        let active = Some(h) == current.as_ref();
        let vendor = vendor_of(h).unwrap_or("?");
        if active {
            format!(
                "  {} {}",
                green("*"),
                bold(&format!("{:<12} {:<9} {}", v, vendor, h.display()))
            )
        } else {
            format!(
                "    {v:<12} {} {}",
                cyan(&format!("{vendor:<9}")),
                dim(&h.display().to_string())
            )
        }
    };
    println!(
        "{} {}:",
        bold("jolta-managed"),
        dim(&format!("({}/jdks)", jolta_home().display()))
    );
    let managed = list_managed();
    if managed.is_empty() {
        println!("    {}", dim("(none — use \"jolta install <spec>\")"));
    } else {
        for (v, h) in &managed {
            println!("{}", row(v, h));
        }
    }
    println!("{}:", bold("system"));
    let mut system = list_system();
    system.sort();
    system.dedup();
    for (v, h) in &system {
        println!("{}", row(v, h));
    }
    match &pin.spec {
        Some(spec) => println!(
            "\n{} = active here {}",
            green("*"),
            dim(&format!("(pinned '{spec}' by {})", pin.source))
        ),
        None => println!(
            "\n{} = active here {}",
            green("*"),
            dim("(system default; nothing pinned)")
        ),
    }
}

fn cmd_jdks() {
    let mut all = list_all();
    all.sort();
    all.dedup();
    for (v, h) in all {
        if let Some(m) = major_of(&v) {
            println!("{m}\t{v}\t{}\t{}", vendor_of(&h).unwrap_or("?"), h.display());
        }
    }
}

fn cmd_current() {
    let r = resolve_current(false);
    let v = jdk_version(&r.home).unwrap_or_else(|| "unknown".into());
    let vendor = vendor_of(&r.home).map(|s| format!(" ({s})")).unwrap_or_default();
    println!("{}{} {}", bold(&v), cyan(&vendor), dim(&format!("(from {})", r.source)));
    println!("{}", r.home.display());
}

fn cmd_which(tool: &str) {
    let r = resolve_current(false);
    let bin = r.home.join("bin").join(tool);
    if !bin.is_file() {
        die(&format!("'{tool}' not found in {}", r.home.display()));
    }
    println!("{}", bin.display());
}

fn cmd_exec(argv: &[String]) -> ! {
    let r = resolve_current(true);
    let path = format!(
        "{}/bin:{}",
        r.home.display(),
        env::var("PATH").unwrap_or_default()
    );
    let err = Command::new(&argv[0])
        .args(&argv[1..])
        .env("JAVA_HOME", &r.home)
        .env("PATH", path)
        .exec();
    die(&format!("failed to exec {}: {err}", argv[0]));
}

fn cmd_env() {
    let r = resolve_current(true);
    println!("export JAVA_HOME=\"{}\"", r.home.display());
    println!("export PATH=\"{}/bin:$PATH\"", r.home.display());
}

fn cmd_home() {
    let r = resolve_current(false);
    println!("{}", r.home.display());
}

fn cmd_hook(shell: &str) {
    match shell {
        "zsh" => print!(
            "_jolta_update_java_home() {{\n  local _jh\n  if _jh=$(jolta home 2>/dev/null); then\n    export JAVA_HOME=$_jh\n  else\n    unset JAVA_HOME\n  fi\n}}\nautoload -Uz add-zsh-hook\nadd-zsh-hook chpwd _jolta_update_java_home\n_jolta_update_java_home\n"
        ),
        "bash" => print!(
            "_jolta_update_java_home() {{\n  if [ \"${{_JOLTA_LAST_PWD:-}}\" = \"$PWD\" ]; then return; fi\n  _JOLTA_LAST_PWD=$PWD\n  local _jh\n  if _jh=$(jolta home 2>/dev/null); then\n    export JAVA_HOME=$_jh\n  else\n    unset JAVA_HOME\n  fi\n}}\ncase \";$PROMPT_COMMAND;\" in\n  *\";_jolta_update_java_home;\"*) ;;\n  *) PROMPT_COMMAND=\"_jolta_update_java_home${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\" ;;\nesac\n_jolta_update_java_home\n"
        ),
        other => die(&format!("no hook available for shell '{other}' (zsh and bash are supported)")),
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var("PATH").ok()?;
    for dir in path.split(':') {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Download URL for the latest GA build of a distro + major on this platform.
fn vendor_url(vendor: &str, major: u32) -> String {
    let os = if env::consts::OS == "macos" { "macos" } else { "linux" };
    let arch = match env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x64",
        other => die(&format!("unsupported architecture: {other}")),
    };
    match vendor {
        "temurin" => {
            let os = if os == "macos" { "mac" } else { os };
            format!("https://api.adoptium.net/v3/binary/latest/{major}/ga/{os}/{arch}/jdk/hotspot/normal/eclipse")
        }
        "corretto" => format!("https://corretto.aws/downloads/latest/amazon-corretto-{major}-{arch}-{os}-jdk.tar.gz"),
        "oracle" => format!("https://download.oracle.com/java/{major}/latest/jdk-{major}_{os}-{arch}_bin.tar.gz"),
        "graalvm" => format!("https://download.oracle.com/graalvm/{major}/latest/graalvm-jdk-{major}_{os}-{arch}_bin.tar.gz"),
        other => die(&format!(
            "don't know how to download '{other}' builds (downloadable distros: {})",
            INSTALLABLE_VENDORS.join(", ")
        )),
    }
}

fn install_vendor_major(vendor: &str, major: u32) -> Result<(), ()> {
    if env::consts::OS != "macos" && env::consts::OS != "linux" {
        die(&format!("unsupported OS: {}", env::consts::OS));
    }

    // Serialize concurrent installs of the same distro+major (parallel builds
    // can fire several shims at once); losers wait, then re-check.
    let _ = fs::create_dir_all(jolta_home().join("cache"));
    let lock = jolta_home().join("cache").join(format!("install-{vendor}-{major}.lock"));
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
            println!("jolta: another install of {vendor} {major} is running; waiting...");
            let mut waited = 0;
            while lock.is_dir() && waited < 600 {
                std::thread::sleep(Duration::from_secs(2));
                waited += 2;
            }
            if resolve(&format!("{vendor}-{major}")).is_some() {
                println!("{} {vendor} {major} installed by the other process", ok_mark());
                return Ok(());
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

    let url = vendor_url(vendor, major);
    let tmp = env::temp_dir().join(format!("jolta-install-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let _tmp_guard = TmpGuard(tmp.clone());

    let tarball = tmp.join("jdk.tar.gz");
    let label = format!("{vendor}@{major}");
    if !download(&url, &tarball, &label) {
        eprintln!(
            "{} is {vendor} {major} published for {}/{}? ({url})",
            paint("2", "hint:", true),
            env::consts::OS,
            env::consts::ARCH
        );
        return Err(());
    }

    let extract = tmp.join("x");
    let _ = fs::create_dir_all(&extract);
    let st = Command::new("tar")
        .arg("-xzf")
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
    Ok(())
}

fn cmd_install(spec: &str) {
    let (vendor, version) = parse_spec(spec);
    let vendor = vendor.unwrap_or("temurin");
    let major = major_of(&version)
        .unwrap_or_else(|| die(&format!("cannot parse version '{spec}' (try e.g. 21 or corretto@21)")));
    if install_vendor_major(vendor, major).is_err() {
        exit(1);
    }
}

fn cmd_uninstall(name: &str) {
    let dest = jolta_home().join("jdks").join(name);
    if !dest.is_dir() {
        die(&format!("'{name}' is not a jolta-managed JDK (see 'jolta list')"));
    }
    fs::remove_dir_all(&dest).unwrap_or_else(|e| die(&format!("cannot remove {}: {e}", dest.display())));
    clear_cache();
    println!("{} removed {}", ok_mark(), dest.display());
}

fn cmd_implode(args: &[String]) {
    let home = jolta_home();
    let profile = profile_file();
    if args.first().map(String::as_str) != Some("--yes") {
        println!(
            "This removes {} (including downloaded JDKs) and the jolta lines from {}.",
            home.display(),
            profile.display()
        );
        print!("Type \"yes\" to continue: ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().lock().read_line(&mut answer);
        if answer.trim() != "yes" {
            die("aborted");
        }
    }
    if let Ok(text) = fs::read_to_string(&profile) {
        let mut out = String::new();
        let mut skipping = false;
        for line in text.lines() {
            if line.contains("# >>> jolta hook") || line.contains("# >>> jolta >>>") {
                skipping = true;
            }
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
            if line.contains("# <<< jolta hook <<<") || line.contains("# <<< jolta <<<") {
                skipping = false;
            }
        }
        if fs::write(&profile, out).is_ok() {
            println!("{} removed jolta lines from {}", ok_mark(), profile.display());
        }
    }
    let _ = fs::remove_dir_all(&home);
    println!("{} removed {} — open a new shell to finish. So long!", ok_mark(), home.display());
}

fn cmd_doctor() -> i32 {
    let mut rc = 0;
    let home = jolta_home();
    println!("{}", bold("jolta doctor"));
    println!("  jolta home:    {}", home.display());
    println!(
        "  binary:        {}",
        env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
    );

    let shim_count = fs::read_dir(shims_dir())
        .map(|e| e.flatten().filter(|x| x.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())).count())
        .unwrap_or(0);
    if shim_count > 0 {
        println!("  shims:         {} ok ({shim_count} installed)", ok_mark());
    } else {
        println!("  shims:         {} MISSING — run \"jolta setup\"", bad_mark());
        rc = 1;
    }

    let path = env::var("PATH").unwrap_or_default();
    let shims = shims_dir().display().to_string();
    if path.split(':').any(|d| d == shims) {
        println!("  PATH:          {} ok (shims dir is on PATH)", ok_mark());
    } else {
        println!("  PATH:          {} shims dir NOT on PATH — run \"jolta setup\" and open a new shell", bad_mark());
        rc = 1;
    }

    match which("java") {
        Some(p) if p.starts_with(&shims_dir()) => {
            println!("  java:          {} ok (resolves to the jolta shim)", ok_mark())
        }
        Some(p) => {
            println!("  java:          {} BYPASSING jolta ({} comes before the shims on PATH)", bad_mark(), p.display());
            rc = 1;
        }
        None => {
            println!("  java:          {} not found on PATH", bad_mark());
            rc = 1;
        }
    }

    let expected = {
        let pin = read_pin();
        match pin.spec {
            Some(s) => resolve(&s),
            None => system_default(),
        }
    };
    match env::var("JAVA_HOME") {
        Ok(jh) if Some(PathBuf::from(&jh)) == expected => {
            println!("  JAVA_HOME:     {} ok ({jh} — matches the pin here, hook is working)", ok_mark());
        }
        Ok(jh) => {
            println!("  JAVA_HOME:     {} STALE: {jh}", bad_mark());
            println!(
                "                 expected {} for this directory",
                expected.map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolvable>".into())
            );
            println!("                 mvn/gradle use JAVA_HOME directly and will bypass jolta;");
            println!("                 remove any manual \"export JAVA_HOME\" from your shell profile");
            println!("                 and make sure the jolta hook line comes after it (\"jolta setup\" adds it)");
            rc = 1;
        }
        Err(_) => {
            println!("  JAVA_HOME:     {} not set — shims still work, but mvn/gradle prefer JAVA_HOME;", warn_mark());
            println!("                 run \"jolta setup\" to install the cd hook that keeps it in sync");
        }
    }

    let count = list_all().len();
    println!("  JDKs found:    {count}");
    if count == 0 {
        println!("                 {} none! install one with \"jolta install 21\"", bad_mark());
        rc = 1;
    }

    let pin = read_pin();
    match pin.spec {
        Some(s) => println!("  pin here:      '{}' via {}", bold(&s), pin.source),
        None => println!("  pin here:      none (system default JDK will be used)"),
    }
    rc
}

// ------------------------------------------------------------------ main

fn main() {
    let mut args: Vec<String> = env::args().collect();
    let invoked = Path::new(&args[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("jolta")
        .to_string();

    if invoked != "jolta" {
        run_shim(&invoked, args.split_off(1));
    }

    let cmd = args.get(1).cloned().unwrap_or_else(|| "help".into());
    let rest: Vec<String> = args.iter().skip(2).cloned().collect();
    match cmd.as_str() {
        "setup" => cmd_setup(),
        "pin" => match rest.first() {
            Some(s) => cmd_pin(s),
            None => die("usage: jolta pin <spec>  (e.g. 21 or corretto@21)"),
        },
        "default" => match rest.first() {
            Some(s) => cmd_default(s),
            None => die("usage: jolta default <spec>"),
        },
        "install" => match rest.first() {
            Some(s) => cmd_install(s),
            None => die("usage: jolta install <spec>  (e.g. 21, corretto@21, graalvm@25)"),
        },
        "uninstall" => match rest.first() {
            Some(s) => cmd_uninstall(s),
            None => die(&format!(
                "usage: jolta uninstall <name>  (a directory name from {}/jdks)",
                jolta_home().display()
            )),
        },
        "list" | "ls" => cmd_list(),
        "jdks" => cmd_jdks(),
        "current" => cmd_current(),
        "which" => cmd_which(rest.first().map(String::as_str).unwrap_or("java")),
        "exec" => {
            if rest.is_empty() {
                die("usage: jolta exec <command> [args...]");
            }
            cmd_exec(&rest);
        }
        "env" => cmd_env(),
        "home" => cmd_home(),
        "hook" => {
            let shell = rest.first().cloned().unwrap_or_else(shell_name);
            cmd_hook(&shell);
        }
        "reshim" => cmd_reshim(),
        "doctor" => exit(cmd_doctor()),
        "implode" => cmd_implode(&rest),
        "version" | "-v" | "--version" => println!("jolta {VERSION}"),
        "help" | "-h" | "--help" => usage(),
        other => {
            usage();
            die(&format!("unknown command '{other}'"));
        }
    }
}
