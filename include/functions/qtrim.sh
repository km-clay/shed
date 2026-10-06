qtrim() {
  local record t f
  local -a fields out

  while IFS= read -r record; do
    unquote -a fields "$record"
    out=()
    for f in "${fields[@]}"; do
      str trim "$@" "$f" >@t
      push out "$t"
    done
    quote "${out[@]}"
  done
}
