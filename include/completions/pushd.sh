_pushd_comp() { compadd -D 'directory' $(compgen -d -- "$2"); }
complete -F _pushd_comp pushd
