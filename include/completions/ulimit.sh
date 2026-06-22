_ulimit_comp() {
	local -A flags=(
		[n]="set open file count limit"
		[u]="set max process count"
		[s]="set max stack size (bytes)"
		[v]="set virtual memory limit (bytes)"
		[c]="set max core dump file size (bytes)"
	)

	case "$2" in
		-*) compadd -A flags -P "-" ;;
		*)
			case "$3" in ulimit) compadd -A flags -P '-' ;; esac
		;;
	esac
}
complete -F _ulimit_comp ulimit
