_local_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _local_comp local
