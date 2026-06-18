qpad() {
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))

	local target=0

	local -a records=()
	local -a headers=()

	if ((has_names)); then
		read -q -a headers
		target="${#headers[@]}"
	fi

	while IFS= read -r line; do
		push records "$line"
		local -a fields
		unquote -a fields "$line"
		(( ${#fields[@]} > target )) && target=${#fields[@]}
	done

	if ((has_names)); then
		while (( ${#headers[@]} < target )); do
			push headers ""
		done
		quote "${headers[@]}"
	fi

	for record in "${records[@]}"; do
		local -a fields
		unquote -a fields "$record"
		while (( ${#fields[@]} < target )); do
			push fields ""
		done
		quote "${fields[@]}"
	done
}
