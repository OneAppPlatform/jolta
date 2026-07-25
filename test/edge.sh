#!/bin/sh
# Edge-case suite for jolta — fully offline and hermetic. Builds fake JDKs
# inside an isolated JOLTA_HOME (majors 79-97, far from anything real), so it
# needs no system JDKs and never touches the network or ~/.jolta.
#
# Test cases are transposed from the suites of the tools jolta replaces:
#   - volta      tests/acceptance (shim fidelity, exit codes, autodownload,
#                corrupted downloads, no-residue-on-failure invariants)
#   - jenv       test/*.bats + libexec logic (version-file parsing, walk-up,
#                JDK registration layouts, macOS bundles)
#   - sdkman-cli src/test features/specs (.sdkmanrc parsing, install
#                lifecycle idempotence, offline behaviors)
#
# Usage: test/edge.sh   (JOLTA_BIN overrides the binary under test)
set -u

repo=$(cd "$(dirname "$0")/.." && pwd -P)
JOLTA_BIN=${JOLTA_BIN:-$repo/target/release/jolta}
[ -x "$JOLTA_BIN" ] || { echo "build first: cargo build --release (missing $JOLTA_BIN)" >&2; exit 1; }
bindir=$(cd "$(dirname "$JOLTA_BIN")" && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/jolta-edge.XXXXXX")
work=$(cd "$work" && pwd -P)   # physical path: error messages print getcwd()
trap 'rm -rf "$work"' EXIT INT TERM

export JOLTA_HOME="$work/home"
export JOLTA_NO_AUTO_INSTALL=1
unset JAVA_HOME JOLTA_JAVA_VERSION JOLTA_DOWNLOAD_BASE 2>/dev/null || true

pass=0; fail=0; failed_names=''

contains() { case "$2" in *"$1"*) return 0 ;; *) return 1 ;; esac; }

