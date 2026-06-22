_hash_comp() { compadd -D 'command' $(compgen -c -- "$2"); }
complete -F _hash_comp hash
