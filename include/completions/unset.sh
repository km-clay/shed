_unset_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _unset_comp unset
