# jolta-core.sh — shared resolution logic for the jolta CLI and its shims.
# POSIX sh. Sourced, never executed.

jolta_home() {
  printf '%s\n' "${JOLTA_HOME:-$HOME/.jolta}"
}

jolta_die() {
  printf 'jolta: %s\n' "$*" >&2
  exit 1
}

# Resolve a path through symlinks to its real location.
jolta_realpath() {
  _p=$1
  _n=0
  while [ -L "$_p" ] && [ $_n -lt 32 ]; do
    _t=$(readlink "$_p")
    case $_t in
      /*) _p=$_t ;;
      *)  _p=$(dirname "$_p")/$_t ;;
    esac
    _n=$((_n + 1))
  done
  ( cd "$(dirname "$_p")" 2>/dev/null && printf '%s/%s\n' "$(pwd -P)" "$(basename "$_p")" ) || printf '%s\n' "$_p"
}

# Major version of a spec: "21"->21, "21.0.4"->21, "1.8"->8, "8u392"->8,
# "temurin-21.0.4"->21.
jolta_major_of() {
  _v=$1
  _v=${_v##*[!0-9.+_u-]-}          # strip vendor prefix like "temurin-"
  case $_v in
    1.*) _v=${_v#1.}; printf '%s\n' "${_v%%[!0-9]*}" ;;
    *)   printf '%s\n' "${_v%%[!0-9]*}" ;;
  esac
}

# Walk up from $1 looking for a .java-version file; print its path or nothing.
jolta_find_version_file() {
  _d=$1
  while :; do
    if [ -f "$_d/.java-version" ]; then
      printf '%s\n' "$_d/.java-version"
      return 0
    fi
    [ "$_d" = "/" ] && return 1
    _d=$(dirname "$_d")
  done
}

# Determine the requested version for the current directory.
# Sets: JOLTA_SPEC (version string or empty), JOLTA_SPEC_SOURCE (description).
jolta_read_pin() {
  JOLTA_SPEC=""
  JOLTA_SPEC_SOURCE=""
  if [ -n "${JOLTA_JAVA_VERSION:-}" ]; then
    JOLTA_SPEC=$JOLTA_JAVA_VERSION
    JOLTA_SPEC_SOURCE="JOLTA_JAVA_VERSION environment variable"
    return 0
  fi
  _f=$(jolta_find_version_file "$PWD") || _f=""
  if [ -n "$_f" ]; then
    JOLTA_SPEC=$(head -n1 "$_f" | tr -d ' \t\r')
    JOLTA_SPEC_SOURCE="$_f"
    [ -n "$JOLTA_SPEC" ] && return 0
  fi
  _d="$(jolta_home)/default"
  if [ -f "$_d" ]; then
    JOLTA_SPEC=$(head -n1 "$_d" | tr -d ' \t\r')
    JOLTA_SPEC_SOURCE="jolta default ($_d)"
    [ -n "$JOLTA_SPEC" ] && return 0
  fi
  return 0   # no pin anywhere: caller falls back to system default
}

# Full JAVA_VERSION of a JDK home, from its release file.
jolta_jdk_version() {
  [ -f "$1/release" ] || return 1
  sed -n 's/^JAVA_VERSION="\(.*\)"/\1/p' "$1/release" | head -n1
}

# The usable home inside a jolta-managed install dir (handles macOS bundles).
jolta_managed_home() {
  if [ -d "$1/Contents/Home" ]; then
    printf '%s\n' "$1/Contents/Home"
  else
    printf '%s\n' "$1"
  fi
}

# List jolta-managed JDKs as "fullversion<TAB>home" lines.
jolta_list_managed() {
  _jdks="$(jolta_home)/jdks"
  [ -d "$_jdks" ] || return 0
  for _d in "$_jdks"/*/; do
    [ -d "$_d" ] || continue
    _h=$(jolta_managed_home "${_d%/}")
    _v=$(jolta_jdk_version "$_h") || continue
    printf '%s\t%s\n' "$_v" "$_h"
  done
}

