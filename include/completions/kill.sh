_kill_comp() {
	case "$3" in
		-s|-l) compadd -D 'signal' $(compgen -S -- "$2") ;;
		*)
			case "$2" in
				-*) compadd -D 'signal' -P '-' $(compgen -S -- "${2#-}") ;;
				*) compadd -D 'job' $(compgen -j -- "$2") ;;
			esac
		;;
	esac
}
complete -F _kill_comp kill
