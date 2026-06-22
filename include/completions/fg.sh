_fg_comp() { compadd -D 'job' $(compgen -j -- "$2"); }
complete -F _fg_comp fg