ok()   { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad()  { fail=$((fail + 1)); failed_names="$failed_names
  - $1"; printf 'FAIL %s\n%s\n' "$1" "$2"; }

# check <desc> <expected-substring> <actual>
check() {
  if contains "$2" "$3"; then ok "$1"
  else bad "$1" "     expected substring: $2
     got: $3"; fi
}
# check_not <desc> <forbidden-substring> <actual>
check_not() {
  if contains "$2" "$3"; then bad "$1" "     expected NO substring: $2
     got: $3"
  else ok "$1"; fi
}
# check_eq <desc> <expected> <actual>
check_eq() {
  if [ "$2" = "$3" ]; then ok "$1"
  else bad "$1" "     expected exactly: $2
     got: $3"; fi
}
# check_rc <desc> <0|nonzero> <actual-rc>
check_rc() {
  if [ "$2" = 0 ] && [ "$3" -eq 0 ]; then ok "$1"
  elif [ "$2" != 0 ] && [ "$3" -ne 0 ]; then ok "$1"
  else bad "$1" "     expected rc $2, got rc $3"; fi
}
section() { printf '\n== %s ==\n' "$1"; }

# mk_jdk <dirname> <version> <implementor> [tools...]
#   Creates a fake JDK under $JOLTA_HOME/jdks/<dirname> (or at <dirname> when
#   it is an absolute path) with a release file and fake tools.
#   -b as first arg = macOS-style Contents/Home bundle layout.
#   Fake tools echo "fake-<tool> <version> dir=<home> JAVA_HOME=..", print each
#   arg on its own line as "arg=[..]", and honor:
#     --fake-exit N     exit with code N
#     --fake-cat        echo stdin (passthrough test)
#     --fake-streams    "OUT" on stdout, "ERR" on stderr
#     --fake-pwd        print the working directory
#     --fake-sig        die of SIGSEGV
mk_jdk() {
  bundle=''
  [ "$1" = "-b" ] && { bundle=1; shift; }
  dirname=$1 version=$2 implementor=$3; shift 3
  case "$dirname" in
    /*) root="$dirname" ;;
    *)  root="$JOLTA_HOME/jdks/$dirname" ;;
  esac
  home="$root"; [ -n "$bundle" ] && home="$root/Contents/Home"
  mkdir -p "$home/bin"
  {
    printf 'JAVA_VERSION="%s"\n' "$version"
    [ "$implementor" = GRAAL ] && printf 'GRAALVM_VERSION="%s"\n' "$version"
    [ -n "$implementor" ] && [ "$implementor" != GRAAL ] \
      && printf 'IMPLEMENTOR="%s"\n' "$implementor"
  } > "$home/release"
  [ $# -eq 0 ] && set -- java javac
  for tool in "$@"; do
    cat > "$home/bin/$tool" <<EOF
#!/bin/sh
case "\${1:-}" in
  --fake-exit) exit "\$2" ;;
  --fake-cat) exec cat ;;
  --fake-streams) echo OUT; echo ERR >&2; exit 0 ;;
  --fake-pwd) pwd; exit 0 ;;
  --fake-sig) kill -s SEGV \$\$ ;;
esac
echo "fake-$tool $version dir=\$(cd "\$(dirname "\$0")/.." && pwd -P) JAVA_HOME=\${JAVA_HOME:-unset}"
for a in "\$@"; do printf 'arg=[%s]\n' "\$a"; done
exit 0
EOF
    chmod +x "$home/bin/$tool"
  done
}

# ---------------------------------------------------------------- fixtures --
mkdir -p "$JOLTA_HOME/jdks"
mk_jdk temurin-97.0.1        97.0.1    "Eclipse Adoptium"
mk_jdk corretto-96.0.1       96.0.1    "Amazon.com Inc." java javac megatool
mk_jdk temurin-1.95.0_10     1.95.0_10 "Eclipse Adoptium"
mk_jdk graalvm-94.0.2        94.0.2    GRAAL
mk_jdk mystery-93.5.0        93.5.0    "Amazon.com Inc."
mk_jdk -b zulu-92.0.1        92.0.1    "Azul Systems, Inc."
mk_jdk temurin-87.0.7        87.0.7    "Eclipse Adoptium"
mk_jdk corretto-87.0.7       87.0.7    "Amazon.com Inc."
# hostile fixtures: must be ignored without crashing anything
mkdir -p "$JOLTA_HOME/jdks/empty-dir"
mk_jdk temurin-badver        abc       "Eclipse Adoptium"

"$JOLTA_BIN" reshim >/dev/null
export PATH="$JOLTA_HOME/shims:$bindir:$PATH"

jolta_rc() { out=$("$@" 2>&1); rc=$?; }   # capture combined output + exit code

# =================================================================
section "A. spec & .java-version parsing (jenv-inspired)"
# =================================================================

mkdir -p "$work/a" && cd "$work/a"

printf '97' > .java-version   # no trailing newline
check "no trailing newline" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '97 \r\n' > .java-version
check "trailing space + CRLF" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '97\n96\n' > .java-version
check "multi-line file: first line wins" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '\n97\n' > .java-version
check "leading blank line then version" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '\357\273\277%s\n' 97 > .java-version   # UTF-8 BOM (Windows editors)
check "UTF-8 BOM before version" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '1.95\n' > .java-version
check "legacy 1.x pin (1.95)" "1.95.0_10" "$(jolta home 2>/dev/null)"
printf '95\n' > .java-version
check "modern major matches legacy JDK (95)" "1.95.0_10" "$(jolta home 2>/dev/null)"

printf 'TEMURIN@97\n' > .java-version
check "uppercase vendor pin" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'temurin-97\n' > .java-version
check "vendor-dash spec" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
printf 'temurin@97\n' > .java-version
check "vendor-at spec" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'foo@97\n' > .java-version
jolta_rc jolta home
check_rc "unknown vendor spec fails" nonzero "$rc"
check "unknown vendor error names the spec" "foo@97" "$out"

printf 'temurin\n' > .java-version
jolta_rc jolta home
check_rc "vendor-only spec fails" nonzero "$rc"

printf '97.0.9\n' > .java-version
jolta_rc jolta home
check_rc "exact pin refuses other builds of the major" nonzero "$rc"

printf 'temurin@97.0.1\n' > .java-version
check "vendored exact pin resolves" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'corretto-97\n' > .java-version
jolta_rc jolta home
check_rc "wrong-vendor pin is strict" nonzero "$rc"

printf 'v97\n' > .java-version
jolta_rc jolta home
check_rc "garbage spec fails" nonzero "$rc"

printf '96\n' > .java-version
check "JOLTA_JAVA_VERSION beats file pin" "temurin-97.0.1" \
  "$(JOLTA_JAVA_VERSION=97 jolta home 2>/dev/null)"
check "empty JOLTA_JAVA_VERSION is ignored" "corretto-96.0.1" \
  "$(JOLTA_JAVA_VERSION= jolta home 2>/dev/null)"
check "whitespace-padded JOLTA_JAVA_VERSION works" "temurin-97.0.1" \
  "$(JOLTA_JAVA_VERSION=' 97 ' jolta home 2>/dev/null)"
jolta_rc env JOLTA_JAVA_VERSION=bogus jolta home
check_rc "invalid env override fails, no silent fallback to pin" nonzero "$rc"
check "invalid env override error names the env var" "JOLTA_JAVA_VERSION" "$out"

# =================================================================
section "B. discovery & precedence (jenv/volta-inspired)"
# =================================================================

mkdir -p "$work/b/1/2/3/4/5/6/7/8/9" && cd "$work/b"
echo 97 > .java-version
cd "$work/b/1/2/3/4/5/6/7/8/9"
check "walk-up ten levels deep" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

: > "$work/b/1/.java-version"   # empty file part-way up
check "empty .java-version continues the walk" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

rm "$work/b/1/.java-version"
mkdir "$work/b/1/2/.java-version"   # a DIRECTORY named .java-version
check ".java-version directory is skipped, walk continues" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
rmdir "$work/b/1/2/.java-version"

ln -s /nonexistent-target "$work/b/1/.java-version"   # dangling symlink
check "dangling .java-version symlink is skipped" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
rm "$work/b/1/.java-version"

cd "$work"
jolta default 96 >/dev/null 2>&1
mkdir -p "$work/b2" && cd "$work/b2"
check "default used when nothing pinned" "corretto-96.0.1" "$(jolta home 2>/dev/null)"
echo 97 > .java-version
check "pin beats default" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
mkdir -p "$work/b3"
check "sibling pin does not leak" "corretto-96.0.1" "$(cd "$work/b3" && jolta home 2>/dev/null)"

ln -s "$work/b2" "$work/b2-link"
check "pin found through a symlinked cwd" "temurin-97.0.1" \
  "$(cd "$work/b2-link" && jolta home 2>/dev/null)"

printf 'nonsense-spec\n' > "$JOLTA_HOME/default"
mkdir -p "$work/b4" && cd "$work/b4"
jolta_rc java -version
check_rc "garbage default fails resolution" nonzero "$rc"
check "error blames the default file" "jolta default" "$out"
jolta default 96 >/dev/null 2>&1

cd "$work/b4"
jolta pin 97 >/dev/null 2>&1
check_eq "jolta pin writes exactly 'spec\\n'" "97" "$(cat .java-version)"
jolta pin 96 >/dev/null 2>&1
check_eq "jolta pin overwrites an existing pin" "96" "$(cat .java-version)"

# deleted cwd: resolution must not crash (falls back to the default)
mkdir -p "$work/doomed" && cd "$work/doomed" && rm -rf "$work/doomed"
jolta_rc jolta home
check_rc "deleted cwd does not crash resolution" 0 "$rc"
cd "$work"

# =================================================================
section "C. .sdkmanrc support (sdkman-inspired)"
# =================================================================

mkdir -p "$work/c" && cd "$work/c"

printf 'java=97.0.1-tem\n' > .sdkmanrc
check ".sdkmanrc -tem suffix -> temurin" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'java=97.0.1-tem' > .sdkmanrc   # no trailing newline
check ".sdkmanrc without trailing newline" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'java=96.0.1-amzn\n' > .sdkmanrc
check ".sdkmanrc -amzn suffix -> corretto" "corretto-96.0.1" "$(jolta home 2>/dev/null)"

printf 'java=97.0.1-librca\n' > .sdkmanrc
check ".sdkmanrc unknown suffix matches any vendor" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf '# toolchain\nmaven=3.9.6\n\njava=97.0.1-tem\n' > .sdkmanrc
check "comments/blank/other candidates skipped" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

printf 'java=97.0.1-tem\r\n' > .sdkmanrc
check ".sdkmanrc with CRLF endings" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

# sdkman's parser strips all whitespace, so "java = x" is legal there
printf 'java = 97.0.1-tem\n' > .sdkmanrc
check ".sdkmanrc spaces around equals (sdkman-legal)" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

# sdkman treats # as comment-start anywhere in the line
printf 'java=97.0.1-tem # pinned for CI\n' > .sdkmanrc
check ".sdkmanrc inline comment after value" "97.0.1" "$(jolta home 2>/dev/null)"

printf 'java=96.0.1-amzn\njava=97.0.1-tem\n' > .sdkmanrc
check ".sdkmanrc duplicate java lines: first wins" "corretto-96.0.1" "$(jolta home 2>/dev/null)"

printf 'java=97.0.9-tem\n' > .sdkmanrc
jolta_rc jolta home
check_rc ".sdkmanrc exact version is strict" nonzero "$rc"

# fall-through cases: rc without a usable java= line must continue the walk
mkdir -p "$work/c/sub" && cd "$work/c/sub"
echo 96 > "$work/c-parent-pin" && mv "$work/c-parent-pin" "$work/.java-version"
printf 'maven=3.9.6\n' > "$work/c/.sdkmanrc"
check ".sdkmanrc without java= continues walk" "corretto-96.0.1" "$(jolta home 2>/dev/null)"
printf 'java=\n' > "$work/c/.sdkmanrc"
check ".sdkmanrc with empty java= continues walk" "corretto-96.0.1" "$(jolta home 2>/dev/null)"
: > "$work/c/.sdkmanrc"
check "empty .sdkmanrc continues walk" "corretto-96.0.1" "$(jolta home 2>/dev/null)"
rm "$work/.java-version"

cd "$work/c"
printf 'java=96.0.1-amzn\n' > .sdkmanrc
echo 97 > .java-version
check ".java-version beats .sdkmanrc in same dir" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
rm .java-version

mkdir -p "$work/c2/sub" && cd "$work/c2"
echo 97 > .java-version
printf 'java=96.0.1-amzn\n' > sub/.sdkmanrc
cd "$work/c2/sub"
check "child .sdkmanrc beats parent .java-version" "corretto-96.0.1" "$(jolta home 2>/dev/null)"

# =================================================================
section "D. shim fidelity (volta-inspired)"
# =================================================================

mkdir -p "$work/d" && cd "$work/d"
echo 97 > .java-version

out=$(java 'two words' 'uni-héllo' '' 2>&1)
check "args with spaces survive" "arg=[two words]" "$out"
check "unicode args survive" "arg=[uni-héllo]" "$out"
check "empty arg survives" "arg=[]" "$out"

out=$(java '*' '$HOME' 'a;b' 2>&1)
check "glob arg not expanded" "arg=[*]" "$out"
check "dollar arg not expanded" "arg=[\$HOME]" "$out"
check "semicolon arg survives" "arg=[a;b]" "$out"

nlarg=$(printf 'line1\nline2')
check "newline inside an arg survives" "line2]" "$(java "$nlarg" 2>&1)"

java --fake-exit 3 >/dev/null 2>&1; rc=$?
check_eq "shim passes exit code through (3)" 3 "$rc"
java --fake-exit 0 >/dev/null 2>&1; rc=$?
check_eq "shim passes exit code through (0)" 0 "$rc"
java --fake-sig >/dev/null 2>&1; rc=$?
check_eq "shim reflects signal death (SIGSEGV=139)" 139 "$rc"

check "stdin reaches the tool" "hello-stdin" "$(echo hello-stdin | java --fake-cat 2>&1)"
big=$(dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\0' 'x' | java --fake-cat | wc -c | tr -d ' ')
check_eq "large stdin (128KB) not truncated" 131072 "$big"

check_eq "stdout/stderr stay separate (stdout)" "OUT" "$(java --fake-streams 2>/dev/null)"
check_eq "stdout/stderr stay separate (stderr)" "ERR" "$(java --fake-streams 2>&1 1>/dev/null)"

check "shim preserves the working directory" "$work/d" "$(java --fake-pwd 2>&1)"

check "shim exports JAVA_HOME to the tool" "JAVA_HOME=$JOLTA_HOME/jdks/temurin-97.0.1" "$(java 2>&1)"

check "shim works via absolute path" "fake-java 97.0.1" "$("$JOLTA_HOME/shims/java" 2>&1)"

check "javac shim dispatches to javac" "fake-javac 97.0.1" "$(javac 2>&1)"

# `jolta which` must agree with what the shim actually execs
shim_dir=$(java 2>/dev/null | sed -n 's/.*dir=\([^ ]*\).*/\1/p')
check_eq "which agrees with the shim's exec path" "$shim_dir/bin/java" "$(jolta which java 2>&1)"

jolta_rc megatool          # only exists in corretto-96, pin here is 97
check_rc "tool missing from resolved JDK fails" nonzero "$rc"
check "missing-tool error names the tool and JDK" "megatool" "$out"

printf '90\n' > .java-version   # nothing installed for 90
jolta_rc java -version
check "resolution error cites the pin file" "$work/d/.java-version" "$out"
check "resolution error suggests jolta install" "jolta install" "$out"
check "resolution error lists installed JDKs" "temurin-97.0.1" "$out"
echo 97 > .java-version

touch "$JOLTA_HOME/shims/not-a-shim"
jolta reshim >/dev/null
[ ! -e "$JOLTA_HOME/shims/not-a-shim" ] \
  && ok "reshim clears foreign files from shims dir" \
  || bad "reshim clears foreign files from shims dir" "     still present"

rm "$JOLTA_HOME/jdks/corretto-96.0.1/bin/megatool"
jolta reshim >/dev/null
[ ! -e "$JOLTA_HOME/shims/megatool" ] \
  && ok "stale shim removed after tool disappears" \
  || bad "stale shim removed after tool disappears" "     still present"
mk_jdk corretto-96.0.1 96.0.1 "Amazon.com Inc." java javac megatool

cat > "$JOLTA_HOME/jdks/temurin-97.0.1/bin/joltafake" <<'EOF'
#!/bin/sh
echo evil
EOF
chmod +x "$JOLTA_HOME/jdks/temurin-97.0.1/bin/joltafake"
jolta reshim >/dev/null
[ ! -e "$JOLTA_HOME/shims/joltafake" ] \
  && ok "reshim skips jolta-prefixed tool names" \
  || bad "reshim skips jolta-prefixed tool names" "     shim created"
rm "$JOLTA_HOME/jdks/temurin-97.0.1/bin/joltafake"

echo data > "$JOLTA_HOME/jdks/temurin-97.0.1/bin/notes.txt"   # not executable
jolta reshim >/dev/null
[ ! -e "$JOLTA_HOME/shims/notes.txt" ] \
  && ok "non-executable bin entries are not shimmed" \
  || bad "non-executable bin entries are not shimmed" "     shim created"
rm "$JOLTA_HOME/jdks/temurin-97.0.1/bin/notes.txt"

# =================================================================
section "E. CLI commands (volta/sdkman-inspired)"
# =================================================================

cd "$work/d"

check "which defaults to java" "temurin-97.0.1/bin/java" "$(jolta which 2>&1)"
check "which javac" "temurin-97.0.1/bin/javac" "$(jolta which javac 2>&1)"
jolta_rc jolta which nosuchtool
check_rc "which for missing tool fails" nonzero "$rc"

out=$(jolta current 2>&1)
check "current shows the resolved version" "97.0.1" "$out"
check "current names its source" ".java-version" "$out"

check_eq "home prints the bare JDK home (stdout-pure)" \
  "$JOLTA_HOME/jdks/temurin-97.0.1" "$(jolta home 2>/dev/null)"

env_out=$(jolta env 2>&1)
check "env exports JAVA_HOME" "export JAVA_HOME=\"$JOLTA_HOME/jdks/temurin-97.0.1\"" "$env_out"
check "env prepends bin to PATH" "temurin-97.0.1/bin:\$PATH" "$env_out"

check "exec runs tool from resolved JDK via PATH" "fake-java 97.0.1" \
  "$(jolta exec java 2>&1)"
check "exec sets JAVA_HOME" "temurin-97.0.1" "$(jolta exec sh -c 'echo $JAVA_HOME' 2>&1)"
jolta exec sh -c 'exit 7' >/dev/null 2>&1; rc=$?
check_eq "exec propagates exit code" 7 "$rc"
jolta_rc jolta exec nosuchbin-xyz
check_rc "exec of nonexistent command fails" nonzero "$rc"

jolta_rc jolta exec;        check_rc "exec with no args fails with usage" nonzero "$rc"
jolta_rc jolta pin;         check_rc "pin with no args fails with usage" nonzero "$rc"
jolta_rc jolta install;     check_rc "install with no args fails with usage" nonzero "$rc"
jolta_rc jolta frobnicate;  check_rc "unknown command fails" nonzero "$rc"
check "unknown command names itself" "frobnicate" "$out"
jolta_rc jolta hook fish
check_rc "hook for unsupported shell fails" nonzero "$rc"
check "hook error lists supported shells" "zsh" "$out"

check "version prints" "jolta " "$(jolta version 2>&1)"

jolta_rc jolta uninstall nosuch-1.2.3
check_rc "uninstall of unknown name fails" nonzero "$rc"

# uninstall must not follow path traversal out of the jdks dir
mkdir -p "$JOLTA_HOME/cache"
echo canary > "$JOLTA_HOME/cache/canary"
jolta_rc jolta uninstall ../cache
[ -f "$JOLTA_HOME/cache/canary" ] \
  && ok "uninstall refuses path traversal (../cache intact)" \
  || bad "uninstall refuses path traversal (../cache intact)" "     cache dir was deleted!"

# =================================================================
section "F. JDK discovery edge cases (jenv-inspired)"
# =================================================================

mkdir -p "$work/f" && cd "$work/f"

echo 92 > .java-version
out=$(jolta home 2>&1)
check "macOS bundle (Contents/Home) resolves" "zulu-92.0.1/Contents/Home" "$out"
check "bundle JDK runs through the shim" "fake-java 92.0.1" "$(java 2>&1)"

echo corretto@93 > .java-version   # dir is named mystery-93.5.0
check "vendor detected from release IMPLEMENTOR, not dir name" "mystery-93.5.0" "$(jolta home 2>/dev/null)"

echo graalvm@94 > .java-version
check "GRAALVM_VERSION marks a graalvm JDK" "graalvm-94.0.2" "$(jolta home 2>/dev/null)"

mk_jdk "$work/outside-91" 91.0.3 "Eclipse Adoptium"
echo 91 > .java-version
check "JAVA_HOME_<major> env registers a JDK" "$work/outside-91" \
  "$(JAVA_HOME_91_ARM64="$work/outside-91" jolta home 2>/dev/null)"
check "trailing slash in JAVA_HOME_<major> normalized" "$work/outside-91" \
  "$(JAVA_HOME_91_ARM64="$work/outside-91/" jolta home 2>/dev/null)"
jolta_rc jolta home
check_rc "env-registered JDK gone -> resolution fails" nonzero "$rc"

jolta_rc jolta jdks
check_rc "jdks survives hostile fixture dirs" 0 "$rc"
check_not "jdks omits unparseable JAVA_VERSION" "abc" "$out"
jolta_rc jolta list
check_rc "list survives hostile fixture dirs" 0 "$rc"

echo 87 > .java-version
count=$(jolta jdks | awk -F'	' '$1==87' | wc -l | tr -d ' ')
check_eq "same version under two vendors both listed" 2 "$count"
echo 87.0.7 > .java-version
jolta_rc jolta home
check_rc "vendorless exact pin with two candidates resolves" 0 "$rc"
check "vendorless exact resolves to one of them" "87.0.7" "$out"

# resolution cache: poison it, jolta must self-heal
echo 97 > .java-version
jolta home >/dev/null 2>&1     # warm the cache
cache_file=$(ls "$JOLTA_HOME"/cache/v-97 2>/dev/null | head -1)
if [ -n "$cache_file" ]; then
  echo "/nonexistent/path" > "$cache_file"
  check "poisoned resolution cache self-heals" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
else
  bad "poisoned resolution cache self-heals" "     no cache file found at cache/v-97"
fi

# uninstall of the active JDK: resolution must fail afterwards, not go stale
mk_jdk temurin-85.0.1 85.0.1 "Eclipse Adoptium"
jolta reshim >/dev/null
echo 85 > .java-version
jolta home >/dev/null 2>&1     # warm cache on the doomed JDK
jolta uninstall temurin-85.0.1 >/dev/null 2>&1
jolta_rc java -version
check_rc "uninstalled active JDK -> shim fails cleanly" nonzero "$rc"
check "post-uninstall error is the no-match error" "no installed JDK matches" "$out"

# =================================================================
section "G. offline install lifecycle via file:// mirror (volta/sdkman)"
# =================================================================

mirror="$work/mirror"
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64)  plat="macos-aarch64" ;;
  Darwin/x86_64) plat="macos-x64" ;;
  Linux/aarch64) plat="linux-aarch64" ;;
  *)             plat="linux-x64" ;;
