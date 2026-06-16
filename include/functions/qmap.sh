qmap() {
	local body="$1"
	eval "__qmap_lambda() { $body; }"
	defer unset -f __qmap_lambda

	while read -q -a fields; do
		__qmap_lambda "${fields[@]}"
	done
}