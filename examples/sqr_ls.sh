qls() {
	local SQR_TABLE_JUSTIFY="left"

	command ls -l | tail -n +2 \
		| vice --lines --quoted --sep 'W' \
			-c 'viW' \
			-m 'W' \
			-r 2:1 \
			-c 'viW' \
			-c 'viWEE' \
			-c '$' \
		| qname mode user size date name \
		| qselect -n name size user date mode \
		| __emit_sqr -n
}