_fpop_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _fpop_comp fpop
