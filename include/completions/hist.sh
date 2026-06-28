_hist_comp() {
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
    [pull]='sync history with the database (picks up commands from other sessions)'
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
		[import]='import history entries from another shell'
	)

	case $2 in
		--*)
			compadd -P '--' -A opts
			compadd -P '--' -A long_flags
		;;
		-*)
      compadd -P '--' -A opts
      compadd -P '--' -A long_flags
      compadd -P '-' -A short_flags
		;;
    *)
      case "$3" in
        hist)
          compadd -P '--' -A opts
          compadd -P '--' -A long_flags
          compadd -P '-' -A short_flags
        ;;
      esac
      ;;
	esac
}
complete -d -f -F _hist_comp hist
