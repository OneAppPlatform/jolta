_jolta_rehash() { hash -r; return 0; }
case ";${PROMPT_COMMAND:-};" in
  *";_jolta_sync;"*) ;;
  *) PROMPT_COMMAND="_jolta_sync${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
_jolta_sync
