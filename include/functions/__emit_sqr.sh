__emit_sqr() {
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))

	if [ -t 1 ]; then
		if ((has_names)); then
			qtable -n
		else
			qtable
		fi
	else
		thru
	fi
}