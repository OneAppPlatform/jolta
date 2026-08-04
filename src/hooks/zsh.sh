_jolta_rehash() { rehash; return 0; }
autoload -Uz add-zsh-hook
# chpwd catches a cd mid-command-line (`cd svc && mvn -v`, before any prompt);
# precmd catches everything else — a checkout that swaps the pin, an install
# from another shell. Both go through the cheap gate, so a cd that doesn't
# change the pin costs nothing either.
add-zsh-hook chpwd _jolta_sync
add-zsh-hook precmd _jolta_sync
_jolta_sync