# List system JDKs as "fullversion<TAB>home" lines (macOS + common Linux dirs).
jolta_list_system() {
  if [ -x /usr/libexec/java_home ]; then
    # -V prints "    21.0.11 (arm64) "Homebrew" - "OpenJDK 21.0.11" /path" on stderr
    /usr/libexec/java_home -V 2>&1 >/dev/null | awk '
      / \(.*\) / && $NF ~ /^\// { print $1 "\t" $NF }'
  fi
  for _base in /usr/lib/jvm "$HOME/.sdkman/candidates/java"; do
    [ -d "$_base" ] || continue
    for _d in "$_base"/*/; do
      [ -d "$_d" ] || continue
      _h=${_d%/}
      [ "$(basename "$_h")" = "current" ] && continue
      _v=$(jolta_jdk_version "$_h") || continue
      printf '%s\t%s\n' "$_v" "$_h"
    done
  done
}

# From "fullversion<TAB>home" lines on stdin, print the home of the best match
# for major version $1: exact full match for $2 wins, else highest of that major.
jolta_best_match() {
  _major=$1
  _spec=$2
  awk -F'\t' -v major="$_major" -v spec="$_spec" '
    function majorof(v,  a, n) {
      n = split(v, a, /[._+u-]/)
      if (a[1] == "1" && n > 1) return a[2] + 0
      return a[1] + 0
    }
    function numkey(v,  a, n) {
      sub(/[+_-].*$/, "", v)
      n = split(v, a, ".")
      return a[1] * 1000000 + a[2] * 1000 + a[3]
    }
    majorof($1) == major {
      if ($1 == spec) { exact = $2 }
      k = numkey($1)
      if (k >= bestk) { bestk = k; best = $2 }
    }
    END {
      if (exact != "") print exact
      else if (best != "") print best
    }'
}

# Resolve a version spec to a JDK home. Prints the home, or fails.
# Order: jolta-managed JDKs, then system JDKs (java_home on macOS).
jolta_resolve() {
  _spec=$1
  _major=$(jolta_major_of "$_spec")
  [ -n "$_major" ] || return 1

  _cache="$(jolta_home)/cache/v-$(printf '%s' "$_spec" | tr '/ ' '__')"
  if [ -f "$_cache" ]; then
    _h=$(cat "$_cache")
    if [ -x "$_h/bin/java" ]; then
      printf '%s\n' "$_h"
      return 0
    fi
    rm -f "$_cache"
  fi

  # NOTE: java_home -v is not usable as a fallback — it exits 0 and prints its
  # default JDK even when nothing matches the requested version.
  _h=$( { jolta_list_managed; jolta_list_system; } | jolta_best_match "$_major" "$_spec")
  [ -n "$_h" ] && [ -x "$_h/bin/java" ] || return 1

  mkdir -p "$(jolta_home)/cache"
  printf '%s\n' "$_h" > "$_cache"
  printf '%s\n' "$_h"
}

# System default JDK home, used when nothing is pinned and no jolta default set.
jolta_system_default() {
  if [ -x /usr/libexec/java_home ]; then
    /usr/libexec/java_home 2>/dev/null
  elif [ -x /usr/bin/java ]; then
    printf '%s\n' "/usr"
  fi
}

# Download the pinned JDK on demand, Volta-style. Only runs when the caller
# opted in (JOLTA_AUTO_INSTALL=1: shims, exec, env — never the cd hook) and the
# user hasn't opted out (JOLTA_NO_AUTO_INSTALL). $JOLTA_CLI is set by callers.
jolta_auto_install() {
  [ "${JOLTA_AUTO_INSTALL:-}" = "1" ] || return 1
  [ -z "${JOLTA_NO_AUTO_INSTALL:-}" ] || return 1
  [ -n "${JOLTA_CLI:-}" ] && [ -x "$JOLTA_CLI" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  _m=$(jolta_major_of "$1")
  case $_m in ''|*[!0-9]*) return 1 ;; esac
  printf 'jolta: Java %s is pinned here but not installed — fetching Temurin %s (set JOLTA_NO_AUTO_INSTALL=1 to disable)\n' "$1" "$_m" >&2
  "$JOLTA_CLI" install "$_m" >&2
}

# Full resolution for the current directory.
# Sets: JOLTA_RESOLVED_HOME, JOLTA_SPEC, JOLTA_SPEC_SOURCE. Fails with message.
jolta_resolve_current() {
  jolta_read_pin
  if [ -n "$JOLTA_SPEC" ]; then
    if ! JOLTA_RESOLVED_HOME=$(jolta_resolve "$JOLTA_SPEC"); then
      jolta_auto_install "$JOLTA_SPEC" && JOLTA_RESOLVED_HOME=$(jolta_resolve "$JOLTA_SPEC") || :
    fi
    [ -n "${JOLTA_RESOLVED_HOME:-}" ] || jolta_die \
"no installed JDK matches '$JOLTA_SPEC' (pinned by $JOLTA_SPEC_SOURCE)
  installed JDKs: $( { jolta_list_managed; jolta_list_system; } | cut -f1 | sort -u | tr '\n' ' ')
  run 'jolta install $JOLTA_SPEC' to download it (Temurin), or 'jolta list' to see what's available"
  else
    JOLTA_RESOLVED_HOME=$(jolta_system_default)
    JOLTA_SPEC_SOURCE="system default (no .java-version found, no jolta default set)"
    [ -n "$JOLTA_RESOLVED_HOME" ] || jolta_die \
"no Java version pinned and no system JDK found
  pin one with 'jolta pin <version>' or set a global default with 'jolta default <version>'"
  fi
}
