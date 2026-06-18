qpipe() {
	local has_names=0
	local OPTIND

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $(( OPTIND - 1 ))

	if ((has_names)); then
		# consume headers row
		IFS= read -q -a headers
	fi

	while IFS= read -q -a fields; do
		local -a transformed=()

		while IFS= read -r line; do
			push transformed "$line"
		done < <(printf '%s\n' "${fields[@]}" | "$@")

		if ((has_names)); then
			quote "${headers[@]}"
		fi

		[ "${#transformed[@]}" -gt 0 ] && quote "${transformed[@]}"
	done
}