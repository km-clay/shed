qwhere() {
	local body="$1"
	local has_names=0
	
	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	
	eval "__qwhere_lambda() { $body; }"
	defer unset -f __qwhere_lambda

	while IFS= read -r line; do
		local -a fields
		unquote -a fields "$line"
		if __qwhere_lambda "${fields[@]}"; then
			printf '%s\n' "$line"
		fi
	done
}