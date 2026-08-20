# Windows smoke test for jolta. Uses an isolated JOLTA_HOME; never touches the
# real one or any profile. Requires >= 2 JDK majors (GitHub runners export
# JAVA_HOME_<major>_X64, which jolta's discovery reads).
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$bin = Join-Path $repo "target\release\jolta.exe"
if (-not (Test-Path $bin)) { Write-Host "build first: cargo build --release"; exit 1 }

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("jolta-test-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path "$work\home\shims", "$work\home\jdks" | Out-Null
$env:JOLTA_HOME = "$work\home"
$env:JOLTA_NO_AUTO_INSTALL = "1"
Remove-Item Env:JAVA_HOME -ErrorAction SilentlyContinue

& $bin reshim | Out-Null
$env:PATH = "$work\home\shims;" + (Split-Path $bin) + ";" + $env:PATH

$majors = & $bin jdks | ForEach-Object { [int]($_ -split "`t")[0] } | Sort-Object -Unique
if ($majors.Count -lt 2) { Write-Host "SKIP: need at least two JDK majors (found: $majors)"; exit 0 }
$m1 = $majors[0]; $m2 = $majors[-1]
Write-Host "testing with majors $m1 and $m2 (available: $majors)"

$script:pass = 0; $script:fail = 0
function Check($name, $pattern, $actual) {
  if ("$actual" -match $pattern) { $script:pass++; Write-Host "ok   $name" }
  else { $script:fail++; Write-Host "FAIL $name`n     expected match: $pattern`n     got: $actual" }
}
function VerPattern($m) { "version ""$m\.|version ""1\.$m\.|version ""$m""" }

# 1. Pinned resolution through the java shim
New-Item -ItemType Directory -Path "$work\p1" | Out-Null
Set-Location "$work\p1"
Set-Content -Path .java-version -Value $m1
Check "pin $m1 -> java -version" (VerPattern $m1) ((java -version 2>&1) -join " ")

# 2. A different pin resolves to a different JDK
New-Item -ItemType Directory -Path "$work\p2" | Out-Null
Set-Location "$work\p2"
Set-Content -Path .java-version -Value $m2
Check "pin $m2 -> java -version" (VerPattern $m2) ((java -version 2>&1) -join " ")

# 3. Walk-up: subdirectory inherits the pin
New-Item -ItemType Directory -Path "$work\p2\sub\deeper" -Force | Out-Null
Set-Location "$work\p2\sub\deeper"
Check "walk-up from subdir" (VerPattern $m2) ((java -version 2>&1) -join " ")

# 4. The project pin is authoritative: a stray JOLTA_JAVA_VERSION is inert
$env:JOLTA_JAVA_VERSION = "$m1"
Check "stray JOLTA_JAVA_VERSION is ignored" (VerPattern $m2) ((java -version 2>&1) -join " ")
Remove-Item Env:JOLTA_JAVA_VERSION

# 5. jolta home matches jolta which
Set-Location "$work\p2"
$whichJava = & $bin which java
$homeDir = & $bin home
Check "jolta home matches which" ([regex]::Escape($homeDir)) $whichJava

# 6. jolta exec sets JAVA_HOME
$jh = & $bin exec cmd /c "echo %JAVA_HOME%"
Check "exec sets JAVA_HOME" ([regex]::Escape($homeDir)) $jh

# 7. Unmatchable pin fails with a helpful error
New-Item -ItemType Directory -Path "$work\p3" | Out-Null
Set-Location "$work\p3"
Set-Content -Path .java-version -Value "99"
$out = (java -version 2>&1) -join " "
Check "unmatched pin errors" "no installed JDK matches" $out
if ($LASTEXITCODE -ne 0) { $script:pass++; Write-Host "ok   unmatched pin exits non-zero" }
else { $script:fail++; Write-Host "FAIL unmatched pin exits non-zero" }

# 8. jolta pin writes the file
New-Item -ItemType Directory -Path "$work\p4" | Out-Null
Set-Location "$work\p4"
& $bin pin "$m1" 2>&1 | Out-Null
Check ".java-version written" "^$m1$" (Get-Content .java-version -Raw).Trim()

# 9. javac shim works too
Set-Location "$work\p2"
Check "javac shim" "javac $m2\.|javac 1\.$m2\.|javac $m2" ((javac -version 2>&1) -join " ")

# 9b. doctor must call a healthy install healthy. Windows shims are hard links
#     or copies whenever symlink_file is refused (Developer Mode is off by
#     default), and PATH comes verbatim out of the registry while shims_dir()
#     builds a fresh path — doctor once counted only symlinks and compared
#     paths byte-wise, so it reported MISSING / NOT on PATH / BYPASSING on an
#     install that worked perfectly.
Set-Location "$work\p2"
$doc = (& $bin doctor 2>&1) -join "`n"
Check "doctor sees the shims"      "shims:\s+\S+ ok"  $doc
Check "doctor sees shims on PATH"  "PATH:\s+\S+ ok"   $doc
Check "doctor sees java resolving" "java:\s+\S+ ok"   $doc

# 9c. The same install, spelled differently. Upper-casing JOLTA_HOME and the
#     PATH entry changes nothing on a case-insensitive filesystem, so every
#     verdict above must survive it unchanged.
$savedHome = $env:JOLTA_HOME
$savedPath = $env:PATH
# replace the shims entry with an upper-cased, trailing-slash spelling rather
# than prepending one, or the untouched original would satisfy the check
$env:PATH = $savedPath -replace [regex]::Escape("$savedHome\shims"), "$($savedHome.ToUpper())\SHIMS\"
$env:JOLTA_HOME = $savedHome.ToUpper()
$docCase = (& $bin doctor 2>&1) -join "`n"
Check "doctor survives a case-mangled JOLTA_HOME" "shims:\s+\S+ ok" $docCase
Check "doctor survives a case-mangled PATH entry" "PATH:\s+\S+ ok"  $docCase
$env:JOLTA_HOME = $savedHome
$env:PATH = $savedPath

# 9d. A non-symlink shim — the unprivileged Windows fallback, hard link then
#     copy — must still be counted. Hard links cannot cross volumes and the
#     runner checks out on a different drive from TEMP, so link against a copy
#     inside JOLTA_HOME and fall back to a plain copy if even that is refused.
$javaShim = Join-Path $env:JOLTA_HOME "shims\java.exe"
$localBin = Join-Path $env:JOLTA_HOME "bin\jolta.exe"
New-Item -ItemType Directory -Force -Path (Split-Path $localBin) | Out-Null
Copy-Item $bin $localBin -Force
Remove-Item $javaShim -Force
try   { New-Item -ItemType HardLink -Path $javaShim -Target $localBin -ErrorAction Stop | Out-Null }
catch { Copy-Item $localBin $javaShim -Force }
$docHard = (& $bin doctor 2>&1) -join "`n"
Check "doctor counts a non-symlink shim" "shims:\s+\S+ ok" $docHard
& $bin reshim | Out-Null

# 10. setup installs, and a RE-RUN from the installed copy must survive it
#     (the verbatim-path compare once made setup delete its own binary).
#     CI only: setup edits the user PATH registry key and PS profiles, which
#     this script promises never to touch on a real machine.
if ($env:CI -eq "true") {
  & $bin setup *> $null
  $inst = Join-Path $env:JOLTA_HOME "bin\jolta.exe"
  if (Test-Path $inst) { $script:pass++; Write-Host "ok   setup installs the binary" }
  else { $script:fail++; Write-Host "FAIL setup installs the binary" }
  $rerun = (& $inst setup 2>&1) -join " "
  if ($LASTEXITCODE -eq 0 -and (Test-Path $inst)) { $script:pass++; Write-Host "ok   setup re-run from installed copy" }
  else { $script:fail++; Write-Host "FAIL setup re-run from installed copy`n     got: $rerun" }
}

Set-Location $repo
Remove-Item -Recurse -Force $work
Write-Host ""
Write-Host "passed: $script:pass, failed: $script:fail"
if ($script:fail -gt 0) { exit 1 }
