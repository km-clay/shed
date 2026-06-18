qtable() {
	local -a widths
	local -a records
	local -a fields
	local -a headers
	local i=0
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))

	if ((has_names)); then
		shift

		IFS= read -q -a headers
	fi


	draw_separator() {
		local left="$1"
		local middle="$2"
		local right="$3"

		echo -n "$left"

		for ((i=0; i<${#widths[@]}; i++)); do
			cell=$(( "${widths[i]}" + 2 ))

			for ((j=0; j<cell; j++)); do
				echo -n '─'
			done

			if (( i < ${#widths[@]} - 1 )); then
				echo -n "$middle"
			fi
		done

		echo "$right"
	}

	draw_row() {
		local row=("$@")

		echo -n '│ '

		for ((col=0; col<${#row[@]}; col++)); do
			field="${row[col]}"
			this_width=$(width "$field")
			target_width=${widths[col]}

			diff=$(( target_width - this_width ))

			for ((pad=0; pad<diff; pad++)); do
				echo -n ' '
			done

			echo -n "$field │ "
		done

		echo
	}

	record_widths() {
		local row=("$@")

		for ((col=0; col < ${#row[@]}; col++)); do
			field="${row[col]}"
			field_width=$(width "$field")

			if [ -z "${widths[col]}" ] || (( field_width > "${widths[col]}" )); then
				widths[col]=$field_width
			fi
		done
	}
	defer unset -f draw_separator
	defer unset -f draw_row
	defer unset -f record_widths

	if [ "${#headers[@]}" -gt 0 ]; then
		record_widths "${headers[@]}"
	fi

	while read -r line; do
		push records "$line"
		unquote -a fields "$line"

		record_widths "${fields[@]}"
	done

	local num_records="${#records}"
	local num_fields="${#widths}"
	local approx_height=$(( num_records + 6 ))
	headers=( "${headers[@]:0:$num_fields}"  )

	draw_separator '╭' '┬' '╮'

	if [ "${#headers[@]}" -gt 0 ]; then
		draw_row "${headers[@]}"
		draw_separator '├' '┼' '┤'
	fi

	local record_no=0
	for record in "${records[@]}"; do
		unquote -a fields "$record"
		draw_row "${fields[@]}"
	done

	if [ "${#headers[@]}" -gt 0 ] && (( approx_height > LINES )); then
		draw_separator '├' '┼' '┤'
		draw_row "${headers[@]}"
	fi

	draw_separator '╰' '┴' '╯'
}