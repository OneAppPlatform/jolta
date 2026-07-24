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
Remove-Item Env:JOLTA_JAVA_VERSION -ErrorAction SilentlyContinue

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

# 4. Env var override beats the file pin
$env:JOLTA_JAVA_VERSION = "$m1"
Check "JOLTA_JAVA_VERSION override" (VerPattern $m1) ((java -version 2>&1) -join " ")
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

Set-Location $repo
Remove-Item -Recurse -Force $work
Write-Host ""
Write-Host "passed: $script:pass, failed: $script:fail"
if ($script:fail -gt 0) { exit 1 }
