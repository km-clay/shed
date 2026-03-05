_get_surround_target() {
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
	local _obj
	_read_obj
	_get_surround_target
	_KEYS="v$_obj"

}
_surround_2() {
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

	left="${_BUFFER:0:$start}"
	mid="${_BUFFER:$start:$delta}"
	right="${_BUFFER:$end}"
	_BUFFER="$left$_sl$mid$_sr$right"
	_CURSOR=$start

}
_surround_del() {
	_get_surround_target
	local left_buf="${_BUFFER:0:$_CURSOR}"
	local right_buf="${_BUFFER:$left}"
	local left=""
	local right=""
	_scan_left $_sl "$left_buf"

	if [ "$?" -ne 0 ]; then
	  _scan_right $_sl "$right_buf"

	  [ "$?" -ne 0 ] && return 1
	  left=$right
	fi

	mid_start=$((left + 1))
	right=""
	left_buf="${_BUFFER:0:$left}"
	right_buf="${_BUFFER:$mid_start}"
	_scan_right $_sr "$right_buf"

	[ "$?" -ne 0 ] && return 1

	mid_end=$((mid_start + right))
	right_start=$((mid_end + 1))
	new_left_buf="${_BUFFER:0:$left}"
	new_mid_buf="${_BUFFER:$mid_start:$right}"
	new_right_buf="${_BUFFER:$right_start}"


	_BUFFER="$new_left_buf$new_mid_buf$new_right_buf"

}
