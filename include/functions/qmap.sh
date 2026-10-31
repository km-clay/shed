qmap() {
  defer eval "$(shopt set.localvars)"
  set -o localvars

  while getopts ":n" opt; do
    case "$opt" in
      n) has_names=1 ;;
    esac
  done
  shift $((OPTIND - 1))

  body="$1"

  if ((has_names)); then
    IFS= read -q -a headers

    for i in "${!headers[@]}"; do
      # bind positional args to named variables
      prelude+="${headers[i]}=\"\${$((i + 1))}\"; "
    done
    body="$prelude$body"

    printf '%s\n' "${headers[*]}"
  fi

  # each record gets its own line
  while read -q -a record; do
    printf '%s\n' "$(call "$body" "${record[@]}")"
  done
}
