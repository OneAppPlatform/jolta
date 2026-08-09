//! CLI command handlers.

use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::install::{
    install_vendor_major, install_vendor_spec, latest_remote_version, probe_latest,
    prune_superseded, release_universe, vendor_versions,
};
use crate::jdk::{
    is_exact, list_all, list_managed, list_system, major_of, numkey, parse_spec, version_of,
    system_default, tool_bin, vendor_of, INSTALLABLE_VENDORS, KNOWN_VENDORS,
};
use crate::platform;
use crate::paths::{home_dir, jolta_home, shims_dir, which};
use crate::resolve::{clear_cache, read_pin, resolve, resolve_current};
use crate::ui::{bad_mark, bold, cyan, die, dim, green, ok_mark, paint, warn_mark, yellow};

/// Expand the `lts` / `latest` version keywords to a concrete major
/// ("lts" -> "25", "corretto@lts" -> "corretto@25") using the live release
/// universe. Non-keyword specs pass through untouched.
fn expand_spec(spec: &str) -> String {
    // A dangling separator carries no information: "@21", "-21" and "21@" all
    // mean 21. Trim it rather than refusing the spec — and, more importantly,
    // rather than writing "21@" verbatim into a .java-version, which is what
    // used to happen. Only the ends are trimmed, so "temurin-21" and "21-ea"
    // keep their interior separators.
    let trimmed = spec.trim().trim_matches(|c| c == '@' || c == '-').trim();
    // All separators and nothing else: keep the original so the error names
    // what the user actually typed.
    let spec = if trimmed.is_empty() { spec.trim() } else { trimmed };
    let (vendor, version) = parse_spec(spec);
    let major = match version.to_ascii_lowercase().as_str() {
        // offline fallback mirrors the first-run bootstrap: the known train
        "lts" => release_universe().map_or(crate::resolve::FALLBACK_LTS, |(_, _, lts, _)| lts),
        "latest" => match release_universe() {
            Some((_, _, _, feature)) => feature,
            None => die("cannot resolve 'latest' — the release index is unreachable (offline?)"),
        },
        _ => return spec.to_string(),
    };
    match vendor {
        Some(v) => format!("{v}@{major}"),
        None => major.to_string(),
    }
}

/// Every tool a current JDK ships. Shimmed unconditionally — before any JDK
/// is installed — so the very first plain `java` reaches jolta and can
/// trigger the bootstrap auto-install instead of falling through to the
/// macOS /usr/bin/java stub ("Unable to locate a Java Runtime").
pub const BASELINE_TOOLS: &[&str] = &[
    "jar", "jarsigner", "java", "javac", "javadoc", "javap", "jcmd", "jconsole", "jdb",
    "jdeprscan", "jdeps", "jfr", "jhsdb", "jimage", "jinfo", "jlink", "jmap", "jmod",
    "jnativescan", "jpackage", "jps", "jrunscript", "jshell", "jstack", "jstat", "jstatd",
    "jwebserver", "keytool", "rmiregistry", "serialver",
];

pub fn cmd_reshim() {
    let shims = shims_dir();
    let _ = fs::create_dir_all(&shims);
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    // When an auto-install fires inside a shim, current_exe on macOS is the
    // shim symlink itself, not the binary. Resolve it BEFORE clearing the
    // shims dir (afterwards the symlink is gone and canonicalize can't), or
    // every rebuilt shim points into the shims dir and java becomes a
    // symlink loop.
    let me = fs::canonicalize(&me).unwrap_or(me);
    let installed = jolta_home().join("bin").join(format!("jolta{}", env::consts::EXE_SUFFIX));
    // Belt and braces: a target inside the shims dir would recreate the loop
    // no matter how we got here — fall back to the installed copy.
    let me = if me.starts_with(&shims) {
        if installed.is_file() {
            installed.clone()
        } else {
            die("cannot locate the real jolta binary (found only the shim) — run 'jolta setup' from the downloaded binary")
        }
    } else {
        me
    };
    // Shims must target the STABLE installed path, never its canonical
    // target: for Homebrew installs bin/jolta is a symlink into brew's opt
    // tree, and the canonical path is a versioned Cellar keg that the next
    // `brew cleanup` deletes — every shim would dangle.
    let me = if me != installed && fs::canonicalize(&installed).ok().as_deref() == Some(&me) {
        installed
    } else {
        me
    };
    // The shims dir is wholly jolta-owned: clear every entry (Windows shims
    // can be hard links or copies, not just symlinks)
    if let Ok(entries) = fs::read_dir(&shims) {
        for entry in entries.flatten() {
            let p = entry.path();
            // a stray DIRECTORY at a shim path would otherwise silently block
            // that shim forever (volta #1183)
            if fs::remove_file(&p).is_err() {
                let _ = fs::remove_dir_all(&p);
            }
        }
    }
    let mut count = 0u32;
    for tool in BASELINE_TOOLS {
        let link = shims.join(format!("{tool}{}", env::consts::EXE_SUFFIX));
        if platform::make_shim(&me, &link) {
            count += 1;
        }
    }
    // Installed JDKs can ship extras beyond the baseline (graalvm's gu and
    // native-image, vendor-patched tools) — shim whatever they have too.
    let mut homes: Vec<PathBuf> = list_all().into_iter().map(|(_, h)| h).collect();
    homes.sort();
    homes.dedup();
    for home in homes {
        let Ok(entries) = fs::read_dir(home.join("bin")) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Language runtimes some JDKs bundle (GraalVM ships node/python/
            // ruby): shimming them would hijack the user's other version
            // managers and then error in every non-GraalVM project (jenv #294).
            const NO_SHIM: [&str; 15] = [
                "node", "npm", "npx", "corepack", "js", "python", "python3", "pip", "pip3",
                "ruby", "gem", "irb", "graalpy", "truffleruby", "lli",
            ];
            // skip our own binary, dotfiles (.DS_Store & co), and non-executables
            if name_str.starts_with("jolta")
                || name_str.starts_with('.')
                || NO_SHIM.iter().any(|t| name_str == *t)
                || !platform::is_shimmable(&entry.path())
            {
                continue;
            }
            let link = shims.join(&name);
            if !link.exists() && platform::make_shim(&me, &link) {
                count += 1;
            }
        }
    }
    clear_cache();
    touch_stamp();
    println!("{} {count} shims in {}", ok_mark(), shims.display());
    // an installed JDK should be discoverable as one: keep /usr/libexec/
    // java_home's view in step with what jolta actually has
    sync_registry_after_change();
}

/// Bump $JOLTA_HOME/.stamp so shell hooks notice state changed and refresh
/// JAVA_HOME + their command cache at the next prompt. Called by everything
/// mutating (reshim covers install/uninstall/upgrade/setup) plus default/pin.
fn touch_stamp() {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let _ = fs::write(jolta_home().join(".stamp"), format!("{t}\n"));
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
        "fish" => home_dir().join(".config").join("fish").join("config.fish"),
        _ => home_dir().join(".profile"),
    }
}

pub fn shell_name() -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("zsh").to_string()
}

/// If `me` (canonicalized) is a Homebrew keg binary — <prefix>/Cellar/jolta/
/// <version>/bin/jolta — return brew's stable <prefix>/opt/jolta/bin/jolta
/// path, verified to resolve back to `me`. Covers /opt/homebrew, /usr/local,
/// and Linuxbrew prefixes alike, since the prefix is taken from the path.
fn brew_opt_path(me: &Path) -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    let comps: Vec<&std::ffi::OsStr> = me.iter().collect();
    let cellar = comps.iter().position(|c| c.to_str() == Some("Cellar"))?;
    if comps.get(cellar + 1).and_then(|c| c.to_str()) != Some("jolta") {
        return None;
    }
    let prefix: PathBuf = comps[..cellar].iter().collect();
    let opt = prefix.join("opt").join("jolta").join("bin").join("jolta");
    (fs::canonicalize(&opt).ok()? == *me).then_some(opt)
}

