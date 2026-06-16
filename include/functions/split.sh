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
    while IFS= read -r parts; do
      while parts="${parts%"${pat}"}"; do :; done # strips all trailing delimiters

      parts="${parts}${pat}" # attaches a delimiter to the end

      while part="${parts%%"${pat}"*}" && parts="${parts#*"${pat}"}"; do
        quote "$part";
      done
    done
  elif [ -z "$2" ]; then
    return
  else
    parts="$2"
    while parts="${parts%"${pat}"}"; do :; done # strips all trailing delimiters

    parts="${parts}${pat}" # attaches a delimiter to the end

    while part="${parts%%"${pat}"*}" && parts="${parts#*"${pat}"}"; do
      quote "$part";
    done
  fi

}
