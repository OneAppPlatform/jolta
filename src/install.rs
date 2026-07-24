//! Fetching and installing JDK builds from vendor download endpoints.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::commands::cmd_reshim;
use crate::download::download;
use crate::jdk::{jdk_version, managed_home, INSTALLABLE_VENDORS};
use crate::paths::jolta_home;
use crate::resolve::resolve;
use crate::ui::{bold, cyan, die, dim, ok_mark, paint};

/// Download URL for the latest GA build of a distro + major on this platform.
fn vendor_url(vendor: &str, major: u32) -> String {
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

pub fn install_vendor_major(vendor: &str, major: u32) -> Result<(), ()> {
    if !matches!(env::consts::OS, "macos" | "linux" | "windows") {
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

    let tarball = tmp.join("jdk.archive");
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
    Ok(())
}
