trim() {
	local parts=() s

  if [ "$#" -gt 0 ]; then
    while [ "$#" -gt 0 ]; do
      s="$1"
      s="${s#"${s%%[![:space:]]*}"}"
      s="${s%"${s##*[![:space:]]}"}"
      push parts "$s"
      shift
    done
    quote "${parts[@]}"
  elif ! [ -t 0 ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      line="${line#"${line%%[![:space:]]*}"}"
      line="${line%"${line##*[![:space:]]}"}"
      quote "$line"
    done
  else
    return 1
  fi
}
