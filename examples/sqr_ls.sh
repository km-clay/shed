qls() {
	# 'ls' output formatted in SQR
	
	emit_fields() {
		quote "${fields[0]}" \
			"${fields[1]}" \
			"${fields[2]}" \
			"${fields[*]:3:3}" \
			"${fields[*]:6}"
	}
	defer unset -f emit_fields

	local buf=""
	local first=0
	ls -l | while read -q -a fields; do
		if (( first == 0 )); then
			buf+=$(emit_fields)
			first=1
			else
			buf+=$'\n'
			buf+=$(emit_fields)
		fi
	done

	echo "$buf" | qname mode size user date name | __emit_sqr -n
}