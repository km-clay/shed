_source_comp() { compadd -D 'file' $(compgen -f -- "$2"); }
complete -F _source_comp source