pub fn cmd_setup() {
    // The installer draws the banner before it has a binary to run, so it asks
    // us not to draw a second one directly beneath it.
    if env::var_os("JOLTA_NO_BANNER").is_some() {
        crate::ui::version_line();
    } else {
        crate::ui::banner();
    }
    let home = jolta_home();
    for sub in ["bin", "jdks", "cache", "shims"] {
        let _ = fs::create_dir_all(home.join(sub));
    }
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    let me = fs::canonicalize(&me).unwrap_or(me);
    let installed = home.join("bin").join(format!("jolta{}", env::consts::EXE_SUFFIX));
    if me != installed {
        let _ = fs::remove_file(&installed);
        if brew_opt_path(&me).is_some_and(|opt| platform::make_shim(&opt, &installed)) {
            // A COPY of a brew binary goes stale on the next `brew upgrade` —
            // and, sitting earlier on PATH, silently shadows the upgraded one.
            // Linking through brew's stable opt path (repointed on every
            // upgrade) makes upgrades flow through with no user action.
            println!(
                "{} linked to Homebrew {}",
                ok_mark(),
                dim("('brew upgrade jolta' now updates shims and all)")
            );
        } else {
            // Install a self-contained copy so the build/checkout can be deleted
            fs::copy(&me, &installed).unwrap_or_else(|e| die(&format!("cannot install to {}: {e}", installed.display())));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&installed, fs::Permissions::from_mode(0o755));
            }
            println!(
                "{} installed to {} {}",
                ok_mark(),
                home.display(),
                dim("(this checkout is no longer needed at runtime)")
            );
        }
        // reshim via the installed copy so shims point at it
        let st = Command::new(&installed).arg("reshim").status();
        if !st.is_ok_and(|s| s.success()) {
            die("reshim via installed copy failed");
        }
    } else {
        cmd_reshim();
    }

    #[cfg(windows)]
    {
        // a new terminal is only needed when the environment actually changed
        if ensure_windows_env() {
            println!("{} setup complete {}", ok_mark(), dim("(open a new terminal to activate)"));
        } else {
            println!("{} setup complete {}", ok_mark(), dim("(already active)"));
        }
        return;
    }
    #[cfg(unix)]
    {
        // a new shell is only needed when profile blocks were actually added;
        // on a re-run everything is live already (the hook syncs via the stamp)
        if ensure_profile() {
            println!("{} setup complete {}", ok_mark(), dim("(open a new shell to activate)"));
        } else {
            println!("{} setup complete {}", ok_mark(), dim("(already active)"));
        }
    }
}

/// Windows counterpart of ensure_profile(): put the shims and bin dirs on
/// the user PATH (registry, REG_EXPAND_SZ preserved, WM_SETTINGCHANGE
/// broadcast) and the JAVA_HOME hook into every PowerShell profile. Returns
/// true if anything changed. Shared by setup and `doctor --fix`.
#[cfg(windows)]
fn ensure_windows_env() -> bool {
    let mut changed = false;
    let dirs = [shims_dir(), jolta_home().join("bin")];
    match platform::add_user_path(&dirs) {
        Some(true) => {
            changed = true;
            println!("{} added the shims and bin directories to your user PATH", ok_mark());
        }
        Some(false) => println!("{} user PATH already set up", ok_mark()),
        None => {
            println!(
                "{} could not edit the user PATH — add these yourself (Settings > Environment Variables):",
                warn_mark()
            );
            println!("    {}", dirs[0].display());
            println!("    {}", dirs[1].display());
        }
    }
    const HOOK_BLOCK: &str = "\n# >>> jolta hook (keeps JAVA_HOME in sync with the pin in effect) >>>\njolta hook powershell | Out-String | Invoke-Expression\n# <<< jolta hook <<<\n";
    let profiles = platform::ps_profiles();
    if profiles.is_empty() {
        println!(
            "{} no PowerShell found — add this line to your shell profile for the JAVA_HOME hook:",
            warn_mark()
        );
        println!("    jolta hook powershell | Out-String | Invoke-Expression");
    }
    for profile in profiles {
        let existing = fs::read_to_string(&profile).unwrap_or_default();
        if existing.contains("jolta hook") {
            println!("{} JAVA_HOME hook already in {}", ok_mark(), profile.display());
            continue;
        }
        if let Some(parent) = profile.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let ok = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&profile)
            .and_then(|mut f| f.write_all(HOOK_BLOCK.as_bytes()))
            .is_ok();
        if ok {
            changed = true;
            println!("{} added JAVA_HOME hook to {}", ok_mark(), profile.display());
        } else {
            println!("{} could not write {} — add the hook line yourself", warn_mark(), profile.display());
        }
    }
    changed
}

/// Append the PATH and JAVA_HOME-hook blocks to the shell profile when they
/// are missing. Returns true if anything was added. Shared by setup and
/// `doctor --fix`.
#[cfg(unix)]
fn ensure_profile() -> bool {
    let shell = shell_name();
    let fish = shell == "fish";
    let profile = profile_file();
    // fish config lives under ~/.config/fish/, which may not exist yet
    if let Some(parent) = profile.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let existing = fs::read_to_string(&profile).unwrap_or_default();
    let mut additions = String::new();
    if existing.contains(">>> jolta >>>") {
        println!("{} PATH setup already in {}", ok_mark(), profile.display());
    } else {
        additions.push_str(if fish {
            "\n# >>> jolta >>>\nset -gx JOLTA_HOME $HOME/.jolta\nset -gx PATH $JOLTA_HOME/shims $JOLTA_HOME/bin $PATH\n# <<< jolta <<<\n"
        } else {
            "\n# >>> jolta >>>\nexport JOLTA_HOME=\"$HOME/.jolta\"\nexport PATH=\"$JOLTA_HOME/shims:$JOLTA_HOME/bin:$PATH\"\n# <<< jolta <<<\n"
        });
        println!("{} added PATH setup to {}", ok_mark(), profile.display());
    }
    if existing.contains("jolta hook") {
        println!("{} JAVA_HOME hook already in {}", ok_mark(), profile.display());
    } else {
        if fish {
            additions.push_str(
                "\n# >>> jolta hook (keeps JAVA_HOME in sync with the pin in effect) >>>\njolta hook fish | source\n# <<< jolta hook <<<\n",
            );
        } else {
            additions.push_str(&format!(
                "\n# >>> jolta hook (keeps JAVA_HOME in sync with the pin in effect) >>>\neval \"$(jolta hook {shell})\"\n# <<< jolta hook <<<\n",
            ));
        }
        println!("{} added JAVA_HOME hook to {}", ok_mark(), profile.display());
    }
    if additions.is_empty() {
        return false;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile)
        .unwrap_or_else(|e| die(&format!("cannot open {}: {e}", profile.display())));
    f.write_all(additions.as_bytes())
        .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", profile.display())));
    true
}

