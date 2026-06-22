_cd_comp() {
  defer eval "$(shopt core.nullglob)"
  shopt core.nullglob=true
  local word=${COMP_WORDS[$COMP_CWORD]}

  for match in ${word}*; do
    if [ -d "$match" ]; then
      basename=${match#"$dir"/}
      mode=$(stat "$match" -c '%A')
      compadd -D "$(printf '%-8s %s %s' "dir" "-" "$mode")" -S '/' "$basename"
    fi
  done
  local cdpath=$CDPATH
  while dir="${cdpath%%:*}"; cdpath="${cdpath#*:}"; do
    for match in "$dir/$word"*; do
      if [ -d "$match" ]; then
        basename=${match#"$dir"/}
        mode=$(stat "$match" -c '%A')
        compadd -D "$(printf '%-8s %s %s' "dir" "-" "$mode")" -S '/' "$basename"
      fi
    done
  done
  if [ -n "$cdpath" ]; then
    cdpath="${cdpath%/}"
    for match in "$cdpath/$word"*; do
      if [ -d "$match" ]; then
        basename=${match#"$dir"/}
        mode=$(stat "$match" -c '%A')
        compadd -D "$(printf '%-8s %s %s' "dir" "-" "$mode")" -S '/' "$basename"
      fi
    done
  fi
}
complete -F _cd_comp -d cd