esac
# publish <vendor> <spec-dir> <version> [--flat|--norelease]: stage a fake JDK
# tarball at the mirror path jolta derives for "jolta install <vendor>@<spec>"
publish() {
  pvendor=$1 pspec=$2 pversion=$3 pmode=${4:-}
  stage="$work/stage-$pversion"; rm -rf "$stage"
  mkdir -p "$stage/jdk-root/bin"
  printf 'JAVA_VERSION="%s"\nIMPLEMENTOR="Eclipse Adoptium"\n' "$pversion" > "$stage/jdk-root/release"
  printf '#!/bin/sh\necho "fake-java %s mirror"\n' "$pversion" > "$stage/jdk-root/bin/java"
  chmod +x "$stage/jdk-root/bin/java"
  mkdir -p "$mirror/$pvendor/$pspec"
  case "$pmode" in
    --flat)      tar -C "$stage/jdk-root" -czf "$mirror/$pvendor/$pspec/$plat.tar.gz" release bin ;;
    --norelease) rm "$stage/jdk-root/release"
                 tar -C "$stage" -czf "$mirror/$pvendor/$pspec/$plat.tar.gz" jdk-root ;;
    *)           tar -C "$stage" -czf "$mirror/$pvendor/$pspec/$plat.tar.gz" jdk-root ;;
  esac
}
no_stale_locks() {
  if ls "$JOLTA_HOME"/cache/install-*.lock >/dev/null 2>&1; then return 1; fi
  return 0
}

