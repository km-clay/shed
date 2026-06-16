# A CSV parser that decodes CSV input into shed's quoted-record protocol
#
# Reads the entire CSV from stdin and emits one shell-quoted record per
# CSV row to stdout, with fields encoded via the `quote` builtin. Implements
# RFC 4180: quoted fields containing commas, embedded newlines in quoted fields,
# and doubled-quote escaping.
#
# Note about the output structure: records are separated by newlines, and fields
# are separated by spaces. Spaces inside fields are always wrapped in quotes,
# newlines inside fields are always escaped into ansi-c strings.
#
# See `help quoted-streams` for more info on the structure.
#
# Output composes with the quote adapter functions and any line-oriented Unix tool,
# since each record is one line.
#
# e.g.
# quote_csv < tickets.csv \
#   | qwhere '[ "$4" = "high" ] || [ "$4" = "critical" ]' \
#   | qselect 1 2 6 \
#   | qmap 'echo "[$1] $2 - $3"'
#
# Note: reads the entire input into memory, be careful with huge files.

quote_csv() {
	local content=$(cat)
	local i=0
	local len=${#content}
	local state="outside"
	local buf=""
	local -a fields

	while (( i < len )); do
		local ch="${content:$i:1}"

		case "$state" in
			outside)
				case "$ch" in
					',') push fields "$buf"; buf="" ;;
					$'\n') push fields "$buf"; quote "${fields[@]}"; fields=(); buf="" ;;
					'"') state="quoted" ;;
					*) state="bare"; buf+="$ch";;
				esac
			;;
			bare)
				case "$ch" in
					',') fields+=("$buf"); buf=""; state="outside" ;;
					$'\n') fields+=("$buf"); quote "${fields[@]}"; fields=(); buf=""; state="outside" ;;
					*) buf+="$ch" ;;
				esac
			;;
			quoted)
				case "$ch" in
					'"') state="maybe_escape" ;;
					*) buf+="$ch" ;;
				esac
			;;
			maybe_escape)
				case "$ch" in
					'"') buf+='"'; state="quoted" ;;
					',') fields+=("$buf"); buf=""; state="outside" ;;
					$'\n') fields+=("$buf"); quote "${fields[@]}"; fields=(); buf=""; state="outside" ;;
					*) buf+="$ch"; state="bare" ;;
				esac
			;;
		esac

		(( i++ ))
	done

	if [ -n "$buf" ] || [ "${#fields[@]}" -gt 0 ]; then
		fields+=("$buf")
		quote "${fields[@]}"
	fi
}
