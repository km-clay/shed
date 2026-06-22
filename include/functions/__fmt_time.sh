__fmt_size() {
	local -a units=(
		B
		KB
		MB
		GB
		TB
		PB
		EB
	)
	local size="$1"

	[ -i "$size" ] || raise "$0: expected integer, got '%1'" "$size"

	local remainder=0
	local -i divisions=0
	while (( size >= 1000 )); do
		remainder=$((size % 1000))
		size=$((size / 1000))
		divisions+=1
	done

	local unit="${units[divisions]}"
	local decimal="$(printf '%02d' $(( remainder / 10 )))"

	echo "$size.$decimal$unit"
}