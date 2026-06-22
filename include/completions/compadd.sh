_compadd_comp() {
  local -A flags=(
    [P]="candidate prefix"
    [S]="candidate suffix"
    [d]="description array"
    [a]="candidate array"
    [A]="associative array (candidate=description)"
  )
	case "$2" in
		-) compadd -P '-' -A flags ;;
		*)
			case "$3" in
        compadd) compadd -P '-' -A flags ;;
				-d|-a)
					local vars=( $(compgen -v) )
					for var in "${vars[@]}"; do
						case $(type -s "$var") in
							array)
								compadd "$var"
							;;
						esac
					done
				;;
			esac
		;;
	esac
}
complete -F _compadd_comp compadd
