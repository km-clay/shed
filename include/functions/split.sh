split() {
	[ "$#" -ge 1 ] || raise "Usage: split <%1> [<%2>] " 'pattern' 'string'
	local pat parts part

  while getopts ":0" opt; do
    case "$opt" in
      0) pat=$'\0' ;;
      *) raise "Usage: split <%1> [<%2>] " 'pattern' 'string' ;;
    esac
  done
  if [ -z "$pat" ]; then
    pat="$1"
  fi

  if ! [ -t 0 ]; then
    while IFS= read -r parts || [ -n "$parts" ]; do
      # strip all trailing delimiters
      while [ "$parts" != "${parts%"${pat}"}" ]; do parts="${parts%"${pat}"}"; done

      parts="${parts}${pat}" # attaches a delimiter to the end

      while [ -n "$parts" ]; do
        part="${parts%%"${pat}"*}"
        parts="${parts#*"${pat}"}"
        quote "$part"
      done
    done
  elif [ -z "$2" ]; then
    return
  else
    parts="$2"
    # strip all trailing delimiters
    while [ "$parts" != "${parts%"${pat}"}" ]; do parts="${parts%"${pat}"}"; done

    parts="${parts}${pat}" # attaches a delimiter to the end

    while [ -n "$parts" ]; do
      part="${parts%%"${pat}"*}"
      parts="${parts#*"${pat}"}"
      quote "$part"
    done
  fi

}
