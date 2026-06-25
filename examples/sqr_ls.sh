# qls - `ls -l` rendered as an aligned, bordered table (a "square ls")
#
# Pipes a long directory listing through shed's quoted-record tooling to turn
# the output `ls -l` into a clean table. The data flows like this:
#
#   command ls -l | tail -n +2
#       Raw long listing, with the leading "total N" line stripped off.
#
#   vice --lines --quoted --sep 'W' -c 'viW' -m 'W' -r 2:1 -c 'viW' -c 'viWEE' -c '$'
#       Uses a `vice` program to extract fields using shed's text editing engine.
#
#       Arguments explained by order of appearance:
#       '--lines': executes the program on each line of the input, instead of once on the entire input
#       `--quoted`: fields extracted using `-c` are packed in shell quotes
#       `--sep 'W'`: after each command, the editor will execute 'W', moving forward by one WORD.
#       `-c 'viW`: uses visual mode to select the WORD under the cursor. visual mode selections are used
#         for field extractions if the editor finishes the command in visual mode.
#       `-m 'W'`: moves forward by one WORD. This effectively skips a field (the links count)
#         because the --sep motion is executed afterward.
#       `-r 2:1`: repeats the last two commands one time.
#         performing the same two commands extracts the user field, and then skips the group field.
#       `-c 'viW'`: at this point, the cursor is on the size field. this motion selects the entire
#         number using visual mode and the WORD text object.
#       `-c 'viWEE'`: now the cursor is on the first word of the date field. dates in ls -l look
#         like this: "Apr 7 11:07", three space-separated words. 'viWEE' selects the current word,
#         then extends the selection to include the next two words.
#       `-c '$'`: the '$' motion moves to the end of the line. After the --sep motion, the cursor
#         ends up on the start of the filename. Since we can't know the structure of the filename,
#         we just select everything between the cursor and the end of the line.
#
#       This is executed on each line, emitting quoted records that look like this:
#         `perf.data 75299588 pagedmov 'Jun 22 15:42' -rw-------`
#       Note the shell-quoted date field. This output is ready to be used by SQR consumers.
#
#   qname mode user size date name
#       Names the five fields, prepending a header record so later stages can
#       refer to columns by name instead of position.
#
#   qselect -n name size user date mode
#       Picks and reorders columns by name (here: swaps mode and filename, swaps size and user).
#       `-n` tells qselect that the records are named.
#
#   __emit_sqr -n
#       Renders the result. To a terminal it draws the bordered table; when the
#       output is piped it instead passes the raw quoted records straight
#       through (it branches on `[ -t 1 ]`), so `qls | ...` stays machine-
#       readable and composes with the rest of the `q*` tools.
#       `-n` again marks the input as named-records.
#
# SQR_TABLE_JUSTIFY controls cell alignment ("left"/"right"/"center"); the
# `qtable` renderer behind `__emit_sqr` reads it via dynamic scoping.
#
# See `help quoted-streams` for the record protocol these tools share.

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
