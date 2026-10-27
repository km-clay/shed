qmap() {
  defer eval "$(shopt set.localvars)"
  set -o localvars

  has_names=0

  while getopts ":n" opt; do
    case "$opt" in
      n) has_names=1 ;;
    esac
  done
  shift $((OPTIND - 1))

  body="$1"

  # save any existing lambdas so qmap can nest
  declare -f __qmap_lambda >@__qmap_lambda_old
  # restores on exit
  defer if [ -z "$__qmap_lambda_old" ]; then
    unset -f __qmap_lambda
  else
    eval "$__qmap_lambda_old"
  fi

  if ((has_names)); then
    prelude=""

    IFS= read -q -a headers

    for i in "${!headers[@]}"; do
      # bind positional args to named variables
      prelude+="${headers[i]}=\"\${$((i + 1))}\"; "
    done

    eval "__qmap_lambda() { $prelude $body; }"
  else
    eval "__qmap_lambda() { $body; }"
  fi

  buf="" first=1

  # each record gets its own line
  while read -q -a record; do
    buf+="$(__qmap_lambda "${record[@]}")"
    buf+=$'\n'
  done

  if ((has_names)); then
    printf '%s\n%s' "${headers[*]}" "$buf"
  else
    printf '%s' "$buf"
  fi

}