mkdir -p "$work/g" && cd "$work/g"

# corrupted archive: fail cleanly, leave nothing behind, allow retry
mkdir -p "$mirror/temurin/90"
echo "this is not a tarball" > "$mirror/temurin/90/$plat.tar.gz"
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 90
check_rc "corrupted archive install fails" nonzero "$rc"
[ ! -d "$JOLTA_HOME/jdks/temurin-90.0.1" ] \
  && ok "corrupted install leaves no partial JDK" \
  || bad "corrupted install leaves no partial JDK" "     partial dir exists"
no_stale_locks && ok "corrupted install leaves no stale lock" \
  || bad "corrupted install leaves no stale lock" "     $(ls "$JOLTA_HOME"/cache/install-*.lock)"
rm -rf "$JOLTA_HOME"/cache/install-*.lock

publish temurin 90 90.0.1
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 90
check_rc "retry after corrupted install succeeds" 0 "$rc"
check "retry installs the JDK" "installed temurin 90.0.1" "$out"

# already-installed: idempotent, says so, exit 0
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 90
check_rc "reinstall of installed version succeeds" 0 "$rc"
check "reinstall reports already installed" "already installed" "$out"

# mirror base with a trailing slash must behave identically
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror/" jolta install 90
check_rc "mirror base with trailing slash works" 0 "$rc"

