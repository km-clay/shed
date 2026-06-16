qpad() {
	local records=()
	local max=0

	while IFS= read -r line; do
		push records "$line"
		local fields=()
		unquote -a fields "$line"
		[ "${#fields[@]}" -gt "$max" ] && max="${#fields[@]}"
	done

	for record in "${records[@]}"; do
		local fields=()
		unquote -a fields "$record"
		while [ "${#fields[@]}" -lt "$max" ]; do
			push fields ""
		done

		quote "${fields[@]}"
	done
}