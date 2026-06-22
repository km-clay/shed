_builtin_comp() { compadd -D 'builtin' $(compgen -b -- "$2"); }
complete -F _builtin_comp builtin
