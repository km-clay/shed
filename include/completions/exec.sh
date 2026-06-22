_exec_comp() { compadd -D 'command' $(compgen -c -- "$2"); }
complete -F _exec_comp exec
