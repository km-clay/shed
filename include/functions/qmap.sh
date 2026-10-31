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
      name="${headers[i]}"

      # prevent stuff like command sub injection
      if ! [[ "$name" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]]; then
        raise "invalid variable name '%(1)'" "$name" --code 2
      fi

      # use take to move the positional args into named variables
      prelude+="take $((i + 1)) $name; "
    done
    body="$prelude$body"

		quote "${headers[@]}"
  fi

  # each record gets its own line
  while read -q -a record; do
    printf '%s\n' "$(call "$body" "${record[@]}")"
  done
}
