_read_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _read_comp read
