_wait_comp() { compadd -D 'job' $(compgen -j -- "$2"); }
complete -F _wait_comp wait
