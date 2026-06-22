_command_comp() { compadd -D 'command' $(compgen -c -- "$2"); }
complete -F _command_comp command
