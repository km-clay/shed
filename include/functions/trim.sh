trim() {
	local parts=()
	while [ "$#" -gt 0 ]; do
		parts+=$(quote $1)
		shift
	done
	quote "${parts[@]}"
}