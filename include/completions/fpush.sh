_fpush_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _fpush_comp fpush