pub fn cmd_pin(rest: &[String]) {
    let resolved_flag = rest.iter().any(|a| a == "--resolved");
    let Some(spec) = rest.iter().find(|a| !a.starts_with('-')) else {
        die("usage: jolta pin <spec> [--resolved]  (e.g. 21 or corretto@21)");
    };
    let spec = &expand_spec(spec);
    let (_, parsed_version) = parse_spec(spec);
    if major_of(&parsed_version).is_none() {
        // an unparseable pin would break every java invocation below this dir
        die(&format!("cannot parse version '{spec}' (try e.g. 21, 21.0.2, or corretto@21)"));
    }
    if resolve(spec).is_none() {
        let (vendor, version) = parse_spec(spec);
        let vendor = vendor.unwrap_or_else(crate::jdk::default_vendor);
        if env::var("JOLTA_NO_AUTO_INSTALL").is_err()
            && INSTALLABLE_VENDORS.contains(&vendor)
            && major_of(&version).is_some()
        {
            eprintln!(
                "{} no installed JDK matches '{spec}' — fetching it now",
                paint("33", "jolta:", true)
            );
            if install_vendor_spec(vendor, &version).is_err() {
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
    // --resolved: pin the exact version of the JDK the spec lands on, so CI
    // and teammates can't drift onto a different point release. The user's
    // vendor choice (or deliberate lack of one) is preserved.
    let spec = if resolved_flag {
        match resolve(spec).as_deref().and_then(version_of) {
            Some(v) => match parse_spec(spec).0 {
                Some(vendor) => format!("{vendor}@{v}"),
                None => v,
            },
            None => {
                eprintln!(
                    "{} --resolved: no installed JDK matches '{spec}' — pinning it as-is",
                    paint("33", "jolta: warning:", true)
                );
                spec.to_string()
            }
        }
    } else {
        spec.to_string()
    };
    let spec = spec.as_str();
    fs::write(".java-version", format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write .java-version: {e}")));
    touch_stamp();
    let cwd = env::current_dir().unwrap_or_default();
    println!("{} pinned Java {} in {}/.java-version", ok_mark(), bold(spec), cwd.display());
    stale_java_home_hint(resolve(spec).as_deref());
}

pub fn cmd_default(spec: &str) {
    let spec = &expand_spec(spec);
    let (_, version) = parse_spec(spec);
    if major_of(&version).is_none() {
        // an unparseable default would break every java invocation on the box
        die(&format!("cannot parse version '{spec}' (try e.g. 21, 21.0.2, or corretto@21)"));
    }
    let resolved = resolve(spec);
    if resolved.is_none() {
        eprintln!(
            "{} no installed JDK currently matches '{spec}'",
            paint("33", "jolta: warning:", true)
        );
    }
    let _ = fs::create_dir_all(jolta_home());
    fs::write(jolta_home().join("default"), format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write default: {e}")));
    touch_stamp();
    println!("{} default Java version set to {}", ok_mark(), bold(spec));
    stale_java_home_hint(resolved.as_deref());
}

/// The shims re-resolve on every call, but JAVA_HOME consumers (maven,
/// gradle, the macOS /usr/bin/java stub) keep whatever the shell hook
/// exported last — warn when a pin change leaves that stale.
fn stale_java_home_hint(resolved: Option<&Path>) {
    if let Ok(jh) = env::var("JAVA_HOME") {
        if resolved != Some(Path::new(&jh)) {
            println!(
                "{}",
                dim("note: this shell's JAVA_HOME predates the change — the shell hook refreshes it at the next prompt")
            );
        }
    }
}

/// Minimal JSON string escaping — paths can carry quotes and backslashes
/// (Windows), and a hand-rolled emitter keeps the crate dependency-free.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list_json() {
    let pin = read_pin();
    let current = match &pin.spec {
        Some(spec) => resolve(spec),
        None => system_default(),
    };
    let managed = list_managed();
    let mut system = list_system();
    system.sort();
    system.dedup();
    let mut items: Vec<String> = Vec::new();
    for (is_managed, (v, h)) in managed
        .iter()
        .map(|x| (true, x))
        .chain(system.iter().map(|x| (false, x)))
    {
        let major = major_of(v).map_or("null".to_string(), |m| m.to_string());
        let vendor = vendor_of(h).map_or("null".to_string(), json_str);
        let active = Some(h) == current.as_ref();
        items.push(format!(
            "    {{\"version\": {}, \"major\": {major}, \"vendor\": {vendor}, \"home\": {}, \"managed\": {is_managed}, \"active\": {active}}}",
            json_str(v),
            json_str(&h.display().to_string())
        ));
    }
    let pin_json = match &pin.spec {
        Some(s) => format!("{{\"spec\": {}, \"source\": {}}}", json_str(s), json_str(&pin.source)),
        None => "null".to_string(),
    };
    let jdks = if items.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n  ]", items.join(",\n"))
    };
    println!("{{\n  \"pin\": {pin_json},\n  \"jdks\": {jdks}\n}}");
}

pub fn cmd_list(rest: &[String]) {
    if rest.iter().any(|a| a == "--json") {
        return list_json();
    }
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

pub fn cmd_jdks() {
    let mut all = list_all();
    all.sort();
    all.dedup();
    for (v, h) in all {
        if let Some(m) = major_of(&v) {
            println!("{m}\t{v}\t{}\t{}", vendor_of(&h).unwrap_or("?"), h.display());
        }
    }
}

/// `"rejected": [...]` — the JDKs the walk passed over. Only ever built when
/// --explain is present, so the default path stays a single flat object.
fn rejected_json(rejected: &[crate::resolve::Rejection]) -> String {
    let items: Vec<String> = rejected
        .iter()
        .map(|r| {
            format!(
                "{{\"version\": {}, \"vendor\": {}, \"home\": {}, \"reason\": {}}}",
                json_str(&r.version),
                r.vendor.as_deref().map_or("null".to_string(), json_str),
                json_str(&r.home.display().to_string()),
                json_str(&r.reason)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

pub fn cmd_current(rest: &[String]) {
    let json = rest.iter().any(|a| a == "--json");
    let explain = rest.iter().any(|a| a == "--explain");
    // --explain re-walks with the cache bypassed; without it, nothing changes.
    let ex = if explain {
        let ex = crate::resolve::explain_current();
        // The near miss is the whole point: when nothing satisfies an exact
        // pin, "17.0.19 is here and it is one release off" is the sentence the
        // user needs — and resolve_current would die before ever printing it.
        if ex.home.is_none() && !ex.rejected.is_empty() {
            explain_no_match(&ex, json);
        }
        Some(ex)
    } else {
        None
    };
    let r = resolve_current(false);
    if json {
        let tail = ex.as_ref().map(provenance_json).unwrap_or_default();
        println!(
            "{{\"version\": {}, \"vendor\": {}, \"home\": {}, \"source\": {}{tail}}}",
            version_of(&r.home).as_deref().map_or("null".to_string(), json_str),
            vendor_of(&r.home).map_or("null".to_string(), json_str),
            json_str(&r.home.display().to_string()),
            json_str(&r.source)
        );
        return;
    }
    let v = version_of(&r.home).unwrap_or_else(|| "unknown".into());
    let vendor = vendor_of(&r.home).map(|s| format!(" ({s})")).unwrap_or_default();
    println!("{}{} {}", bold(&v), cyan(&vendor), dim(&format!("(from {})", r.source)));
    println!("{}", r.home.display());
    if let Some(ex) = &ex {
        print_rejected(&ex.rejected);
        print_provenance(ex);
    }
}

/// Nothing matched the pin, but JDKs were considered. Report what was here and
/// why each one missed, then exit — this is the case a bare "no match" error
/// leaves the user to reconstruct by hand.
fn explain_no_match(ex: &crate::resolve::Explanation, json: bool) -> ! {
    let spec = ex.pin.spec.as_deref().unwrap_or("?");
    if json {
        println!(
            "{{\"version\": null, \"vendor\": null, \"home\": null, \"source\": {}, \"spec\": {}{}}}",
            json_str(&ex.pin.source),
            json_str(spec),
            provenance_json(ex)
        );
        std::process::exit(1);
    }
    eprintln!(
        "{} no installed JDK matches '{spec}' (pinned by {})",
        crate::ui::paint("31", "jolta:", true),
        ex.pin.source
    );
    print_rejected(&ex.rejected);
    print_provenance(ex);
    eprintln!("\n  run 'jolta install {spec}' to download it");
    std::process::exit(1);
}

/// The rejected set plus the identity of the universe it was rejected from.
/// Without the inventory digest and resolver version, the same spec can yield
/// a different explanation later and nothing marks that the ground moved.
fn provenance_json(ex: &crate::resolve::Explanation) -> String {
    format!(
        ", \"rejected\": {}, \"inventory\": {{\"count\": {}, \"digest\": {}}}, \"resolver\": {}",
        rejected_json(&ex.rejected),
        ex.inventory.count,
        json_str(&ex.inventory.digest),
        json_str(ex.resolver)
    )
}

fn print_provenance(ex: &crate::resolve::Explanation) {
    println!(
        "{}",
        dim(&format!(
            "considered {} installed JDK(s) · inventory {} · resolver {}",
            ex.inventory.count, ex.inventory.digest, ex.resolver
        ))
    );
}

/// The losing candidates in prose. Silence when nothing was passed over is
/// deliberate: "considered 0 alternatives" is noise, not evidence.
fn print_rejected(rejected: &[crate::resolve::Rejection]) {
    if rejected.is_empty() {
        return;
    }
    println!("\n{}", dim("passed over:"));
    for r in rejected {
        println!(
            "  {:<12} {:<11} {}",
            r.version,
            r.vendor.as_deref().unwrap_or("?"),
            dim(&r.reason)
        );
    }
}

pub fn cmd_which(rest: &[String]) {
    let explain = rest.iter().any(|a| a == "--explain");
    let tool = rest.iter().find(|a| !a.starts_with("--")).map_or("java", |s| s.as_str());
    let r = resolve_current(false);
    let bin = tool_bin(&r.home, tool);
    if !bin.is_file() {
        die(&format!("'{tool}' not found in {}", r.home.display()));
    }
    println!("{}", bin.display());
    if explain {
        let ex = crate::resolve::explain_current();
        println!(
            "{}",
            dim(&format!(
                "selected {} via {}",
                version_of(&r.home).unwrap_or_else(|| "unknown".into()),
                ex.pin.source
            ))
        );
        print_rejected(&ex.rejected);
        print_provenance(&ex);
    }
}

pub fn cmd_exec(argv: &[String]) -> ! {
    let r = resolve_current(true);
    let mut dirs = vec![r.home.join("bin")];
    if let Some(old) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&old));
    }
    let path = env::join_paths(dirs).unwrap_or_else(|e| die(&format!("bad PATH: {e}")));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).env("JAVA_HOME", &r.home).env("PATH", path);
    platform::exec_replace(cmd);
}

/// Single-quote a value for `eval`-safe shell output: double quotes would let
/// a hostile path ($(...), backticks) execute during eval (volta #216 class).
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn cmd_env() {
    let r = resolve_current(true);
    let home = r.home.display().to_string();
    println!("export JAVA_HOME={}", sh_quote(&home));
    println!("export PATH={}:\"$PATH\"", sh_quote(&format!("{home}/bin")));
}

pub fn cmd_home() {
    let r = resolve_current(false);
    println!("{}", r.home.display());
}

/// The pin-tracking half of the hook, shared by zsh and bash; each shell then
/// appends its own rehash builtin and registration (see src/hooks/).
const HOOK_COMMON: &str = include_str!("hooks/common.sh");

pub fn cmd_hook(shell: &str) {
    // The pre-prompt sync watches two things, both with builtins only (no
    // fork): $JOLTA_HOME/.stamp, touched by mutating jolta commands, so an
    // install/uninstall/default in one shell can't leave this one on a stale
    // JDK; and a fingerprint of the pin file in effect, so a git checkout or
    // an edit that rewrites .java-version applies without a cd out and back.
    match shell {
        "zsh" => print!("{HOOK_COMMON}{}", include_str!("hooks/zsh.sh")),
        "bash" => print!("{HOOK_COMMON}{}", include_str!("hooks/bash.sh")),
        "fish" => print!("{}", include_str!("hooks/fish.fish")),
        "powershell" | "pwsh" => print!("{}", include_str!("hooks/powershell.ps1")),
        other => die(&format!("no hook available for shell '{other}' (zsh, bash, fish, and powershell are supported)")),
    }
}

pub fn cmd_install(spec: &str) {
    let spec = &expand_spec(spec);
    let (vendor, version) = parse_spec(spec);
    let vendor = vendor.unwrap_or_else(crate::jdk::default_vendor);
    major_of(&version)
        .unwrap_or_else(|| die(&format!("cannot parse version '{spec}' (try e.g. 21, 21.0.2, or corretto@21)")));
    if install_vendor_spec(vendor, &version).is_err() {
        std::process::exit(1);
    }
    // First JDK on this machine: pin it as the global default so plain `java`
    // works right away (macOS java_home never reports managed JDKs).
    let default = jolta_home().join("default");
    if !default.exists() {
        let _ = fs::create_dir_all(jolta_home());
        fs::write(&default, format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write default: {e}")));
        println!(
            "{} set {} as your default Java version {}",
            ok_mark(),
            bold(spec),
            dim("(change with 'jolta default <version>')")
        );
    }
}

/// Set of (distro, full version) currently installed on this machine, for
/// marking listings.
fn installed_set() -> Vec<(String, String)> {
    list_all()
        .into_iter()
        .filter_map(|(v, h)| vendor_of(&h).map(|ven| (ven.to_string(), v)))
        .collect()
}

fn installed_mark(installed: &[(String, String)], vendor: &str, version: &str) -> String {
    if installed.iter().any(|(ven, v)| ven == vendor && v == version) {
        format!(" {}", green("✓ installed"))
    } else {
        String::new()
    }
}

/// jolta catalog                  -> latest per distro
/// jolta catalog 21               -> each distro's latest 21.x
/// jolta catalog temurin          -> a distro's latest per major (LTS + current)
/// jolta catalog temurin@21[.0]   -> published versions matching the filter
pub fn cmd_catalog(arg: Option<&str>) {
    let installed = installed_set();

    // Prefetch the whole dataset behind one spinner instead of stalling
    // row-by-row; the render below then reads warm cache exclusively.
    let sp = crate::ui::spinner("updating catalog");
    let universe = release_universe();
    if let Some((_, lts, _, feature)) = &universe {
        let mut majors: Vec<u32> = lts.clone();
        majors.push(*feature);
        majors.sort_by_key(|x| std::cmp::Reverse(*x));
        majors.dedup();
        let wanted: Vec<u32> = match arg {
            Some(a) if !KNOWN_VENDORS.contains(&parse_spec(a).0.unwrap_or("")) && !INSTALLABLE_VENDORS.contains(&a) => {
                major_of(a).map(|m| vec![m]).unwrap_or(majors)
            }
            _ => majors,
        };
        // fan out: every (distro, major) lookup runs its curl concurrently,
        // so a cold prefetch costs one round-trip, not thirty
        std::thread::scope(|scope| {
            for vendor in INSTALLABLE_VENDORS {
                for m in &wanted {
                    let m = *m;
                    scope.spawn(move || {
                        if latest_remote_version(vendor, m).is_none() && matches!(vendor, "oracle" | "graalvm") {
                            let _ = probe_latest(vendor, m);
                        }
                    });
                }
            }
            // filter mode additionally needs the full listing for that distro+major
            if let Some(a) = arg {
                let (v, ver) = parse_spec(a);
                if let (Some(v), false) = (v, ver.is_empty()) {
                    if let Some(m) = major_of(&ver) {
                        scope.spawn(move || {
                            let _ = vendor_versions(v, m);
                        });
                    }
                }
            }
        });
    }
    sp.finish();

    // header line: the Java release universe with LTS + newest highlighted
    if let Some((available, lts, recent_lts, recent_feature)) = &universe {
        let majors: Vec<String> = available
            .iter()
            .map(|m| {
                let s = m.to_string();
                if m == recent_feature {
                    bold(&cyan(&s))
                } else if lts.contains(m) {
                    bold(&s)
                } else {
                    dim(&s)
                }
            })
            .collect();
        println!(
            "{} {}   {}",
            bold("Java majors:"),
            majors.join(" "),
            dim(&format!("(bold = LTS, latest LTS {recent_lts}, newest {recent_feature})"))
        );
        println!();
    }

    match arg {
        // jolta available temurin  /  temurin@21
        Some(a) if KNOWN_VENDORS.contains(&parse_spec(a).0.unwrap_or("")) || INSTALLABLE_VENDORS.contains(&a) => {
            let (vendor, ver) = if INSTALLABLE_VENDORS.contains(&a) {
                (a, String::new())
            } else {
                let (v, ver) = parse_spec(a);
                (v.unwrap(), ver)
            };
            if ver.is_empty() {
                // distro overview: latest per major, LTS set + current feature
                let mut majors: Vec<u32> = universe
                    .as_ref()
                    .map(|(_, lts, _, feature)| {
                        let mut m = lts.clone();
                        m.push(*feature);
                        m
                    })
                    .unwrap_or_default();
                majors.sort_by_key(|x| std::cmp::Reverse(*x));
                majors.dedup();
                println!(
                    "{} {}",
                    bold(&cyan(vendor)),
                    dim("— latest per major (LTS + current; use @<major> for every build)")
                );
                let mut any = false;
                for major in majors {
                    match latest_remote_version(vendor, major) {
                        Some(v) => {
                            any = true;
                            let extra = if installed.iter().any(|(ven, iv)| ven == vendor && iv == &v) {
                                format!(" {}", green("✓ installed"))
                            } else {
                                let others: Vec<&str> = installed
                                    .iter()
                                    .filter(|(ven, iv)| ven == vendor && major_of(iv) == Some(major))
                                    .map(|(_, iv)| iv.as_str())
                                    .collect();
                                if others.is_empty() {
                                    String::new()
                                } else {
                                    dim(&format!(" (installed: {})", others.join(", ")))
                                }
                            };
                            println!("  {:<4} {}{extra}", bold(&major.to_string()), v);
                        }
                        None if probe_latest(vendor, major) => {
                            any = true;
                            println!("  {:<4} {}", bold(&major.to_string()), dim("published (version shown on install)"));
                        }
                        None => {}
                    }
                }
                if !any {
                    println!("  {}", dim("(nothing reachable — network problem, or distro doesn't publish these majors)"));
                }
            } else {
                // @v is a prefix filter: temurin@21 -> all 21.x, @21.0 -> 21.0.x
                let major = major_of(&ver).unwrap_or_else(|| die(&format!("cannot parse '{ver}'")));
                println!(
                    "{} {}",
                    bold(&cyan(vendor)),
                    dim(&format!("— published GA versions matching {ver}"))
                );
                if matches!(vendor, "oracle" | "graalvm") {
                    println!("{}", dim("  (no listing API; latest per major only — exact installs of any published version work)"));
                }
                let versions: Vec<String> = vendor_versions(vendor, major)
                    .into_iter()
                    .filter(|v| v == &ver || v.starts_with(&format!("{ver}.")) || v.starts_with(&format!("{ver}u")))
                    .collect();
                if versions.is_empty() {
                    println!("  {}", dim("(no published versions match)"));
                } else {
                    for (i, v) in versions.iter().enumerate() {
                        let latest_tag = if i == 0 { dim(" (latest)") } else { String::new() };
                        println!("  {v}{latest_tag}{}", installed_mark(&installed, vendor, v));
                    }
                }
            }
        }
        // jolta available 21
        Some(a) => {
            let major = major_of(a).unwrap_or_else(|| {
                die(&format!("cannot parse '{a}' (try a major like 21, or a distro like temurin)"))
            });
            println!("{} {}", bold(&format!("Latest Java {major} by distro")), dim("(✓ = installed)"));
            for vendor in INSTALLABLE_VENDORS {
                match latest_remote_version(vendor, major) {
                    Some(v) => println!("  {:<10} {}{}", cyan(vendor), bold(&v), installed_mark(&installed, vendor, &v)),
                    None if probe_latest(vendor, major) => println!(
                        "  {:<10} {}",
                        cyan(vendor),
                        dim("published (vendor hides the version until download)")
                    ),
                    None => println!("  {:<10} {}", cyan(vendor), dim("not published for this platform/major (archive may still have exact versions)")),
                }
            }
        }
        // jolta available
        None => {
            let Some((available, _, _, recent_feature)) = &universe else {
                die("cannot reach the Adoptium release index (offline?)");
            };
            let mut majors_desc = available.clone();
            majors_desc.sort_by_key(|x| std::cmp::Reverse(*x));
            println!("{} {}", bold("Latest available by distro"), dim("(✓ = installed)"));
            for vendor in INSTALLABLE_VENDORS {
                // newest major this distro actually publishes (probe downward)
                let mut hit = None;
                for major in majors_desc.iter().take(4) {
                    if let Some(v) = latest_remote_version(vendor, *major) {
                        hit = Some((*major, Some(v)));
                        break;
                    }
                    if probe_latest(vendor, *major) {
                        hit = Some((*major, None));
                        break;
                    }
                }
                match hit {
                    Some((m, v)) => {
                        let note = if m == *recent_feature { String::new() } else { dim(&format!(" (newest at {m})")) };
                        match v {
                            Some(v) => println!("  {:<10} {}{note}{}", cyan(vendor), bold(&v), installed_mark(&installed, vendor, &v)),
                            None => println!("  {:<10} {}{note}", cyan(vendor), bold(&format!("{m} (exact version shown on install)"))),
                        }
                    }
                    None => println!("  {:<10} {}", cyan(vendor), dim("unreachable")),
                }
            }
            println!("
{}", dim("jolta available <major> · jolta available <distro>[@<major>] for more"));
        }
    }
}

/// jolta-managed JDKs as (vendor, major, best installed full version),
/// one entry per distro+major (highest build wins).
fn managed_targets() -> Vec<(String, u32, String)> {
    let mut best: Vec<(String, u32, String)> = Vec::new();
    let jdks = jolta_home().join("jdks");
    let Ok(entries) = fs::read_dir(&jdks) else { return best };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((vendor, full)) = name.split_once('-') else { continue };
        if !KNOWN_VENDORS.contains(&vendor) {
            continue;
        }
        let Some(major) = major_of(full) else { continue };
        match best.iter_mut().find(|(v, m, _)| v == vendor && *m == major) {
            Some(slot) if numkey(full) > numkey(&slot.2) => slot.2 = full.to_string(),
            Some(_) => {}
            None => best.push((vendor.to_string(), major, full.to_string())),
        }
    }
    best.sort();
    best
}

/// Is a vendor's advertised version newer than the installed JAVA_VERSION?
/// Distro schemes carry extra build parts (corretto: 21.0.2.13.1 vs the
/// JDK's 21.0.2), so compare at the installed version's precision — else
/// every `jolta upgrade` re-downloads the same release forever.
fn newer_than(latest: &str, installed: &str) -> bool {
    let prec = installed
        .split(['+', '-'])
        .next()
        .unwrap_or("")
        .replace(['u', '_'], ".")
        .split('.')
        .filter(|s| !s.is_empty())
        .count()
        .clamp(1, 4);
    let key = |v: &str| {
        let v = v.split(['+', '-']).next().unwrap_or("").replace(['u', '_'], ".");
        let mut parts = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0).min(999));
        let mut k = 0u64;
        for i in 0..4 {
            let x = if i < prec { parts.next().unwrap_or(0) } else { 0 };
            k = k * 1_000 + x;
        }
        k
    };
    key(latest) > key(installed)
}

pub fn cmd_update() {
    let targets = managed_targets();
    if targets.is_empty() {
        println!(
            "no jolta-managed JDKs to check {}",
            dim("(system JDKs update through their own package managers, e.g. brew)")
        );
        return;
    }
    let mut outdated = 0;
    for (vendor, major, installed) in &targets {
        match latest_remote_version(vendor, *major) {
            Some(latest) if newer_than(&latest, installed) => {
                println!(
                    "  {} {}@{}  {} -> {}",
                    yellow("!"),
                    cyan(vendor),
                    major,
                    installed,
                    bold(&latest)
                );
                outdated += 1;
            }
            Some(_) => {
                println!("  {} {}@{}  {} {}", ok_mark(), cyan(vendor), major, installed, dim("(up to date)"));
            }
            None => {
                println!(
                    "  ? {}@{}  {} {}",
                    cyan(vendor),
                    major,
                    installed,
                    dim("(latest unknown — mirror or non-redirecting vendor; 'jolta upgrade' will check by fetching)")
                );
            }
        }
    }
    if outdated > 0 {
        println!("\n{outdated} outdated — run {} to update", bold("jolta upgrade"));
    }
}

pub fn cmd_upgrade(spec: Option<&str>) {
    crate::install::set_fresh(); // never skip a new point release on stale cache

    let managed = managed_targets();
    let targets: Vec<(String, u32, String)> = match spec {
        Some(s) => {
            let (vendor, version) = parse_spec(s);
            let vendor = vendor.unwrap_or_else(crate::jdk::default_vendor);
            let major = major_of(&version)
                .unwrap_or_else(|| die(&format!("cannot parse '{s}' (try e.g. 21 or corretto@21)")));
            match managed.into_iter().find(|(v, m, _)| v == vendor && *m == major) {
                Some(t) => vec![t],
                None => die(&format!(
                    "{vendor}@{major} is not jolta-managed — 'jolta install {s}' to add it \
                     (system JDKs upgrade through their own package managers)"
                )),
            }
        }
        None => managed,
    };
    if targets.is_empty() {
        println!(
            "nothing to upgrade {}",
            dim("(no jolta-managed JDKs; system JDKs upgrade through their own package managers)")
        );
        return;
    }
    let mut upgraded = 0;
    for (vendor, major, installed) in &targets {
        if let Some(latest) = latest_remote_version(vendor, *major) {
            if !newer_than(&latest, installed) {
                println!("  {} {}@{}  {} {}", ok_mark(), cyan(vendor), major, installed, dim("(up to date)"));
                continue;
            }
        }
        // unknown or newer: fetch latest; install reports "already installed"
        // if it lands on the same version we have
        match install_vendor_major(vendor, *major) {
            Ok(landed) => {
                if numkey(&landed) > numkey(installed) {
                    prune_superseded(vendor, *major, &landed);
                    upgraded += 1;
                }
            }
            Err(()) => eprintln!(
                "{} upgrade of {vendor}@{major} failed; kept {installed}",
                paint("33", "jolta: warning:", true)
            ),
        }
    }
    if upgraded > 0 {
        println!("{} upgraded {upgraded} JDK(s)", ok_mark());
    }
}

/// Shortest spec that uniquely picks `pick` out of `all` (dir names like
/// "temurin-25.0.3"): bare version, then vendor@major, then vendor@version.
fn minimal_spec(pick: &str, all: &[String]) -> String {
    let (vendor, full) = pick.split_once('-').unwrap_or(("", pick));
    let full_dup = all
        .iter()
        .filter(|n| n.split_once('-').is_some_and(|(_, f)| f == full))
        .count()
        > 1;
    if !full_dup {
        return full.to_string();
    }
    let major = major_of(full);
    let vm_dup = all
        .iter()
        .filter(|n| n.split_once('-').is_some_and(|(v, f)| v == vendor && major_of(f) == major))
        .count()
        > 1;
    match (vm_dup, major) {
        (false, Some(m)) => format!("{vendor}@{m}"),
        _ => format!("{vendor}@{full}"),
    }
}

pub fn cmd_uninstall(name: &str) {
    // The argument is a spec or a directory entry inside jdks/, never a path:
    // reject separators and dot-dirs so "../cache" can't escape the jdks dir.
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        die(&format!("'{name}' is not a jolta-managed JDK name (see 'jolta list')"));
    }
    let jdks = jolta_home().join("jdks");
    // A literal directory name (temurin-25.0.3) always works; otherwise
    // accept the same spec forms every other command takes (25, 25.0.3,
    // temurin@25) and match them against the managed installs.
    let mut dest = jdks.join(name);
    if !dest.is_dir() {
        let (vendor, version) = parse_spec(name);
        let major = major_of(&version);
        let mut all_names: Vec<String> = Vec::new();
        let mut matches: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&jdks) {
            for entry in entries.flatten() {
                let dir_name = entry.file_name().to_string_lossy().to_string();
                let Some((v, full)) = dir_name.split_once('-') else { continue };
                all_names.push(dir_name.clone());
                if vendor.is_some_and(|want| v != want) {
                    continue;
                }
                let hit = if is_exact(&version) {
                    full == version
                } else {
                    major.is_some() && major_of(full) == major
                };
                if hit {
                    matches.push(dir_name);
                }
            }
        }
        matches.sort();
        dest = match matches.len() {
            0 => die(&format!("'{name}' is not a jolta-managed JDK (see 'jolta list')")),
            1 => jdks.join(&matches[0]),
            _ => {
                let mut lines = String::new();
                for m in &matches {
                    // minimality is judged against everything installed, not
                    // just the matches — a suggestion must never itself be
                    // ambiguous when pasted back
                    lines.push_str(&format!("\n  jolta uninstall {}", minimal_spec(m, &all_names)));
                }
                die(&format!("'{name}' matches more than one installed JDK — be more specific:{lines}"));
            }
        };
    }
    fs::remove_dir_all(&dest).unwrap_or_else(|e| die(&format!("cannot remove {}: {e}", dest.display())));
    clear_cache();
    println!("{} removed {}", ok_mark(), dest.display());
    // Drop shims for vendor-specific extras (gu, native-image, ...) that no
    // remaining JDK provides; the baseline set always survives.
    cmd_reshim();
}

