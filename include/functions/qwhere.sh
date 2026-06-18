qwhere() {
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))

	local body="$1"

	if ((has_names)); then
		local header_line
		IFS= read -r header_line
		local -a headers
		unquote -a headers "$header_line"

		local prelude=""
		for i in "${!headers[@]}"; do
			prelude+="local ${headers[i]}=\"\${$((i+1))}\"; "
		done
		eval "__qwhere_lambda() { $prelude $body; }"

		printf '%s\n' "$header_line"
	else
		eval "__qwhere_lambda() { $body; }"
	fi

	defer unset -f __qwhere_lambda

	while IFS= read -r line; do
		local -a fields
		unquote -a fields "$line"
		if __qwhere_lambda "${fields[@]}"; then
			printf '%s\n' "$line"
		fi
	done
}
