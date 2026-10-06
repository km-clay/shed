qtable() {
	local -a widths records fields headers
	local left i=0 has_names=0
	local justify="${SQR_TABLE_JUSTIFY:-right}"
	local SQR_TABLE_MARKER="${SQR_TABLE_MARKER:-…}"

	while getopts ":nl" opt; do
		case "$opt" in
			n) has_names=1 ;;
			l) justify="left" ;;
		esac
	done
	shift $((OPTIND - 1))

	if ((has_names)); then
		shift

		IFS= read -q -a headers
	fi

	if [[ "$justify" == left ]]; then
		left=1
	else
		left=0
	fi

  # draw a row, taking a left, middle, and right separator character
  # e.g. `draw_separator '├' '┼' '┤'`
	draw_separator() {
		local left="$1"
		local middle="$2"
		local right="$3"

		printf '%s' "$left"

		for ((i=0; i<${#widths[@]}; i++)); do
			cell=$(( "${widths[i]}" + 2 ))

      # shed extension - 'r' repeats the character {width} times
      # In this case, "$cell" is used for the width, and '─' is repeated that many times.
			printf '%*r' "$cell" '─'

			if (( i < ${#widths[@]} - 1 )); then
				printf '%s' "$middle"
			fi
		done

		echo "$right"
	}

	draw_row() {
		local row=("$@")

		printf '│ '

		for ((col=0; col<${#widths[@]}; col++)); do
			field="${row[col]}"

			target_width=${widths[col]}
      (( $(len -w -- "$field") > target_width )) && field="$(str clip -w -m "$SQR_TABLE_MARKER" "$target_width" "$field")"

      this_width=$(len -w -- "$field")
			diff=$(( target_width - this_width ))

			((left)) && printf "%s" "$field"

			printf "%*s" "$diff" ""

			((left)) || printf "%s" "$field"

      if (( col == ${#widths[@]} - 1 )); then
        printf " │"
      else
        printf " │ "
      fi
		done

		printf "\n"
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

  # Shrink to fit the terminal. Each column costs its width plus two spaces of
  # padding, and the borders add one character per column plus one, so the
  # drawn width is the sum of the widths plus 3n+1.
	local budget="${SQR_TABLE_WIDTH:-${COLUMNS:-80}}"
	local min_col=3
	local total=$(( 3 * num_fields + 1 ))

	for ((i=0; i<num_fields; i++)); do
		total=$(( total + widths[i] ))
	done

  # Take from the widest column first, down to the next-widest, so narrow
  # columns keep their size and only the long field is truncated.
	while (( total > budget )); do
		local wi=0 w1=0 w2=0

		for ((i=0; i<num_fields; i++)); do
			if (( widths[i] > w1 )); then
				w2=$w1
				w1=${widths[i]}
				wi=$i
			elif (( widths[i] > w2 )); then
				w2=${widths[i]}
			fi
		done

		(( w1 <= min_col )) && break

		local floor=$min_col
		(( w2 > floor )) && floor=$w2

		local take=$(( w1 - floor ))
		local need=$(( total - budget ))

		(( take > need )) && take=$need
		(( take < 1 )) && take=1

		widths[wi]=$(( w1 - take ))
		total=$(( total - take ))
	done

	draw_separator '╭' '┬' '╮'

	if [ "${#headers[@]}" -gt 0 ]; then
		draw_row "${headers[@]}"
		draw_separator '├' '┼' '┤'
	fi

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
