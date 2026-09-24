_hist_subcommands() {
  local -A c=(
    [pull]='sync history with the database (picks up commands from other sessions)'
    [branch]='list branches, or create one at the current position'
    [checkout]='switch to a history branch'
    [switch]='switch to a history branch'
    [merge]='merge another branch into the current one'
    [export]='dump the entire history as JSON for backup'
    [import]='restore a backup dump, or import another shell'\''s history'
  )
  compadd -A c
}

_hist_branches() {
  local b cands=() descs=()
  while IFS= read -r b; do
    # strip the "* "/"  " current-branch marker
    push cands "${b:2}"
    push descs 'branch'
  done < <(hist branch 2>/dev/null)
  compadd -a cands -d descs
}

_hist_query_flags() {
  local -A short_flags=(
    [n]='omit history entry numbers'
    [r]='reverse history entry order'
  )
  local -A long_flags=(
    [delete]='deletes history entries instead of printing'
    [ex]='query ex-mode history'
    [restore]='restores the most recent deletion'
    [count]='prints the number of matches'
    [not]='inverts the next query filter'
    [json]='output as json'
  )
  local -A opts=(
    [after]='history entries after a certain time, e.g. "10-08-2024" or "15 minutes ago", etc'
    [before]='history entries before a certain time, e.g. "last thursday" or "2 hours ago", etc'
    [lines-gt]='entries with more than N lines'
    [lines-lt]='entries with less than N lines'
    [ends-with]='entries that end with a substring'
    [contains]='entries that contain a substring'
    [starts-with]='entries that start with a substring'
    [matches]='entries that match a pattern'
    [duration-gt]='entries with a runtime duration longer than the one given'
    [duration-lt]='entries with a runtime duration shorter than the one given'
    [with-status]='entries with a specific exit status'
    [with-token]='entries with a specific uuid'
    [in-dir]='entries executed in a specific directory'
    [limit]='limits the number of entries to output'
  )
  case $1 in
    --*)
      compadd -P '--' -A opts
      compadd -P '--' -A long_flags
    ;;
    -*)
      compadd -P '--' -A opts
      compadd -P '--' -A long_flags
      compadd -P '-' -A short_flags
    ;;
  esac
}

_hist_comp() {
  local cur="$2"

  # find the subcommand: the first non-flag word after `hist`
  local i=1 cmd=""
  while (( i < COMP_CWORD )); do
    case "${COMP_WORDS[$i]}" in
      -*) let i=i+1 ;;
      *) cmd="${COMP_WORDS[$i]}"; break ;;
    esac
  done

  case "$cmd" in
    checkout|switch)
      case "$cur" in
        -*)
          local -A cshort=( [b]='create the branch if it does not exist, then switch' )
          local -A clong=( [orphan]='switch to a new empty branch, disconnected from existing history' )
          compadd -P '-' -A cshort
          compadd -P '--' -A clong
          return
        ;;
      esac
      _hist_branches
    ;;
    merge)
      _hist_branches
    ;;
    branch)
      case "$cur" in
        -*)
          local -A bshort=( [d]='delete a branch (must be fully merged)' [D]='force-delete a branch even if unmerged' )
          compadd -P '-' -A bshort
          return
        ;;
      esac
      # when deleting, complete existing branch names; a new name is free-form
      local w
      for w in "${COMP_WORDS[@]}"; do
        case "$w" in
          -d|-D) _hist_branches; return ;;
        esac
      done
    ;;
    export)
      # takes no arguments
    ;;
    import)
      case "$cur" in
        # a bare argument falls back to file completion (backup or history file)
        -*) compadd -P '-' f; compadd -P '--' force ;;
      esac
    ;;
    pull)
      case "$cur" in
        -*) compadd -P '--' ex ;;
      esac
    ;;
    *)
      # no subcommand yet: offer verbs, or query flags for the default listing
      case "$cur" in
        -*) _hist_query_flags "$cur" ;;
        *) _hist_subcommands ;;
      esac
    ;;
  esac
}
complete -d -f -F _hist_comp hist
