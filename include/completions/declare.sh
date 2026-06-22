_declare_comp() {
  local -A flags=(
    [f]='show function body'
    [F]='list defined functions'
    [p]='display variables'
    [r]='declare a variable as readonly'
    [x]='declare a variable as exported'
    [a]='declare a variable as an array'
    [A]='declare a variable as an associative array'
    [i]='declare a variable as an integer'
  )

	case "$3" in
		-f|-F)
			local funcs=( $(declare -F | vice --lines -m 'WW' -c 'E') )
			compadd -D 'function' -a funcs
		;;
		*) compadd -A flags -P '-' ;;
	esac
}
complete -F _declare_comp declare
