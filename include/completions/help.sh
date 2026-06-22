_help_comp() {
	local tags=( $(help -l) )
	compadd -D 'help topic' -a tags
}
complete -f -F _help_comp help
