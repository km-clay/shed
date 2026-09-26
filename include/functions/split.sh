split() {
  local USAGE=("Usage: split <%1> [<%2>] " 'pattern' 'string')
	[ "$#" -ge 1 ] || raise "${USAGE[@]}"

	local pat="$1" parts part input line

  while getopts ":0" opt; do
    case "$opt" in
      0) pat=$'\0' ;;
      *) raise "${USAGE[@]}";;
    esac
  done

  if [ -z "$pat" ]; then
    raise "${USAGE[@]}"
  fi

  if [ -n "$2" ]; then
    input="${2%"$pat"}"
    [ -n "$input" ] || return 0

    while true; do
      part="${input%%"${pat}"*}"
      push parts "$part"

      [ "$part" = "$input" ] && break

      input="${input#*"${pat}"}"
    done

    quote "${parts[@]}"
    return 0
  fi

  if ! [ -t 0 ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      line="${line%"$pat"}"
      [ -n "$line" ] || continue

      parts=()
      while true; do
        part="${line%%"${pat}"*}"
        push parts "$part"

        [ "$part" = "$line" ] && break

        line="${line#*"${pat}"}"
      done

      quote "${parts[@]}"
    done
    return 0
  fi
}
