qname() {
	input=$(thru)

	quote "$@"
	echo "$input"
}