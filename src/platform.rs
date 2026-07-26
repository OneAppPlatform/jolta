//! OS-specific glue: signal setup, process replacement, shim link creation.

use std::path::Path;
use std::process::Command;

use crate::ui::die;

/// Rust ignores SIGPIPE by default, turning `jolta list | head` into a
/// "failed printing to stdout" panic. Restore normal Unix behavior. No-op on
/// Windows, which has no SIGPIPE.
#[cfg(unix)]
pub fn init() {
    unsafe {
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        const SIGPIPE: i32 = 13;
        const SIG_DFL: usize = 0;
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(windows)]
pub fn init() {}

/// Replace this process with `cmd` (Unix), or run it and exit with its code
/// (Windows, which has no exec).
#[cfg(unix)]
pub fn exec_replace(mut cmd: Command) -> ! {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    die(&format!("failed to exec: {err}"));
}

#[cfg(windows)]
pub fn exec_replace(mut cmd: Command) -> ! {
    match cmd.status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(e) => die(&format!("failed to run command: {e}")),
    }
}

/// Create a shim entry at `link` invoking the jolta binary `me`.
#[cfg(unix)]
pub fn make_shim(me: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(me, link).is_ok()
}

/// Plain symlinks require Developer Mode on Windows; hard links work
/// unprivileged on the same volume, and a copy always works.
#[cfg(windows)]
pub fn make_shim(me: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(me, link).is_ok()
        || std::fs::hard_link(me, link).is_ok()
        || std::fs::copy(me, link).is_ok()
}

/// Should this JDK bin/ entry get a shim? Unix: any executable; Windows: .exe.
#[cfg(unix)]
pub fn is_shimmable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
pub fn is_shimmable(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// Broadcast WM_SETTINGCHANGE("Environment") so Explorer (and terminals it
/// spawns) pick up a registry PATH edit without a logoff. reg.exe writes but
/// never broadcasts, which is why the whole edit runs through PowerShell.
#[cfg(windows)]
const PS_BROADCAST: &str = r#"
$sig='[DllImport("user32.dll",SetLastError=true,CharSet=CharSet.Auto)]public static extern IntPtr SendMessageTimeout(IntPtr hWnd,uint Msg,UIntPtr wParam,string lParam,uint fuFlags,uint uTimeout,out UIntPtr lpdwResult);'
$t=Add-Type -MemberDefinition $sig -Name 'NM' -Namespace 'JoltaW32' -PassThru
[UIntPtr]$out=[UIntPtr]::Zero
$null=$t::SendMessageTimeout([IntPtr]0xFFFF,0x1A,[UIntPtr]::Zero,'Environment',2,5000,[ref]$out)
"#;

/// Run a PATH-editing PowerShell snippet with the target dirs in
/// $env:JOLTA_PATH_DIRS (no quoting games). Snippets exit 10 when they
/// changed the registry, 0 when there was nothing to do.
#[cfg(windows)]
fn run_path_ps(dirs: &[std::path::PathBuf], script: &str) -> Option<bool> {
    let joined = std::env::join_paths(dirs.iter().cloned()).ok()?;
    let st = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .env("JOLTA_PATH_DIRS", joined)
        .status()
        .ok()?;
    match st.code() {
        Some(10) => Some(true),
        Some(0) => Some(false),
        _ => None,
    }
}

/// Prepend `dirs` to the user PATH (HKCU\Environment) if missing. The value
/// is read and written unexpanded as REG_EXPAND_SZ, so existing %VAR% entries
/// survive — the classic footgun of [Environment]::SetEnvironmentVariable.
/// Some(true) = PATH edited, Some(false) = already present, None = failed.
#[cfg(windows)]
pub fn add_user_path(dirs: &[std::path::PathBuf]) -> Option<bool> {
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$dirs=@($env:JOLTA_PATH_DIRS -split ';' | Where-Object {{ $_ -ne '' }})
$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true)
$old=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$parts=@($old -split ';' | Where-Object {{ $_ -ne '' }})
$add=@($dirs | Where-Object {{ $parts -notcontains $_ }})
if($add.Count -gt 0){{
  $k.SetValue('Path',(($add+$parts) -join ';'),[Microsoft.Win32.RegistryValueKind]::ExpandString)
  $k.Close()
  {PS_BROADCAST}
  exit 10
}}
$k.Close()
exit 0"#
    );
    run_path_ps(dirs, &script)
}

/// Remove `dirs` from the user PATH (for `jolta implode`). Same REG_EXPAND_SZ
/// care as add_user_path.
#[cfg(windows)]
pub fn remove_user_path(dirs: &[std::path::PathBuf]) -> Option<bool> {
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$dirs=@($env:JOLTA_PATH_DIRS -split ';' | Where-Object {{ $_ -ne '' }})
$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true)
$old=[string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$parts=@($old -split ';' | Where-Object {{ $_ -ne '' }})
$keep=@($parts | Where-Object {{ $dirs -notcontains $_ }})
if($keep.Count -ne $parts.Count){{
  $k.SetValue('Path',($keep -join ';'),[Microsoft.Win32.RegistryValueKind]::ExpandString)
  $k.Close()
  {PS_BROADCAST}
  exit 10
}}
$k.Close()
exit 0"#
    );
    run_path_ps(dirs, &script)
}

/// PowerShell profile paths (CurrentUserAllHosts) for every installed engine —
/// Windows PowerShell and PowerShell 7 keep separate profile.ps1 files.
#[cfg(windows)]
pub fn ps_profiles() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for engine in ["powershell", "pwsh"] {
        let Ok(o) = Command::new(engine)
            .args(["-NoProfile", "-NonInteractive", "-Command", "$PROFILE.CurrentUserAllHosts"])
            .output()
        else {
            continue;
        };
        if !o.status.success() {
            continue;
        }
        let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if p.is_empty() {
            continue;
        }
        let pb = std::path::PathBuf::from(p);
        if !out.contains(&pb) {
            out.push(pb);
        }
    }
    out
}
