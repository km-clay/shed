_export_comp() { compadd -D 'variable' $(compgen -v -- "$2"); }
complete -F _export_comp export
