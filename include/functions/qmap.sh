qmap() {
	local has_names=0

	while getopts ":n" opt; do
		case "$opt" in
			n) has_names=1 ;;
		esac
	done
	shift $((OPTIND - 1))

	local body="$1"

	if ((has_names)); then
		local prelude=""
		IFS= read -q -a headers
		for i in "${!headers[@]}"; do
			prelude+="local ${headers[i]}=\"\${$((i+1))}\"; "
		done
		eval "__qmap_lambda() { $prelude $body; }"
	else
		eval "__qmap_lambda() { $body; }"
	fi

	defer unset -f __qmap_lambda

	local buf=""
	local first=1
	while read -q -a record; do
		if (( first == 0 )); then
			buf+=$'\n'
		else
			first=0
		fi
		buf+="$(__qmap_lambda "${record[@]}")"
	done

	if ((has_names)); then
		echo "${headers[@]}"
	fi

	echo "$buf"
}