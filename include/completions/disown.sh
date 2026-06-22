_disown_comp() { compadd -D 'job' $(compgen -j -- "$2"); }
complete -F _disown_comp disown
