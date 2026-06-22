_unalias_comp() { compadd -D 'alias' $(compgen -a -- "$2"); }
complete -F _unalias_comp unalias
