_type_comp() { compadd -D 'command' $(compgen -c -- "$2"); }
complete -F _type_comp type