# flat archive (no top-level dir)
publish temurin 89 89.0.1 --flat
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 89
check_rc "flat archive (no top dir) fails" nonzero "$rc"
no_stale_locks && ok "flat-archive failure leaves no stale lock" \
  || bad "flat-archive failure leaves no stale lock" "     $(ls "$JOLTA_HOME"/cache/install-*.lock)"
rm -rf "$JOLTA_HOME"/cache/install-*.lock

# archive without a release file
publish temurin 88.5 88.5.0 --norelease
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 88.5
check_rc "archive without release file fails" nonzero "$rc"
[ ! -d "$JOLTA_HOME/jdks/temurin-88.5.0" ] \
  && ok "no-release install leaves no partial JDK" \
  || bad "no-release install leaves no partial JDK" "     partial dir exists"
no_stale_locks && ok "no-release failure leaves no stale lock" \
  || bad "no-release failure leaves no stale lock" "     $(ls "$JOLTA_HOME"/cache/install-*.lock)"
rm -rf "$JOLTA_HOME"/cache/install-*.lock

# volta-style autodownload: first shim use fetches the pinned JDK
publish temurin 88 88.0.2
mkdir -p "$work/g2" && cd "$work/g2"
echo 88 > .java-version
out=$(env -u JOLTA_NO_AUTO_INSTALL JOLTA_DOWNLOAD_BASE="file://$mirror" java 2>&1); rc=$?
check_rc "shim auto-installs pinned-but-missing JDK" 0 "$rc"
check "auto-install announces the fetch" "not installed" "$out"
check "auto-installed java actually ran" "fake-java 88.0.2 mirror" "$out"

