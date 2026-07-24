//! jolta — automatic per-project JDK switching, Volta-style.
//!
//! One binary, two personalities: invoked as `jolta` it is the CLI; invoked
//! through a symlink named after a JDK tool (`java`, `javac`, ...) it acts as
//! that tool's shim — resolve the pinned JDK, set JAVA_HOME, exec the real
//! binary. Behavior is kept in lockstep with the reference sh implementation
//! on master; test/smoke.sh is the conformance suite.

use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn die(msg: &str) -> ! {
    eprintln!("jolta: {msg}");
    exit(1);
}

fn home_dir() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| die("HOME is not set")))
}

fn jolta_home() -> PathBuf {
    env::var("JOLTA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".jolta"))
}

// ---------------------------------------------------------------- versions

/// Major version of a spec: "21"->21, "21.0.4"->21, "1.8"->8, "temurin-21"->21.
fn major_of(spec: &str) -> Option<u32> {
    let mut s = spec.trim();
    if let Some(idx) = s.rfind('-') {
        let tail = &s[idx + 1..];
        if tail.starts_with(|c: char| c.is_ascii_digit()) {
            s = tail;
        }
    }
    let first: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let first: u32 = first.parse().ok()?;
    if first == 1 {
        // legacy "1.8" style
        let rest = s.split('.').nth(1)?;
        let second: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return second.parse().ok();
    }
    Some(first)
}

/// Sortable key from "21.0.4+7" -> 21_000_004 etc. (matches the awk numkey).
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

// ---------------------------------------------------------------- discovery

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

// ---------------------------------------------------------------- resolution

/// Best home for a major: exact full-version match wins, else highest build.
/// Managed JDKs come first in `candidates`, matching the sh implementation.
fn best_match(candidates: &[(String, PathBuf)], major: u32, spec: &str) -> Option<PathBuf> {
    let mut best: Option<(u64, &PathBuf)> = None;
    let mut exact: Option<&PathBuf> = None;
    for (v, home) in candidates {
        if major_of(v) != Some(major) {
            continue;
        }
        if v == spec {
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
    let major = major_of(spec)?;
    let cache = cache_path(spec);
    if let Ok(cached) = fs::read_to_string(&cache) {
        let home = PathBuf::from(cached.trim());
        if home.join("bin/java").is_file() {
            return Some(home);
        }
        let _ = fs::remove_file(&cache);
    }
    let home = best_match(&list_all(), major, spec)?;
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
    let Some(major) = major_of(spec) else { return false };
    eprintln!(
        "jolta: Java {spec} is pinned here but not installed — fetching Temurin {major} \
         (set JOLTA_NO_AUTO_INSTALL=1 to disable)"
    );
    install_major(major).is_ok()
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
                    let mut vs: Vec<String> = list_all().into_iter().map(|(v, _)| v).collect();
                    vs.sort();
                    vs.dedup();
                    vs
                };
                die(&format!(
                    "no installed JDK matches '{spec}' (pinned by {})\n  installed JDKs: {}\n  \
                     run 'jolta install {spec}' to download it (Temurin), or 'jolta list' to see what's available",
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

// ---------------------------------------------------------------- shim mode

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

// ---------------------------------------------------------------- commands

fn usage() {
    print!(
        "jolta — automatic per-project JDK switching (like Volta, for Java)

Usage: jolta <command> [args]

  setup                Install shims and add jolta to your shell profile
  pin <version>        Pin a Java version for this project (.java-version)
  default <version>    Set the global fallback Java version
  install <major>      Download and install a Temurin JDK (e.g. jolta install 21)
  uninstall <name>     Remove a jolta-managed JDK (see 'jolta list' for names)
  list, ls             List installed JDKs and where they come from
  jdks                 Machine-readable list: major<TAB>version<TAB>home
  current              Show the Java version resolved for this directory
  which [tool]         Show the full path the shim would exec (default: java)
  exec <cmd> [args]    Run a command with JAVA_HOME and PATH set for this project
  env                  Print export statements for eval in scripts
  home                 Print the resolved JAVA_HOME for this directory
  hook [zsh|bash]      Print shell hook code that keeps JAVA_HOME in sync on cd
  reshim               Regenerate shims from installed JDKs
  doctor               Diagnose common setup problems
  implode              Uninstall jolta completely (~/.jolta + shell profile lines)
  version              Print jolta's version

Version resolution order:
  JOLTA_JAVA_VERSION env var  >  nearest .java-version (walking up)  >
  jolta default  >  system default JDK
"
    );
}

fn shims_dir() -> PathBuf {
    jolta_home().join("shims")
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
    println!("jolta: {count} shims in {}", shims.display());
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
        println!("jolta: installed to {} (this checkout is no longer needed at runtime)", home.display());
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
        println!("jolta: PATH setup already in {}", profile.display());
    } else {
        additions.push_str(
            "\n# >>> jolta >>>\nexport JOLTA_HOME=\"$HOME/.jolta\"\nexport PATH=\"$JOLTA_HOME/shims:$JOLTA_HOME/bin:$PATH\"\n# <<< jolta <<<\n",
        );
        println!("jolta: added PATH setup to {}", profile.display());
    }
    if existing.contains("jolta hook") {
        println!("jolta: JAVA_HOME hook already in {}", profile.display());
    } else {
        additions.push_str(&format!(
            "\n# >>> jolta hook (keeps JAVA_HOME in sync with your cwd) >>>\neval \"$(jolta hook {})\"\n# <<< jolta hook <<<\n",
            shell_name()
        ));
        println!("jolta: added JAVA_HOME hook to {}", profile.display());
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
    println!("jolta: setup complete (open a new shell to activate)");
}

fn cmd_pin(spec: &str) {
    if resolve(spec).is_none() {
        if env::var("JOLTA_NO_AUTO_INSTALL").is_err() && which("curl").is_some() {
            eprintln!("jolta: no installed JDK matches '{spec}' — fetching it now");
            if let Some(major) = major_of(spec) {
                if install_major(major).is_err() {
                    eprintln!("jolta: warning: install failed; pin written anyway ('jolta install {spec}' to retry)");
                }
            }
        } else {
            eprintln!("jolta: warning: no installed JDK currently matches '{spec}' ('jolta install {spec}' to fetch it)");
        }
    }
    fs::write(".java-version", format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write .java-version: {e}")));
    let cwd = env::current_dir().unwrap_or_default();
    println!("jolta: pinned Java {spec} in {}/.java-version", cwd.display());
}

fn cmd_default(spec: &str) {
    if resolve(spec).is_none() {
        eprintln!("jolta: warning: no installed JDK currently matches '{spec}'");
    }
    let _ = fs::create_dir_all(jolta_home());
    fs::write(jolta_home().join("default"), format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write default: {e}")));
    println!("jolta: default Java version set to {spec}");
}

fn cmd_list() {
    let pin = read_pin();
    let current = match &pin.spec {
        Some(spec) => resolve(spec),
        None => system_default(),
    };
    let mark = |h: &PathBuf| if Some(h) == current.as_ref() { "*" } else { " " };
    println!("jolta-managed ({}/jdks):", jolta_home().display());
    let managed = list_managed();
    if managed.is_empty() {
        println!("   (none — use \"jolta install <major>\")");
    } else {
        for (v, h) in &managed {
            println!("  {} {:<12} {}", mark(h), v, h.display());
        }
    }
    println!("system:");
    let mut system = list_system();
    system.sort();
    system.dedup();
    for (v, h) in &system {
        println!("  {} {:<12} {}", mark(h), v, h.display());
    }
    match &pin.spec {
        Some(spec) => println!("\n* = active here (pinned '{spec}' by {})", pin.source),
        None => println!("\n* = active here (system default; nothing pinned)"),
    }
}

fn cmd_jdks() {
    let mut all = list_all();
    all.sort();
    all.dedup();
    for (v, h) in all {
        if let Some(m) = major_of(&v) {
            println!("{m}\t{v}\t{}", h.display());
        }
    }
}

fn cmd_current() {
    let r = resolve_current(false);
    let v = jdk_version(&r.home).unwrap_or_else(|| "unknown".into());
    println!("{v} (from {})", r.source);
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

fn install_major(major: u32) -> Result<(), ()> {
    let os = match env::consts::OS {
        "macos" => "mac",
        "linux" => "linux",
        other => die(&format!("unsupported OS: {other}")),
    };
    let arch = match env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x64",
        other => die(&format!("unsupported architecture: {other}")),
    };

    // Serialize concurrent installs of the same major (parallel builds can
    // fire several shims at once); whoever loses the race waits, then re-checks.
    let _ = fs::create_dir_all(jolta_home().join("cache"));
    let lock = jolta_home().join("cache").join(format!("install-{major}.lock"));
    let lock_guard = match fs::create_dir(&lock) {
        Ok(()) => Some(LockGuard(lock.clone())),
        Err(_) => {
            println!("jolta: another install of Temurin {major} is running; waiting...");
            let mut waited = 0;
            while lock.is_dir() && waited < 600 {
                std::thread::sleep(std::time::Duration::from_secs(2));
                waited += 2;
            }
            if resolve(&major.to_string()).is_some() {
                println!("jolta: Temurin {major} installed by the other process");
                return Ok(());
            }
            if lock.is_dir() {
                die(&format!("timed out waiting for concurrent install (remove {} if stale)", lock.display()));
            }
            match fs::create_dir(&lock) {
                Ok(()) => Some(LockGuard(lock.clone())),
                Err(_) => die(&format!("could not acquire install lock {}", lock.display())),
            }
        }
    };

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

    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/{major}/ga/{os}/{arch}/jdk/hotspot/normal/eclipse"
    );
    let tmp = env::temp_dir().join(format!("jolta-install-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let _tmp_guard = TmpGuard(tmp.clone());

    println!("jolta: downloading Temurin {major} ({os}/{arch}) from Adoptium...");
    let tarball = tmp.join("jdk.tar.gz");
    let st = Command::new("curl")
        .args(["-fSL", "--progress-bar", "-o"])
        .arg(&tarball)
        .arg(&url)
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("jolta: download failed — is Temurin {major} published for {os}/{arch}? ({url})");
        drop(lock_guard);
        return Err(());
    }

    println!("jolta: extracting...");
    let extract = tmp.join("x");
    let _ = fs::create_dir_all(&extract);
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&extract)
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("jolta: extraction failed");
        drop(lock_guard);
        return Err(());
    }
    let top = fs::read_dir(&extract)
        .ok()
        .and_then(|mut e| e.find_map(|x| x.ok().map(|x| x.path()).filter(|p| p.is_dir())))
        .unwrap_or_else(|| die("unexpected archive layout"));

    let home = managed_home(&top);
    let full = jdk_version(&home).unwrap_or_else(|| die("no release file in extracted JDK"));

    let dest = jolta_home().join("jdks").join(format!("temurin-{full}"));
    if dest.is_dir() {
        println!("jolta: Temurin {full} is already installed");
    } else {
        let _ = fs::create_dir_all(jolta_home().join("jdks"));
        fs::rename(&top, &dest).unwrap_or_else(|e| die(&format!("cannot move JDK into place: {e}")));
        println!("jolta: installed Temurin {full} -> {}", dest.display());
    }
    drop(lock_guard);
    cmd_reshim();
    Ok(())
}

fn cmd_uninstall(name: &str) {
    let dest = jolta_home().join("jdks").join(name);
    if !dest.is_dir() {
        die(&format!("'{name}' is not a jolta-managed JDK (see 'jolta list')"));
    }
    fs::remove_dir_all(&dest).unwrap_or_else(|e| die(&format!("cannot remove {}: {e}", dest.display())));
    clear_cache();
    println!("jolta: removed {}", dest.display());
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
            println!("jolta: removed jolta lines from {}", profile.display());
        }
    }
    let _ = fs::remove_dir_all(&home);
    println!("jolta: removed {} — open a new shell to finish. So long!", home.display());
}

fn cmd_doctor() -> i32 {
    let mut ok = 0;
    let home = jolta_home();
    println!("jolta doctor");
    println!("  jolta home:    {}", home.display());
    println!(
        "  binary:        {}",
        env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
    );

    let shim_count = fs::read_dir(shims_dir())
        .map(|e| e.flatten().filter(|x| x.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())).count())
        .unwrap_or(0);
    if shim_count > 0 {
        println!("  shims:         ok ({shim_count} installed)");
    } else {
        println!("  shims:         MISSING — run \"jolta setup\"");
        ok = 1;
    }

    let path = env::var("PATH").unwrap_or_default();
    let shims = shims_dir().display().to_string();
    if path.split(':').any(|d| d == shims) {
        println!("  PATH:          ok (shims dir is on PATH)");
    } else {
        println!("  PATH:          shims dir NOT on PATH — run \"jolta setup\" and open a new shell");
        ok = 1;
    }

    match which("java") {
        Some(p) if p.starts_with(&shims_dir()) => println!("  java:          ok (resolves to the jolta shim)"),
        Some(p) => {
            println!("  java:          BYPASSING jolta ({} comes before the shims on PATH)", p.display());
            ok = 1;
        }
        None => {
            println!("  java:          not found on PATH");
            ok = 1;
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
            println!("  JAVA_HOME:     ok ({jh} — matches the pin here, hook is working)");
        }
        Ok(jh) => {
            println!("  JAVA_HOME:     STALE: {jh}");
            println!(
                "                 expected {} for this directory",
                expected.map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolvable>".into())
            );
            println!("                 mvn/gradle use JAVA_HOME directly and will bypass jolta;");
            println!("                 remove any manual \"export JAVA_HOME\" from your shell profile");
            println!("                 and make sure the jolta hook line comes after it (\"jolta setup\" adds it)");
            ok = 1;
        }
        Err(_) => {
            println!("  JAVA_HOME:     not set — shims still work, but mvn/gradle prefer JAVA_HOME;");
            println!("                 run \"jolta setup\" to install the cd hook that keeps it in sync");
        }
    }

    let count = list_all().len();
    println!("  JDKs found:    {count}");
    if count == 0 {
        println!("                 none! install one with \"jolta install 21\"");
        ok = 1;
    }

    let pin = read_pin();
    match pin.spec {
        Some(s) => println!("  pin here:      '{s}' via {}", pin.source),
        None => println!("  pin here:      none (system default JDK will be used)"),
    }
    ok
}

// ---------------------------------------------------------------- main

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
            None => die("usage: jolta pin <version>"),
        },
        "default" => match rest.first() {
            Some(s) => cmd_default(s),
            None => die("usage: jolta default <version>"),
        },
        "install" => match rest.first().and_then(|s| major_of(s)) {
            Some(m) => {
                if install_major(m).is_err() {
                    exit(1);
                }
            }
            None => die("usage: jolta install <major>  (e.g. jolta install 21)"),
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
