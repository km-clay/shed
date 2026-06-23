# Scans COMP_WORDS for the first word that is a known subcommand. Expects the
# verb set in an associative array named 'subcommands' in the caller's scope.
# On a match, emits an SQR record of the word and its 1-based index, returns 0.
__find_subcmd() {
	local -i i=1
	for word in "${COMP_WORDS[@]:1}"; do
		if [[ -n "${subcommands[$word]+x}" ]]; then
			quote "$word" "$i"
			return 0
		fi
		i+=1
	done
	return 1
}