/// jolta vendor [name|--unset]: the preferred distro for vendorless specs.
/// Resolution prefers this vendor's builds (even over higher builds of other
/// vendors) and vendorless installs fetch it; explicit specs always win.
/// The per-user JVM registry `/usr/libexec/java_home` reads. The system-wide
/// /Library/Java/JavaVirtualMachines would need sudo; java_home treats both
/// alike, so jolta uses the one it can write.
///
/// Reads $HOME directly rather than via home_dir(), which dies when it is
/// unset: with an explicit $JOLTA_HOME, reshim works in HOME-less
/// environments (cron, containers) and registration must not be what breaks
/// it — same reasoning as sdkman_java_dir.
#[cfg(target_os = "macos")]
fn jvm_registry_dir() -> Option<PathBuf> {
    let home = env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(home).join("Library/Java/JavaVirtualMachines"))
}

/// A registry entry jolta owns: named "jolta-*.jdk" AND a symlink that
/// resolves inside $JOLTA_HOME/jdks. Both halves matter — the registry also
/// holds JDKs the user installed by other means, and this is the guard that
/// keeps sync from ever deleting one.
#[cfg(target_os = "macos")]
fn is_ours(entry: &Path) -> bool {
    let named_ours = entry.file_name().is_some_and(|n| n.to_string_lossy().starts_with("jolta-"));
    let is_link = fs::symlink_metadata(entry).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    // compare resolved paths: $JOLTA_HOME itself may sit behind a symlink
    let jdks =
        fs::canonicalize(jolta_home().join("jdks")).unwrap_or_else(|_| jolta_home().join("jdks"));
    let points_into_ours = fs::read_link(entry)
        .map(|t| fs::canonicalize(&t).unwrap_or(t).starts_with(&jdks))
        .unwrap_or(false);
    named_ours && is_link && points_into_ours
}