# concurrent installs of the same version: both succeed, one JDK results
publish temurin 81 81.0.1
( env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 81 >/dev/null 2>&1; echo $? > "$work/rc1" ) &
( env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 81 >/dev/null 2>&1; echo $? > "$work/rc2" ) &
wait
check_eq "concurrent installs of same version both succeed" "0 0" "$(cat "$work/rc1") $(cat "$work/rc2")"
count=$(ls -d "$JOLTA_HOME"/jdks/temurin-81* 2>/dev/null | wc -l | tr -d ' ')
check_eq "concurrent installs produce exactly one JDK" 1 "$count"
no_stale_locks && ok "concurrent installs leave no stale lock" \
  || bad "concurrent installs leave no stale lock" "     $(ls "$JOLTA_HOME"/cache/install-*.lock)"

# pin auto-installs too (and warns-but-writes when auto-install is off)
publish temurin 86 86.0.1
mkdir -p "$work/g3" && cd "$work/g3"
out=$(env -u JOLTA_NO_AUTO_INSTALL JOLTA_DOWNLOAD_BASE="file://$mirror" jolta pin 86 2>&1); rc=$?
check_rc "pin auto-installs a missing version" 0 "$rc"
check_eq "pin wrote the file" "86" "$(cat .java-version)"
[ -d "$JOLTA_HOME/jdks/temurin-86.0.1" ] \
  && ok "pin fetched the JDK from the mirror" \
  || bad "pin fetched the JDK from the mirror" "     jdks/temurin-86.0.1 missing"

mkdir -p "$work/g4" && cd "$work/g4"
jolta_rc jolta pin 83                        # nothing published for 83, no auto-install
check_rc "pin of unavailable version still exits 0" 0 "$rc"
check "pin warns when nothing matches" "no installed JDK" "$out"
check_eq "warned pin still writes the file" "83" "$(cat .java-version)"

jolta_rc jolta default 82
check_rc "default of missing version exits 0" 0 "$rc"
check "default warns when nothing matches" "no installed JDK" "$out"
jolta default 96 >/dev/null 2>&1             # restore a sane default

# a mirror path containing a space (URL quoting regression)
spacemirror="$work/mir ror"
saved_mirror=$mirror; mirror=$spacemirror
publish temurin 80 80.0.1
mirror=$saved_mirror
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$spacemirror" jolta install 80
check_rc "mirror path with spaces installs" 0 "$rc"

# uninstall the mirror-installed JDKs; listing updates
jolta_rc jolta uninstall temurin-90.0.1
check_rc "uninstall managed JDK succeeds" 0 "$rc"
check_not "uninstalled JDK gone from jdks" "90.0.1" "$(jolta jdks 2>&1)"
for v in temurin-88.0.2 temurin-86.0.1 temurin-81.0.1 temurin-80.0.1; do
  jolta uninstall "$v" >/dev/null 2>&1
done

# =================================================================
section "H. robustness & shell integration (volta/jenv-inspired)"
# =================================================================

# version/help must work even with completely broken state
jolta_rc env JOLTA_JAVA_VERSION=bogus jolta version
check_rc "version works with bogus env override" 0 "$rc"
printf 'nonsense\n' > "$JOLTA_HOME/default"
jolta_rc jolta help
check_rc "help works with garbage default file" 0 "$rc"
jolta default 96 >/dev/null 2>&1

# HOME unset: JOLTA_HOME is explicit, so commands should still work
jolta_rc env -u HOME -u USERPROFILE jolta version
check_rc "version works without HOME" 0 "$rc"
mkdir -p "$work/h2" && cd "$work/h2"
echo corretto-96 > .java-version
jolta_rc env -u HOME -u USERPROFILE jolta home
check_rc "resolution works without HOME when JOLTA_HOME is set" 0 "$rc"

# hook output must be syntactically valid shell
jolta hook bash > "$work/hook-bash.sh" 2>/dev/null
bash -n "$work/hook-bash.sh" 2>/dev/null \
  && ok "bash hook output parses (bash -n)" \
  || bad "bash hook output parses (bash -n)" "     syntax error"
if command -v zsh >/dev/null 2>&1; then
  jolta hook zsh > "$work/hook-zsh.sh" 2>/dev/null
  zsh -n "$work/hook-zsh.sh" 2>/dev/null \
    && ok "zsh hook output parses (zsh -n)" \
    || bad "zsh hook output parses (zsh -n)" "     syntax error"
fi

