# jolta shell hook — see src/hooks/common.sh for what the two checks cover:
# $JOLTA_HOME/.stamp (jolta run elsewhere) and a fingerprint of the pin file
# (a checkout or an edit rewriting it under a shell that never moved).
function global:__jolta_update_java_home {
  $jh = & jolta home 2>$null
  if ($LASTEXITCODE -eq 0 -and $jh) { $env:JAVA_HOME = "$jh" }
  else { Remove-Item Env:JAVA_HOME -ErrorAction SilentlyContinue }
}

function global:__jolta_pin_id {
  $dir = $PWD.Path
  while ($dir) {
    foreach ($n in '.java-version', '.sdkmanrc') {
      $f = Join-Path $dir $n
      if (Test-Path -LiteralPath $f -PathType Leaf) {
        return "$f=" + (Get-Content -LiteralPath $f -Raw -ErrorAction SilentlyContinue)
      }
    }
    $parent = Split-Path -Parent $dir
    if (-not $parent -or $parent -eq $dir) { break }
    $dir = $parent
  }
  return ''
}

function global:__jolta_sync {
  $root = if ($env:JOLTA_HOME) { $env:JOLTA_HOME } else { Join-Path $HOME '.jolta' }
  $stampFile = Join-Path $root '.stamp'
  $stamp = if (Test-Path -LiteralPath $stampFile -PathType Leaf) {
    Get-Content -LiteralPath $stampFile -Raw -ErrorAction SilentlyContinue
  } else { '' }
  $pin = __jolta_pin_id
  if ($stamp -eq $global:__jolta_stamp -and $pin -eq $global:__jolta_pin -and $global:__jolta_pin_ok) {
    return
  }
  $global:__jolta_stamp = $stamp
  $global:__jolta_pin = $pin
  __jolta_update_java_home
  # unresolvable pin: keep checking so an install elsewhere lands without a cd
  $global:__jolta_pin_ok = ($env:JAVA_HOME -or [string]::IsNullOrEmpty($pin))
}

if (-not $global:__jolta_prev_prompt) { $global:__jolta_prev_prompt = $function:prompt }
function global:prompt { __jolta_sync; & $global:__jolta_prev_prompt }
__jolta_sync