/// Mirror the jolta-managed JDKs into the JVM registry so
/// `/usr/libexec/java_home -V` lists them — an installed JDK should be
/// discoverable as one, not just through jolta. Homebrew formulae have the
/// same gap and tell you to symlink by hand; jolta does it for you.
///
/// Returns (added, removed). Runs on every reshim rather than only on install,
/// so entries whose JDK was uninstalled, upgraded, pruned or hand-deleted get
/// cleaned up too. That part is hygiene, not correctness: java_home resolves
/// through the symlink and silently skips one whose target is gone (verified —
/// it lists neither the entry nor an error). Left alone they would just
/// accumulate as junk in a directory the user reads. Only entries `is_ours`
/// accepts are ever created or destroyed.
#[cfg(target_os = "macos")]
fn sync_registry() -> (usize, usize) {
    // no HOME, or a registry we can't create: skip quietly. Registration is a
    // convenience layered on top of the install — it must never be the thing
    // that fails one.
    let Some(reg) = jvm_registry_dir() else { return (0, 0) };
    if fs::create_dir_all(&reg).is_err() {
        return (0, 0);
    }
    // java_home only understands macOS bundles, so a flat install has nothing
    // worth linking — skip it rather than create an entry it can't read.
    let mut want: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(jolta_home().join("jdks")) {
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let dir = e.path();
            if dir.is_dir() && dir.join("Contents/Home").is_dir() {
                want.push((format!("jolta-{}.jdk", e.file_name().to_string_lossy()), dir));
            }
        }
    }
    let (mut added, mut removed) = (0, 0);
    if let Ok(entries) = fs::read_dir(&reg) {
        for e in entries.flatten() {
            let keep = want.iter().any(|(name, _)| name.as_str() == e.file_name());
            if !keep && is_ours(&e.path()) && fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
    }
    for (name, target) in &want {
        let link = reg.join(name);
        if fs::read_link(&link).is_ok_and(|t| &t == target) {
            continue; // already correct
        }
        if fs::symlink_metadata(&link).is_ok() {
            if !is_ours(&link) {
                continue; // someone else's entry wearing our name: leave it
            }
            let _ = fs::remove_file(&link);
        }
        if std::os::unix::fs::symlink(target, &link).is_ok() {
            added += 1;
        }
    }
    (added, removed)
}

