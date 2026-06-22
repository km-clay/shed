_alias_comp() { compadd -D 'alias' $(compgen -a -- "$2"); }
complete -F _alias_comp alias
