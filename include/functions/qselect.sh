qselect() {
	local -a requested
	while [ $# -gt 0 ]; do
		[ -i "$1" ] || raise "Expected an integer, found '%1'" "$1"
		[ "$1" -eq 0 ] && raise "Field index out of bounds" -n "Fields are 1-indexed"
		push requested "$1"
		shift
	done
	while read -q -a fields; do
		local -a new
		for idx in "${requested[@]}"; do
			[ "$idx" -gt "${#fields}" ] && raise "Field index out of bounds: '%1'" "$idx"
			push new "${fields[idx - 1]}"
		done
		quote "${new[@]}"
	done
}