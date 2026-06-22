_readkey_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _readkey_comp readkey
