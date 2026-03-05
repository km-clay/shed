_get_surround_target() {
	# get the delimiters to surround our selection with
	read_key -v _s_ch
	case "$_s_ch" in
	  \(|\)) _sl='('; _sr=')' ;;
	  \[|\]) _sl='['; _sr=']' ;;
	  \{|\}) _sl='{'; _sr='}' ;;
	  \<|\>) _sl='<'; _sr='>' ;;
	  *) _sl="$_s_ch"; _sr="$_s_ch" ;;
	esac

}
_read_obj() {
	# use the 'read_key' builtin to have our keymap function take user input
	# keep reading keys until we have something that looks like a text object
	_obj=""
	while read_key -v key; do
	  if [[ "${#_obj}" -ge 3 ]]; then return 1; fi
	  case "$key" in
	    i|a)
	      if [ -n "$_obj" ]; then return 1; fi
	      _obj="$key"
	      ;;
	    b|e)
	      if [ -n "$_obj" ]; then return 1; fi
	      _obj="$key"
	      break
	      ;;
	    w|W)
	      _obj="$_obj$key"
	      break
	      ;;
	    f|F)
	      read_key -v char
	      _obj="$key$char"
	      break
	    ;;
	    \(|\)|\[|\]|\{|\}|\"|\')
	      if [ -z "$_obj" ]; then return 1; fi
	      _obj="$_obj$key"
	      break
	      ;;
	  esac
	done

}
_scan_left() {
	# search to the left for a pattern in a string
	local needle="$1"
	local haystack="$2"
	local i=$((${#haystack} - 1))


	while [ "$i" -ge 0 ]; do
	  ch="${haystack:$i:1}"
	  if [ "$ch" = "$needle" ]; then
	    left=$i
	    return 0
	  fi
	  i=$((i - 1))
	done

	return 1

}
_scan_right() {
	# search to the right for a pattern in a string
	local needle="$1"
	local haystack="$2"
	local i=0


	while [ "$i" -lt "${#haystack}" ]; do
	  ch="${haystack:$i:1}"
	  if [ "$ch" = "$needle" ]; then
	    right="$i"
	    return 0
	  fi
	  i=$((i + 1))
	done

	return 1

}
_surround_1() {
	# here we get our text object and our delimiters
	local _obj
	_read_obj
	_get_surround_target

	# the $_KEYS variable can be used to send a sequence of keys
	# back to the editor. here, we prefix the text object with 'v'
	# to make the line editor enter visual mode and select the text object.
	_KEYS="v$_obj"

}
_surround_2() {
	# this is called after _surround_1. the editor received our visual
	# selection command, so now we can operate on the range it has selected.
	# $_ANCHOR and $_CURSOR can be used to find both sides of the selection
	local start
	local end
	if [ "$_ANCHOR" -lt "$_CURSOR" ]; then
	  start=$_ANCHOR
	  end=$_CURSOR
	else
	  start=$_CURSOR
	  end=$_ANCHOR
	fi
	end=$((end + 1))

	delta=$((end - start))

	# use parameter expansion to slice up the buffer into 3 parts
	left="${_BUFFER:0:$start}"
	mid="${_BUFFER:$start:$delta}"
	right="${_BUFFER:$end}"

	# slide our delimiters inbetween those parts
	_BUFFER="$left$_sl$mid$_sr$right"
	_CURSOR=$start

}
_surround_del() {
	# this one is pretty weird

	# get our delimiters
	_get_surround_target

	# slice the buffer in half at the cursor
	local left_buf="${_BUFFER:0:$_CURSOR}"
	local right_buf="${_BUFFER:$left}"
	local left=""
	local right=""

	# scan left to see if we find our left delimiter
	_scan_left $_sl "$left_buf"

	if [ "$?" -ne 0 ]; then
		# we didnt find $_sl to the left of the cursor
		# so let's look for it to the right of the cursor
	  _scan_right $_sl "$right_buf"

	  [ "$?" -ne 0 ] && return 1 # did not find it

		# we found the left delimiter to the right of the cursor.
		# _scan_right set the value of $right so lets take that and put it in $left
	  left=$right
	fi

	# this is the start of the middle part of the buffer
	mid_start=$((left + 1))
	right=""
	left_buf="${_BUFFER:0:$left}"
	right_buf="${_BUFFER:$mid_start}" # from mid_start to end of buffer
	_scan_right $_sr "$right_buf" # scan right

	[ "$?" -ne 0 ] && return 1

	# right now contains the distance we traveled to hit our right delimiter
	mid_end=$((mid_start + right)) # use that to calculate the end of the middle
	right_start=$((mid_end + 1)) # and get the start of the last part

	# and now we just slice it like we did in _surround_2
	new_left_buf="${_BUFFER:0:$left}"
	new_mid_buf="${_BUFFER:$mid_start:$right}"
	new_right_buf="${_BUFFER:$right_start}"

	# put them back together. the end result is a buffer without those pesky delimiters
	_BUFFER="$new_left_buf$new_mid_buf$new_right_buf"

}

# map our functions to some keys
# our savvy readers will notice that these are the same
# default keybinds set by kylechui's 'nvim-surround' plugin
keymap -n 'ds' '<CMD>!_surround_del<CR>'
keymap -n 'ys' '<CMD>!_surround_1<CR><CMD>!_surround_2<CR>' # chain these two together
