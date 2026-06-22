_readonly_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _readonly_comp readonly
