# jolta shell hook — see src/hooks/common.sh for what the two checks cover:
# $JOLTA_HOME/.stamp (jolta run elsewhere) and a fingerprint of the pin file
# (a checkout or an edit rewriting it under a shell that never moved).
function __jolta_update_java_home
  set -l _jh (jolta home 2>/dev/null)
  if test -n "$_jh"
    set -gx JAVA_HOME $_jh
  else
    set -e JAVA_HOME
  end
end

function __jolta_pin_id
  set -l dir $PWD
  while true
    for f in "$dir/.java-version" "$dir/.sdkmanrc"
      if test -r "$f"
        set -l txt ""
        while read -l line
          set txt "$txt$line;"
        end <"$f"
        set -g __jolta_pin "$f=$txt"
        return
      end
    end
    if test -z "$dir"
      break
    end
    set dir (string replace -r '/[^/]*$' '' -- "$dir")
  end
  set -g __jolta_pin ""
end

function __jolta_sync --on-event fish_prompt
  set -l _dir "$JOLTA_HOME"
  test -n "$_dir"; or set _dir "$HOME/.jolta"
  set -l _s ""
  if test -f "$_dir/.stamp"
    read _s <"$_dir/.stamp"
  end
  __jolta_pin_id
  if test "$_s" = "$__jolta_stamp" -a "$__jolta_pin" = "$__jolta_pin_last" \
       -a "$__jolta_pin_ok" = 1
    return
  end
  set -g __jolta_stamp "$_s"
  set -g __jolta_pin_last "$__jolta_pin"
  __jolta_update_java_home
  if test -n "$JAVA_HOME" -o -z "$__jolta_pin"
    set -g __jolta_pin_ok 1
  else
    set -g __jolta_pin_ok 0
  end
end

function __jolta_cd --on-variable PWD
  __jolta_sync
end

__jolta_sync
