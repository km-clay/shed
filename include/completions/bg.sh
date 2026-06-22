_bg_comp() { compadd -D 'job' $(compgen -j -- "$2"); }
complete -F _bg_comp bg
