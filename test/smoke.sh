#!/bin/sh
# Smoke test for jolta. Uses an isolated JOLTA_HOME; never touches ~/.jolta
# or your shell profile. Requires at least two system JDKs with different majors.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/jolta-test.XXXXXX")
trap 'rm -rf "$work"' EXIT INT TERM

export JOLTA_HOME="$work/home"
# Keep the default run offline and deterministic; JOLTA_TEST_NETWORK=1 enables
# the auto-install download test at the end.
export JOLTA_NO_AUTO_INSTALL=1
unset JAVA_HOME JOLTA_JAVA_VERSION 2>/dev/null || true

pass=0; fail=0
check() {  # check <description> <expected-substring> <actual>
  if printf '%s' "$3" | grep -q "$2"; then
    pass=$((pass + 1)); printf 'ok   %s\n' "$1"
  else
    fail=$((fail + 1)); printf 'FAIL %s\n     expected match: %s\n     got: %s\n' "$1" "$2" "$3"
  fi
}

# Set up shims in the isolated home (skip profile edits by writing shims directly)
mkdir -p "$JOLTA_HOME/shims" "$JOLTA_HOME/jdks"
"$repo/bin/jolta" reshim >/dev/null
export PATH="$JOLTA_HOME/shims:$repo/bin:$PATH"

# Pick two distinct majors that exist on this machine
majors=$(. "$repo/libexec/jolta-core.sh"; { jolta_list_managed; jolta_list_system; } \
  | cut -f1 | while read -r v; do jolta_major_of "$v"; done | sort -un)
m1=$(printf '%s\n' "$majors" | head -n1)
m2=$(printf '%s\n' "$majors" | tail -n1)
if [ -z "$m1" ] || [ "$m1" = "$m2" ]; then
  echo "SKIP: need at least two JDK majors installed (found: $majors)" >&2
  exit 0
fi
echo "testing with majors $m1 and $m2 (available: $(printf '%s' "$majors" | tr '\n' ' '))"

# 1. Pinned resolution
mkdir -p "$work/p1" && cd "$work/p1"
echo "$m1" > .java-version
check "pin $m1 -> java -version" "\"$m1\.\|\"1\.$m1\.\|version \"$m1\"" "$(java -version 2>&1)"

# 2. A different pin resolves to a different JDK
mkdir -p "$work/p2" && cd "$work/p2"
echo "$m2" > .java-version
check "pin $m2 -> java -version" "\"$m2\.\|\"1\.$m2\.\|version \"$m2\"" "$(java -version 2>&1)"

# 3. Walk-up: subdirectory inherits the pin
mkdir -p "$work/p2/sub/deeper" && cd "$work/p2/sub/deeper"
check "walk-up from subdir" "\"$m2\.\|\"1\.$m2\.\|version \"$m2\"" "$(java -version 2>&1)"

# 4. Env var override beats the file pin
check "JOLTA_JAVA_VERSION override" "\"$m1\.\|\"1\.$m1\.\|version \"$m1\"" \
  "$(JOLTA_JAVA_VERSION=$m1 java -version 2>&1)"

# 5. jolta default used when nothing pinned
cd "$work"
jolta default "$m1" >/dev/null
check "jolta default fallback" "\"$m1\.\|\"1\.$m1\.\|version \"$m1\"" "$(java -version 2>&1)"

# 6. jolta which points into the right home
cd "$work/p2"
check "jolta which" "bin/java" "$(jolta which java)"

# 7. jolta exec sets JAVA_HOME
check "jolta exec sets JAVA_HOME" "." "$(jolta exec sh -c 'echo $JAVA_HOME')"
jh=$(jolta exec sh -c 'echo "$JAVA_HOME"')
check "exec JAVA_HOME matches pin" "$(cd "$work/p2" && jolta which java | sed 's|/bin/java$||')" "$jh"

# 8. javac shim works too
check "javac shim" "javac $m2\.\|javac 1\.$m2\.\|javac $m2" "$(javac -version 2>&1)"

# 9. Unmatchable pin produces a helpful error
mkdir -p "$work/p3" && cd "$work/p3"
echo "99" > .java-version
out=$(java -version 2>&1) && rc=0 || rc=$?
check "unmatched pin errors" "no installed JDK matches" "$out"
[ "$rc" -ne 0 ] && { pass=$((pass+1)); echo "ok   unmatched pin exits non-zero"; } \
  || { fail=$((fail+1)); echo "FAIL unmatched pin exits non-zero"; }

# 10. jolta pin writes the file
mkdir -p "$work/p4" && cd "$work/p4"
jolta pin "$m1" >/dev/null 2>&1
check ".java-version written" "^$m1\$" "$(cat .java-version)"

# 11. jolta home prints the resolved JAVA_HOME
cd "$work/p2"
check "jolta home matches which" "^$(jolta which java | sed 's|/bin/java$||')\$" "$(jolta home)"

# 12. zsh hook keeps JAVA_HOME in sync across cd (skip if zsh unavailable)
if command -v zsh >/dev/null 2>&1; then
  hook_out=$(zsh -f -c '
    export PATH='"$JOLTA_HOME/shims:$repo/bin"':$PATH
    export JOLTA_HOME='"$JOLTA_HOME"'
    eval "$(jolta hook zsh)"
    cd '"$work/p1"'   && echo "p1:$JAVA_HOME"
    cd '"$work/p2"'   && echo "p2:$JAVA_HOME"
    cd '"$work/p3"'   && echo "p3:${JAVA_HOME:-unset}"
  ')
  check "zsh hook: pin $m1 dir" "p1:$(cd "$work/p1" && jolta home)" "$hook_out"
  check "zsh hook: pin $m2 dir" "p2:$(cd "$work/p2" && jolta home)" "$hook_out"
  check "zsh hook: unmatched pin unsets" "p3:unset" "$hook_out"
else
  echo "skip zsh hook tests (zsh not found)"
fi

# 13. bash hook does the same via PROMPT_COMMAND
if command -v bash >/dev/null 2>&1; then
  hook_out=$(bash --noprofile --norc -c '
    export PATH='"$JOLTA_HOME/shims:$repo/bin"':$PATH
    export JOLTA_HOME='"$JOLTA_HOME"'
    eval "$(jolta hook bash)"
    cd '"$work/p1"' && eval "$PROMPT_COMMAND" && echo "p1:$JAVA_HOME"
    cd '"$work/p3"' && eval "$PROMPT_COMMAND" && echo "p3:${JAVA_HOME:-unset}"
  ')
  check "bash hook: pin $m1 dir" "p1:$(cd "$work/p1" && jolta home)" "$hook_out"
  check "bash hook: unmatched pin unsets" "p3:unset" "$hook_out"
else
  echo "skip bash hook tests (bash not found)"
fi

# 14. auto-install: pinning an uninstalled major downloads it (network, opt-in)
if [ "${JOLTA_TEST_NETWORK:-}" = "1" ]; then
  want=25
  if printf '%s\n' "$majors" | grep -qx "$want"; then
    echo "skip auto-install test (Java $want already installed system-wide)"
  else
    mkdir -p "$work/p5" && cd "$work/p5"
    echo "$want" > .java-version
    unset JOLTA_NO_AUTO_INSTALL
    check "auto-install on first use" "version \"$want\." "$(java -version 2>&1)"
    export JOLTA_NO_AUTO_INSTALL=1
  fi
else
  echo "skip auto-install download test (set JOLTA_TEST_NETWORK=1 to enable)"
fi

echo
echo "passed: $pass, failed: $fail"
[ "$fail" -eq 0 ]
