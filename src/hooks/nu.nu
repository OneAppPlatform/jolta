# jolta shell hook — nu dialect. See src/hooks/common.sh for what the two
# checks cover: $JOLTA_HOME/.stamp (touched by every mutating jolta command,
# so an install in another shell can't leave this one on a stale JDK) and a
# fingerprint of the pin file in effect (so a git checkout that rewrites
# .java-version applies without a cd out and back).
#
# Nu reads files with builtins, so the gate forks nothing; `jolta home` runs
# only when the stamp or the fingerprint actually changed. State lives in
# __jolta_-prefixed env vars because nu has no other durable scope a hook can
# reach, which is also how zoxide and friends do it.

def _jolta_stamp [] {
  let home = ($env.JOLTA_HOME? | default ($env.HOME | path join ".jolta"))
  let f = ($home | path join ".stamp")
  if ($f | path exists) { open --raw $f | str trim } else { "" }
}

# The pin that applies to $PWD: first .java-version or .sdkmanrc found walking
# up. Returns "<path>=<contents>" so an edit in place is caught too, not just
# a move to a different directory.
def _jolta_pin_id [] {
  mut dir = $env.PWD
  loop {
    for f in [".java-version" ".sdkmanrc"] {
      let p = ($dir | path join $f)
      if ($p | path exists) { return $"($p)=(open --raw $p)" }
    }
    let up = ($dir | path dirname)
    if $up == $dir { break }
    $dir = $up
  }
  ""
}

def --env _jolta_update_java_home [] {
  let r = (do --ignore-errors { ^jolta home | complete })
  let jh = if $r == null { "" } else if $r.exit_code == 0 { $r.stdout | str trim } else { "" }
  if ($jh | is-empty) {
    # a pin that can't be satisfied unsets JAVA_HOME, so builds fail loudly
    # rather than on whatever JDK happened to be there before
    hide-env --ignore-errors JAVA_HOME
  } else {
    $env.JAVA_HOME = $jh
  }
}

def --env _jolta_sync [] {
  let stamp = (_jolta_stamp)
  let pin = (_jolta_pin_id)
  if $stamp == ($env.__jolta_stamp? | default "") and $pin == ($env.__jolta_pin? | default "") and ($env.__jolta_ok? | default "0") == "1" {
    return
  }
  $env.__jolta_stamp = $stamp
  $env.__jolta_pin = $pin
  _jolta_update_java_home
  $env.__jolta_ok = (if ($env.JAVA_HOME? | is-not-empty) or ($pin | is-empty) { "1" } else { "0" })
}

# env_change.PWD catches a cd; pre_prompt catches everything else — a checkout
# that swaps the pin, an install from another shell. Both go through the gate.
$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {|| _jolta_sync })
$env.config.hooks.env_change = ($env.config.hooks.env_change? | default {} | upsert PWD (
  ($env.config.hooks.env_change?.PWD? | default []) | append {|| _jolta_sync }
))
_jolta_sync
