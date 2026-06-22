_rotate_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _rotate_comp rotate
