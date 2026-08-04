# jolta shell hook — keeps JAVA_HOME in sync with the pin that applies here.
#
# Two things change JAVA_HOME under a shell that never moves:
#   1. jolta itself, run from another shell (install, uninstall, default,
#      vendor) — signalled by $JOLTA_HOME/.stamp;
#   2. the pin file, rewritten underneath you — a git checkout of a branch with
#      a different .java-version, a rebase, an editor. Nothing signals that, so
#      the hook fingerprints the pin in effect and compares it every prompt.
# Both checks are shell builtins (one small file read); jolta itself only runs
# when the stamp or the fingerprint actually changed, so an idle prompt forks
# nothing.

_jolta_update_java_home() {
  local _jh
  if _jh=$(jolta home 2>/dev/null); then
    export JAVA_HOME=$_jh
  else
    unset JAVA_HOME
  fi
  return 0
}

# Slurp a small file into $_jolta_text without forking a subshell.
_jolta_read() {
  _jolta_text=
  _jolta_line=
  while IFS= read -r _jolta_line || [ -n "$_jolta_line" ]; do
    _jolta_text="$_jolta_text$_jolta_line;"
  done < "$1"
  _jolta_line=
  return 0
}

# Fingerprint of the pin in effect: same string => same JAVA_HOME. Mirrors the
# lookup order in `jolta home` — nearest .java-version, else .sdkmanrc, walking
# up to /. The jolta default and vendor preference are not read here; changing
# either goes through jolta, which bumps the stamp.
_jolta_pin_id() {
  _jolta_id=
  _jolta_dir=$PWD
  while :; do
    for _jolta_f in "$_jolta_dir/.java-version" "$_jolta_dir/.sdkmanrc"; do
      if [ -r "$_jolta_f" ]; then
        _jolta_read "$_jolta_f"
        _jolta_id="$_jolta_f=$_jolta_text"
        return 0
      fi
    done
    [ -n "$_jolta_dir" ] || break
    _jolta_dir=${_jolta_dir%/*}
  done
  return 0
}

_jolta_sync() {
  _jolta_stamp=
  _jolta_f="${JOLTA_HOME:-${HOME-}/.jolta}/.stamp"
  if [ -r "$_jolta_f" ]; then
    _jolta_read "$_jolta_f"
    _jolta_stamp=$_jolta_text
  fi
  _jolta_pin_id
  if [ "${_JOLTA_STAMP-}" = "$_jolta_stamp" ] &&
     [ "${_JOLTA_PIN_ID-}" = "$_jolta_id" ] &&
     [ "${_JOLTA_PIN_OK-}" = 1 ]; then
    return 0
  fi
  if [ "${_JOLTA_STAMP-}" != "$_jolta_stamp" ]; then
    _JOLTA_STAMP=$_jolta_stamp
    _jolta_rehash   # JDKs (and shims) may have come or gone
  fi
  _JOLTA_PIN_ID=$_jolta_id
  _jolta_update_java_home
  if [ -n "${JAVA_HOME-}" ] || [ -z "$_jolta_id" ]; then
    _JOLTA_PIN_OK=1
  else
    # Pinned here but unresolvable — usually not installed yet. Keep checking
    # so an install from another shell lands without needing a cd.
    _JOLTA_PIN_OK=0
  fi
  return 0
}