# eval-ing the bash hook twice must not stack PROMPT_COMMAND entries
hookcount=$(bash --noprofile --norc -c '
  export PATH="'"$JOLTA_HOME/shims:$bindir"':$PATH" JOLTA_HOME="'"$JOLTA_HOME"'"
  eval "$(jolta hook bash)"; eval "$(jolta hook bash)"
  printf "%s" "$PROMPT_COMMAND" | grep -c _jolta_sync')
check_eq "bash hook eval is idempotent" 1 "$hookcount"

# doctor: healthy here, unhealthy when the shims dir is off PATH
cd "$work/d"
jolta_rc jolta doctor
check_rc "doctor passes in a healthy setup" 0 "$rc"
jolta_rc env PATH="$bindir:/usr/bin:/bin" "$JOLTA_BIN" doctor
check_rc "doctor fails when shims are off PATH" nonzero "$rc"
check "doctor names the PATH problem" "PATH" "$out"

# a JOLTA_HOME containing spaces must not break shims or resolution
spacehome="$work/home with space"
mkdir -p "$spacehome/jdks"
(
  export JOLTA_HOME="$spacehome"
  mk_jdk temurin-84.0.1 84.0.1 "Eclipse Adoptium"
  "$JOLTA_BIN" reshim >/dev/null 2>&1
  export PATH="$spacehome/shims:$bindir:$PATH"
  mkdir -p "$work/h" && cd "$work/h"
  echo 84 > .java-version
  check "JOLTA_HOME with spaces: shim resolves" "fake-java 84.0.1" "$(java 2>&1)"
  check "JOLTA_HOME with spaces: jolta home" "temurin-84.0.1" "$(jolta home 2>/dev/null)"
  echo "$pass $fail" > "$work/h-counts"
  printf '%s' "$failed_names" > "$work/h-fails"
)
read -r pass fail < "$work/h-counts"
failed_names=$(cat "$work/h-fails")

# jolta list | head must not panic on SIGPIPE
out=$(jolta list 2>&1 | head -1)
check_not "list|head does not panic on SIGPIPE" "failed printing" "$out"

# =================================================================
section "I. mise-inspired edge cases"
# =================================================================

mk_jdk temurin-9.0.4   9.0.4   "Eclipse Adoptium"
mk_jdk temurin-75.0.1  75.0.1  "Eclipse Adoptium"
mk_jdk temurin-75.0.10 75.0.10 "Eclipse Adoptium"
mkdir -p "$work/i" && cd "$work/i"

# comment handling in .java-version
printf '# team standard\n97\n' > .java-version
check "full-line comment then version" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
printf '97 # pinned for CI\n' > .java-version
check "inline comment after version" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
printf '# only a comment\n' > .java-version
check "comment-only file behaves as absent (default wins)" "corretto-96.0.1" "$(jolta home 2>/dev/null)"

# multi-line file: a missing FIRST version must error, not fall through
printf '74\n97\n' > .java-version
jolta_rc jolta home
check_rc "missing first version errors (no silent 2nd-line fallback)" nonzero "$rc"
check "that error names the first version" "74" "$out"

# hostile version strings from a file are inert
printf '$(touch %s/pwned)\n' "$work" > .java-version
jolta_rc jolta home
check_rc "command-substitution spec fails cleanly" nonzero "$rc"
[ ! -e "$work/pwned" ] \
  && ok "command-substitution spec is never executed" \
  || bad "command-substitution spec is never executed" "     pwned file created!"
printf '../../etc\n' > .java-version
jolta_rc jolta home
check_rc "path-traversal spec fails cleanly" nonzero "$rc"

# prefix boundaries (mise shipped both of these bugs)
printf '9\n' > .java-version
check "major 9 does not match 97/9x" "temurin-9.0.4" "$(jolta home 2>/dev/null)"
printf '75.0.1\n' > .java-version
check "75.0.1 does not prefix-match 75.0.10" "temurin-75.0.1" "$(jolta home 2>/dev/null)"
printf '75\n' > .java-version
check "major pin takes highest build (75.0.10)" "temurin-75.0.10" "$(jolta home 2>/dev/null)"
printf '75.0.1+7\n' > .java-version
check "+build metadata in pin matches base version" "temurin-75.0.1" "$(jolta home 2>/dev/null)"

# .sdkmanrc traps
mkdir -p "$work/i2" && cd "$work/i2"
printf 'javafx=94.0.2-graalce\njava=97.0.1-tem\n' > .sdkmanrc
check "javafx= key does not shadow java=" "temurin-97.0.1" "$(jolta home 2>/dev/null)"
printf '# java=94.0.2-tem\njava=97.0.1-tem\n' > .sdkmanrc
check "commented-out java= line is ignored" "temurin-97.0.1" "$(jolta home 2>/dev/null)"

# a pre-set JAVA_HOME must be replaced, never trusted
cd "$work/d"
check "shim replaces inherited JAVA_HOME" "JAVA_HOME=$JOLTA_HOME/jdks/temurin-97.0.1" \
  "$(JAVA_HOME=/wrong java 2>&1)"
check "exec replaces inherited JAVA_HOME" "temurin-97.0.1" \
  "$(JAVA_HOME=/wrong jolta exec sh -c 'echo $JAVA_HOME' 2>&1)"

# exec PATH ordering: resolved JDK bin is FIRST, and beats a stale JDK on PATH
first_entry=$(jolta exec sh -c 'echo "$PATH"' 2>/dev/null | tr ':' '\n' | head -1)
check_eq "exec puts resolved bin first on PATH" "$JOLTA_HOME/jdks/temurin-97.0.1/bin" "$first_entry"
check "stale JDK earlier on PATH does not win in exec" "temurin-97.0.1/bin/java" \
  "$(PATH="$JOLTA_HOME/jdks/corretto-96.0.1/bin:$PATH" jolta exec sh -c 'command -v java' 2>&1)"

# jolta env output must survive strict-mode eval
out=$(sh -u -c 'eval "$('"$JOLTA_BIN"' env)" && echo "OK:$JAVA_HOME"' 2>&1)
check "env output is eval-safe under set -u" "OK:$JOLTA_HOME/jdks/temurin-97.0.1" "$out"

# hooks must survive nounset shells
out=$(bash --noprofile --norc -c '
  set -u
  export PATH="'"$JOLTA_HOME/shims:$bindir"':$PATH" JOLTA_HOME="'"$JOLTA_HOME"'"
  eval "$(jolta hook bash)" && echo NOUNSET-OK' 2>&1)
check "bash hook works under set -u" "NOUNSET-OK" "$out"
if command -v zsh >/dev/null 2>&1; then
  out=$(zsh -f -c '
    setopt nounset
    export PATH="'"$JOLTA_HOME/shims:$bindir"':$PATH" JOLTA_HOME="'"$JOLTA_HOME"'"
    eval "$(jolta hook zsh)" && echo NOUNSET-OK' 2>&1)
  check "zsh hook works under nounset" "NOUNSET-OK" "$out"
fi

# resolution of an installed pin must make zero network calls (curl spy)
spybin="$work/spybin"; mkdir -p "$spybin"
printf '#!/bin/sh\ntouch "%s/curl-called"\nexit 1\n' "$work" > "$spybin/curl"
chmod +x "$spybin/curl"
env PATH="$spybin:/usr/bin:/bin" "$JOLTA_BIN" home >/dev/null 2>&1
[ ! -e "$work/curl-called" ] \
  && ok "offline resolution never shells out to curl" \
  || bad "offline resolution never shells out to curl" "     curl was invoked"

# JOLTA_NO_AUTO_INSTALL must be respected by every entry point
publish temurin 74 74.0.1
mkdir -p "$work/i3" && cd "$work/i3"
echo 74 > .java-version
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" java -version
check_rc "no-auto-install: shim refuses" nonzero "$rc"
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta env
check_rc "no-auto-install: env refuses" nonzero "$rc"
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta exec sh -c true
check_rc "no-auto-install: exec refuses" nonzero "$rc"
[ ! -d "$JOLTA_HOME/jdks/temurin-74.0.1" ] \
  && ok "no-auto-install: nothing was downloaded" \
  || bad "no-auto-install: nothing was downloaded" "     temurin-74.0.1 appeared"

# uninstall drops shims for tools only that JDK provided
mk_jdk temurin-73.0.1 73.0.1 "Eclipse Adoptium" java javac soloitool
jolta reshim >/dev/null
[ -e "$JOLTA_HOME/shims/soloitool" ] || bad "uninstall reshim precondition" "     soloitool shim missing"
jolta uninstall temurin-73.0.1 >/dev/null 2>&1
[ ! -e "$JOLTA_HOME/shims/soloitool" ] \
  && ok "uninstall reshims (unique tool's shim removed)" \
  || bad "uninstall reshims (unique tool's shim removed)" "     soloitool shim survived"
[ -e "$JOLTA_HOME/shims/java" ] \
  && ok "uninstall reshims (shared java shim kept)" \
  || bad "uninstall reshims (shared java shim kept)" "     java shim gone"

# dotfiles in a JDK's bin/ are not shimmed
printf '#!/bin/sh\n' > "$JOLTA_HOME/jdks/temurin-97.0.1/bin/.hidden-helper"
chmod +x "$JOLTA_HOME/jdks/temurin-97.0.1/bin/.hidden-helper"
jolta reshim >/dev/null
[ ! -e "$JOLTA_HOME/shims/.hidden-helper" ] \
  && ok "dotfiles in bin/ are not shimmed" \
  || bad "dotfiles in bin/ are not shimmed" "     .hidden-helper shim created"
rm "$JOLTA_HOME/jdks/temurin-97.0.1/bin/.hidden-helper"

# invalid UTF-8 in argv must not panic (CLI and shim paths)
out=$(jolta "$(printf 'ver\303')" 2>&1); rc=$?
check_not "invalid UTF-8 CLI arg does not panic" "panic" "$out"
check_rc "invalid UTF-8 CLI arg fails as unknown command" nonzero "$rc"
cd "$work/d"
out=$(java "$(printf 'arg\303')" 2>&1); rc=$?
check_rc "invalid UTF-8 shim arg passes through" 0 "$rc"
check_not "invalid UTF-8 shim arg does not panic" "panic" "$out"

# upgrade of one major leaves other majors alone
publish temurin 78 78.0.1
publish temurin 77 77.0.1
env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 78 >/dev/null 2>&1
env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta install 77 >/dev/null 2>&1
publish temurin 78 78.0.2
jolta_rc env JOLTA_DOWNLOAD_BASE="file://$mirror" jolta upgrade 78
check_rc "targeted upgrade succeeds" 0 "$rc"
[ -d "$JOLTA_HOME/jdks/temurin-78.0.2" ] && [ ! -d "$JOLTA_HOME/jdks/temurin-78.0.1" ] \
  && ok "targeted upgrade replaced 78.0.1 with 78.0.2" \
  || bad "targeted upgrade replaced 78.0.1 with 78.0.2" "     $(ls -d "$JOLTA_HOME"/jdks/temurin-78* 2>/dev/null)"
[ -d "$JOLTA_HOME/jdks/temurin-77.0.1" ] \
  && ok "targeted upgrade left major 77 untouched" \
  || bad "targeted upgrade left major 77 untouched" "     temurin-77.0.1 gone"
jolta uninstall temurin-78.0.2 >/dev/null 2>&1
jolta uninstall temurin-77.0.1 >/dev/null 2>&1

# bare `jolta` prints help and exits 0
jolta_rc jolta
check_rc "bare jolta exits 0" 0 "$rc"
check "bare jolta prints usage" "Usage" "$out"

# =================================================================
echo
echo "passed: $pass, failed: $fail"
if [ "$fail" -gt 0 ]; then
  echo "failing:$failed_names"
fi
[ "$fail" -eq 0 ]