/// Remove every registry entry jolta owns (used by implode).
#[cfg(target_os = "macos")]
pub fn unregister_all() -> usize {
    let mut removed = 0;
    let Some(reg) = jvm_registry_dir() else { return 0 };
    if let Ok(entries) = fs::read_dir(reg) {
        for e in entries.flatten() {
            if is_ours(&e.path()) && fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// Called from cmd_reshim, the common tail of install/uninstall/upgrade/
/// prune/setup — so every path that changes the JDK set keeps java_home
/// honest. Deliberately not opt-out-able: "an installed JDK is visible to the
/// system" is only useful as an invariant, and the entries are user-level
/// symlinks jolta owns, removable at any time.
#[cfg(target_os = "macos")]
pub fn sync_registry_after_change() {
    let (added, removed) = sync_registry();
    if added + removed > 0 {
        println!("{} java_home sees {added} new, {removed} removed", ok_mark());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn sync_registry_after_change() {}

#[cfg(not(target_os = "macos"))]
pub fn unregister_all() -> usize {
    0
}

pub fn cmd_vendor(rest: &[String]) {
    let f = jolta_home().join("vendor");
    match rest.first().map(String::as_str) {
        None => match crate::jdk::preferred_vendor() {
            Some(v) => println!("{}", v),
            None => println!("{}", dim("(no preferred vendor set — vendorless specs pick the highest build, install defaults to temurin)")),
        },
        Some("--unset") => {
            let _ = fs::remove_file(&f);
            touch_stamp();
            println!("{} preferred vendor cleared", ok_mark());
        }
        Some(name) => {
            let name = name.to_ascii_lowercase();
            if !KNOWN_VENDORS.contains(&name.as_str()) {
                die(&format!("unknown vendor '{name}' (known: {})", KNOWN_VENDORS.join(", ")));
            }
            let _ = fs::create_dir_all(jolta_home());
            fs::write(&f, format!("{name}\n")).unwrap_or_else(|e| die(&format!("cannot write {}: {e}", f.display())));
            clear_cache();
            touch_stamp();
            if !INSTALLABLE_VENDORS.contains(&name.as_str()) {
                eprintln!(
                    "{} '{name}' is matched but not downloadable — vendorless installs will still fetch temurin",
                    paint("33", "jolta: note:", true)
                );
            }
            println!("{} preferred vendor set to {}", ok_mark(), bold(&name));
        }
    }
}

/// jolta prune [spec] [--dry-run]: two tiers, both respecting remembered
/// pins (re-read live from the projects' own files):
///   1. within each vendor+major, keep the newest build; drop older ones
///      unless a pin exact-references them
///   2. drop entire majors that are non-LTS, not the current feature
///      release, and not the vendor's newest — unless some pin references
///      that major
/// A spec argument (17, temurin@17) scopes both tiers.
pub fn cmd_prune(rest: &[String]) {
    let dry = rest.iter().any(|a| a == "--dry-run" || a == "-n");
    let scope = rest.iter().find(|a| !a.starts_with('-')).map(|s| {
        let (v, ver) = parse_spec(s);
        match major_of(&ver) {
            Some(m) => (v, m),
            None => die(&format!("cannot parse '{s}' (try e.g. 17 or temurin@17)")),
        }
    });
    let pins = crate::resolve::remembered_pin_specs();
    // group managed builds by (vendor, major) from install-dir names
    let jdks_dir = jolta_home().join("jdks");
    let mut groups: Vec<((String, u32), Vec<(String, String)>)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&jdks_dir) {
        let mut names: Vec<String> =
            entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for name in names {
            let Some((vendor, full)) = name.split_once('-') else { continue };
            if !KNOWN_VENDORS.contains(&vendor) {
                continue;
            }
            let Some(major) = major_of(full) else { continue };
            let key = (vendor.to_string(), major);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push((full.to_string(), name.clone())),
                None => groups.push((key, vec![(full.to_string(), name.clone())])),
            }
        }
    }
    if groups.is_empty() {
        println!("nothing to prune {}", dim("(no jolta-managed JDKs)"));
        return;
    }
    // LTS/current knowledge: vendor metadata (or mirror), else the known train
    let (lts_set, feature) = match release_universe() {
        Some((_, lts, _, feature)) => (lts, Some(feature)),
        None => (vec![8, 11, 17, 21, 25], None),
    };
    // a vendor's newest installed major always survives tier 2
    let mut newest_major: Vec<(String, u32)> = Vec::new();
    for ((v, m), _) in &groups {
        match newest_major.iter_mut().find(|(nv, _)| nv == v) {
            Some((_, nm)) => *nm = (*nm).max(*m),
            None => newest_major.push((v.clone(), *m)),
        }
    }
    fn prune_one(name: &str, why: &str, dry: bool, removed: &mut u32) {
        if dry {
            println!("  would prune {name} {}", dim(&format!("({why})")));
        } else if fs::remove_dir_all(jolta_home().join("jdks").join(name)).is_ok() {
            println!("  {} pruned {name} {}", ok_mark(), dim(&format!("({why})")));
        }
        *removed += 1;
    }
    let mut removed = 0u32;
    for ((vendor, major), mut builds) in groups {
        if scope.as_ref().is_some_and(|(sv, sm)| *sm != major || sv.is_some_and(|s| s != vendor)) {
            continue;
        }
        builds.sort_by_key(|(full, _)| std::cmp::Reverse(numkey(full)));
        let is_lts = lts_set.contains(&major);
        let is_current = feature == Some(major)
            || newest_major.iter().any(|(v, m)| *v == vendor && *m == major);
        let major_pin = pins.iter().find(|(spec, _)| {
            let (pv, ver) = parse_spec(spec);
            major_of(&ver) == Some(major) && pv.map_or(true, |p| p == vendor)
        });
        if !is_lts && !is_current {
            match major_pin {
                Some((_, src)) => println!(
                    "  {} kept {vendor} {major} {}",
                    ok_mark(),
                    dim(&format!("(non-LTS, but pinned by {src})"))
                ),
                None => {
                    for (_, name) in &builds {
                        prune_one(name, &format!("non-LTS {major}, higher major installed"), dry, &mut removed);
                    }
                    continue;
                }
            }
        }
        for (full, name) in &builds[1..] {
            if let Some((_, src)) = pins.iter().find(|(spec, _)| crate::resolve::pin_protects(spec, &vendor, full)) {
                println!("  {} kept {name} {}", ok_mark(), dim(&format!("(pinned by {src})")));
            } else {
                prune_one(name, "superseded", dry, &mut removed);
            }
        }
    }
    if removed == 0 {
        println!("{} nothing to prune", ok_mark());
    } else if dry {
        println!("{removed} to prune — run without --dry-run to remove");
    } else {
        clear_cache();
        cmd_reshim();
        println!("{} pruned {removed} JDK build(s)", ok_mark());
    }
}

pub fn cmd_implode(args: &[String]) {
    let home = jolta_home();
    let profile = profile_file();
    if args.first().map(String::as_str) != Some("--yes") {
        println!(
            "This removes {} and the jolta lines from {}.",
            home.display(),
            profile.display()
        );
        let managed = list_managed();
        if managed.is_empty() {
            println!("No jolta-installed JDKs to delete.");
        } else {
            println!("The following jolta-installed JDKs will be deleted:");
            for (v, h) in &managed {
                println!("  - {} ({})", v, h.display());
            }
        }
        println!("JDKs installed outside jolta (Homebrew, /Library/Java, SDKMAN, ...) are not touched.");
        print!("Type \"yes\" to continue: ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().lock().read_line(&mut answer);
        if answer.trim() != "yes" {
            die("aborted");
        }
    }
    #[cfg(windows)]
    {
        let _ = &profile;
        for p in platform::ps_profiles() {
            strip_jolta_blocks(&p);
        }
        match platform::remove_user_path(&[shims_dir(), home.join("bin")]) {
            Some(true) => println!("{} removed jolta from your user PATH", ok_mark()),
            Some(false) => {}
            None => println!(
                "{} could not edit the user PATH — remove the jolta entries yourself",
                warn_mark()
            ),
        }
    }
    #[cfg(unix)]
    strip_jolta_blocks(&profile);
    // java_home ignores dangling entries, so this is tidiness rather than
    // repair: uninstalling jolta shouldn't leave jolta-named junk behind in a
    // directory the user (and other JDK tools) look at
    let unregistered = unregister_all();
    if unregistered > 0 {
        println!("{} removed {unregistered} java_home registry entr(ies)", ok_mark());
    }
    let _ = fs::remove_dir_all(&home);
    println!("{} removed {} — open a new shell to finish. So long!", ok_mark(), home.display());
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const TOOLCHAINS_MARKER: &str = "generated by jolta";

/// jolta toolchains [--write]: Maven toolchains.xml from installed JDKs.
/// Maven's toolchain resolution (and Gradle's auto-provisioning) otherwise
/// discovers or downloads JDKs on its own, silently bypassing jolta — this
/// hands both build tools the jolta-managed set instead. One <toolchain> per
/// distro+major, newest build wins.
pub fn cmd_toolchains(rest: &[String]) {
    let write = rest.iter().any(|a| a == "--write");
    let mut all = list_all();
    all.sort();
    all.dedup();
    // newest build per (vendor, major) — two toolchains claiming the same
    // provides would make Maven's pick order-dependent
    let mut best: Vec<(&'static str, u32, String, PathBuf)> = Vec::new();
    for (v, h) in &all {
        let Some(major) = major_of(v) else { continue };
        let Some(vendor) = vendor_of(h) else { continue };
        match best.iter_mut().find(|(bv, bm, _, _)| *bv == vendor && *bm == major) {
            Some(slot) if numkey(v) > numkey(&slot.2) => {
                slot.2 = v.clone();
                slot.3 = h.clone();
            }
            Some(_) => {}
            None => best.push((vendor, major, v.clone(), h.clone())),
        }
    }
    if best.is_empty() {
        die("no JDKs installed — nothing to write (see 'jolta install')");
    }
    best.sort_by(|a, b| (a.0, std::cmp::Reverse(a.1)).cmp(&(b.0, std::cmp::Reverse(b.1))));
    let mut xml = format!(
        // no "--" in the comment: XML forbids it inside comments
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {TOOLCHAINS_MARKER}; regenerate after installing or removing JDKs -->\n<toolchains>\n"
    );
    for (vendor, major, _, home) in &best {
        xml.push_str(&format!(
            "  <toolchain>\n    <type>jdk</type>\n    <provides>\n      <version>{major}</version>\n      <vendor>{vendor}</vendor>\n    </provides>\n    <configuration>\n      <jdkHome>{}</jdkHome>\n    </configuration>\n  </toolchain>\n",
            xml_escape(&home.display().to_string())
        ));
    }
    xml.push_str("</toolchains>\n");

    let gradle_paths = best
        .iter()
        .map(|(_, _, _, h)| h.display().to_string())
        .collect::<Vec<_>>()
        .join(",");

    if write {
        let dest = home_dir().join(".m2").join("toolchains.xml");
        if let Ok(existing) = fs::read_to_string(&dest) {
            if !existing.contains(TOOLCHAINS_MARKER) {
                die(&format!(
                    "{} exists and was not generated by jolta — refusing to overwrite\n  \
                     merge by hand: 'jolta toolchains' prints the entries to stdout",
                    dest.display()
                ));
            }
        }
        let _ = fs::create_dir_all(dest.parent().unwrap());
        fs::write(&dest, &xml).unwrap_or_else(|e| die(&format!("cannot write {}: {e}", dest.display())));
        println!("{} wrote {} ({} JDKs)", ok_mark(), dest.display(), best.len());
    } else {
        print!("{xml}");
    }
    // Gradle reads no toolchains.xml; point its detection at the same JDKs.
    // auto-download=false stops resolver plugins (foojay) fetching their own
    // JDK when nothing matches — jolta stays the sole source.
    eprintln!(
        "{}\n{}\n{}",
        paint("2", "gradle: add these to ~/.gradle/gradle.properties to expose the same JDKs (and only these):", true),
        paint("2", &format!("  org.gradle.java.installations.paths={gradle_paths}"), true),
        paint("2", "  org.gradle.java.installations.auto-download=false", true)
    );
}

/// Remove jolta's marked blocks (PATH + hook) from a profile file. Used by
/// implode for the shell profile and, on Windows, the PowerShell profiles.
fn strip_jolta_blocks(profile: &Path) {
    let Ok(text) = fs::read_to_string(profile) else { return };
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
    if out != text && fs::write(profile, out).is_ok() {
        println!("{} removed jolta lines from {}", ok_mark(), profile.display());
    }
}

/// CPU architecture of a compiled binary from its ELF/Mach-O header (8-20
/// bytes read, no exec). None for scripts and unknown formats — no verdict.
fn binary_arch(path: &Path) -> Option<&'static str> {
    let mut f = fs::File::open(path).ok()?;
    let mut b = [0u8; 20];
    f.read_exact(&mut b).ok()?;
    match b[..4] {
        [0x7f, b'E', b'L', b'F', ..] => match u16::from_le_bytes([b[18], b[19]]) {
            0x3e => Some("x86_64"),
            0xb7 => Some("aarch64"),
            _ => None,
        },
        [0xcf, 0xfa, 0xed, 0xfe, ..] => match u32::from_le_bytes([b[4], b[5], b[6], b[7]]) {
            0x0100_0007 => Some("x86_64"),
            0x0100_000c => Some("aarch64"),
            _ => None,
        },
        // fat/universal Mach-O runs on either arch
        [0xca, 0xfe, 0xba, 0xbe, ..] | [0xbe, 0xba, 0xfe, 0xca, ..] => Some("universal"),
        _ => None,
    }
}

pub fn cmd_doctor(fix: bool) -> i32 {
    let mut rc = 0;
    // --fix repairs what is safe to repair mechanically: broken shims
    // (reshim) and missing profile blocks (the same appends setup does).
    // Everything else — dangling brew links, stale JAVA_HOME, mavenrc
    // overrides — needs the user, and only gets diagnosed.
    let mut needs_reshim = false;
    let mut needs_profile = false;
    let home = jolta_home();
    println!("{}", bold("jolta doctor"));
    println!("  jolta home:    {}", home.display());
    println!(
        "  binary:        {}",
        env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
    );

    // Homebrew installs link bin/jolta through brew's opt path; a brew
    // uninstall (without `jolta implode`) leaves the link dangling and every
    // shim dead. Copies (curl/source installs) don't hit this and stay silent.
    let installed = home.join("bin").join(format!("jolta{}", env::consts::EXE_SUFFIX));
    if installed.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        match fs::canonicalize(&installed) {
            Ok(target) if target.is_file() => {
                println!("  install link:  {} ok (bin/jolta -> {})", ok_mark(), target.display());
            }
            _ => {
                println!("  install link:  {} DANGLING — bin/jolta points at a binary that is gone", bad_mark());
                println!("                 (Homebrew uninstall or cleanup?) — reinstall jolta, then run \"jolta setup\"");
                rc = 1;
            }
        }
    }

    let shim_count = fs::read_dir(shims_dir())
        .map(|e| e.flatten().filter(|x| x.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())).count())
        .unwrap_or(0);
    if shim_count > 0 {
        // Symlinks can exist yet be dead: dangling after a moved binary, or a
        // self-referential loop — either silently hands java to the OS stub.
        let java_shim = shims_dir().join(format!("java{}", env::consts::EXE_SUFFIX));
        match fs::canonicalize(&java_shim) {
            Ok(target) if target.is_file() => {
                println!("  shims:         {} ok ({shim_count} installed, java -> {})", ok_mark(), target.display());
            }
            _ => {
                println!(
                    "  shims:         {} BROKEN — the java shim is dangling or a symlink loop; run \"jolta setup\"",
                    bad_mark()
                );
                rc = 1;
                needs_reshim = true;
            }
        }
    } else {
        println!("  shims:         {} MISSING — run \"jolta setup\"", bad_mark());
        rc = 1;
        needs_reshim = true;
    }

    let on_path = env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| d == shims_dir()))
        .unwrap_or(false);
    if on_path {
        println!("  PATH:          {} ok (shims dir is on PATH)", ok_mark());
    } else {
        println!("  PATH:          {} shims dir NOT on PATH — run \"jolta setup\" and open a new shell", bad_mark());
        rc = 1;
        needs_profile = true;
    }

    match which("java") {
        Some(p) if p.starts_with(shims_dir()) => {
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
                expected.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolvable>".into())
            );
            println!("                 mvn/gradle use JAVA_HOME directly and will bypass jolta;");
            println!("                 remove any manual \"export JAVA_HOME\" from your shell profile");
            println!("                 and make sure the jolta hook line comes after it (\"jolta setup\" adds it)");
            rc = 1;
        }
        Err(_) => {
            println!("  JAVA_HOME:     {} not set — shims still work, but mvn/gradle prefer JAVA_HOME;", warn_mark());
            println!("                 run \"jolta setup\" to install the shell hook that keeps it in sync");
            needs_profile = true;
        }
    }

    // Wrong-architecture JDKs "exist" but can't exec (jenv #396): peek at the
    // resolved java's binary header. Scripts/fat binaries give no verdict.
    if let Some(exp) = &expected {
        if let Some(arch) = binary_arch(&tool_bin(exp, "java")) {
            let host = env::consts::ARCH;
            if arch != "universal" && arch != host {
                println!(
                    "  binary arch:   {} resolved java is {arch}, but this machine is {host}",
                    warn_mark()
                );
                println!("                 it may fail to exec (or run under emulation); install a native build");
            }
        }
    }

    // Maven reads ~/.mavenrc before anything else; a JAVA_HOME set there
    // silently bypasses jolta for every mvn run (jenv #232/#78).
    for rc_file in [home_dir().join(".mavenrc"), PathBuf::from("/etc/mavenrc")] {
        if let Ok(text) = fs::read_to_string(&rc_file) {
            if text.contains("JAVA_HOME") {
                println!(
                    "  mavenrc:       {} {} sets JAVA_HOME — mvn will ignore jolta's pin",
                    warn_mark(),
                    rc_file.display()
                );
            }
        }
    }

    if let Ok(base) = env::var("JOLTA_DOWNLOAD_BASE") {
        match crate::install::release_universe() {
            Some((_, _, lts, _)) => {
                println!("  mirror:        {} {base} (metadata found, LTS {lts})", ok_mark());
            }
            None => {
                println!("  mirror:        {} {base} has no metadata — update/catalog run blind", warn_mark());
                println!("                 'jolta mirror sync' writes latest/index.txt/lts files");
            }
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
    if let Some(v) = crate::jdk::preferred_vendor() {
        println!("  vendor:        {v} preferred (vendorless specs pick it first)");
    }

    if fix {
        let mut fixed = false;
        // profile/PATH edits need a fresh shell to land; a reshim is live at once
        let mut env_fixed = false;
        if needs_reshim {
            println!();
            cmd_reshim();
            fixed = true;
        }
        #[cfg(unix)]
        if needs_profile {
            println!();
            env_fixed = ensure_profile();
            fixed |= env_fixed;
        }
        #[cfg(windows)]
        if needs_profile {
            println!();
            env_fixed = ensure_windows_env();
            fixed |= env_fixed;
        }
        if env_fixed {
            println!(
                "\n{} fixes applied — open a new shell, then re-run {} to confirm",
                ok_mark(),
                bold("jolta doctor")
            );
        } else if fixed {
            println!("\n{} fixes applied — re-run {} to confirm", ok_mark(), bold("jolta doctor"));
        } else if rc != 0 {
            println!("\n{} nothing here that --fix can repair automatically", warn_mark());
        }
    }
    rc
}
