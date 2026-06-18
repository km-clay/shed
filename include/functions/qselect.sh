qselect() {
	local -a requested
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))


	if ((has_names)); then
		local -a name_args=("$@")

		local -a headers
		read -q -a headers

		for arg in "${name_args[@]}"; do
			if [ -i "$arg" ]; then
				[ "$1" -eq 0 ] && raise "Field index out of bounds" -n "Fields are 1-indexed"
				push requested "$arg"
			else
				local found=0
				for i in ${!headers[@]}; do
					if [ "${headers[i]}" = "$arg" ]; then
						push requested "$((i + 1))"
						found=1
						break
					fi
				done
				((found == 0)) && raise "Field name not found: '%1'" "$arg"
			fi
		done

		local -a new_header
		for idx in "${requested[@]}"; do
			push new_header "${headers[idx - 1]}"
		done
		quote "${new_header[@]}"
	else
		while [ $# -gt 0 ]; do
			[ -i "$1" ] || raise "Expected an integer, found '%1'" "$1"
			[ "$1" -eq 0 ] && raise "Field index out of bounds" -n "Fields are 1-indexed"
			push requested "$1"
			shift
		done
	fi

	while read -q -a fields; do
		local -a new
		for idx in "${requested[@]}"; do
			[ "$idx" -gt "${#fields}" ] && raise "Field index out of bounds: '%1'" "$idx"
			push new "${fields[idx - 1]}"
		done
		quote "${new[@]}"
	done
}