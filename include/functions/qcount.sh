qcount() {
	local fields=0
	local has_names=0
	while getopts ":fn" opt; do
		case $opt in
			f) fields=1 ;;
			n) has_names=1 ;;
			*) raise "Unrecognized option: '%1'" "$opt" ;;
		esac
	done
	shift $((OPTIND - 1))

	if ((has_names)); then
		IFS= read -q -a headers # ignored
	fi

	if ((fields)); then
		while read -q -a fields; do
			echo "${#fields[@]}"
		done
	else
		local n=0
		while IFS= read -r line; do n=$((n+1)); done
		echo "$n"
	fi
}