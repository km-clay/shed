qcount() {
	local fields=0
	while getopts ":f" opt; do
		case $opt in
			f) fields=1 ;;
			*) raise "Unrecognized option: '%1'" "$opt" ;;
		esac
	done

	if (( fields == 0 )); then
		local n=0
		while IFS= read -r line; do n=$((n+1)); done
		echo "$n"
	else
		while read -q -a fields; do
			echo "${#fields[@]}"
		done
	fi
}