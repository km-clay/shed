# This is the code for the prompt I currently use
# It makes use of the '\@funcname' function expansion escape sequence
# and the '-p' flag for echo which expands prompt escape sequences
#
# The final product looks like this:
# ┏━ user@hostname INSERT
# ┃ 󰔛 1ms
# ┃  main[!?] ~1 +1 -1
# ┃ 󰒓 1 job(s) running
# ┣━━ ~/path/to/pwd/
# ┗━ $                                                       $shed 0.5.0 (x86_64 linux)
# (The vi mode indicator is styled to match the color of the separators)

prompt() {
	local statline="$(prompt_stat_line)"
	local topline="$(prompt_topline)"
	local jobsline="$(prompt_jobs_line)"
	local sshline="$(prompt_ssh_line)"
	local pwdline="$(prompt_pwd_line)"
	local dollarline="$(prompt_dollar_line)"
	local prompt="$topline$statline$PROMPT_GIT_LINE$jobsline$sshline$pwdline\n$dollarline"

	echo -en "$prompt"

}
prompt_dollar_line() {
	local dollar="$(echo -p "\$ ")"
	local dollar="$(echo -e "\e[1;32m$dollar\e[0m")"
	echo -n "\e[1;34m┗━ $dollar "

}
prompt_git_line() {
	# git is really expensive so we've gotta make these calls count

	# get the status
	local status="$(git status --porcelain -b 2>/dev/null)" || return

	local branch="" gitsigns="" ahead=0 behind=0
	# split at the first linebreak
	local header="${status%%$'\n'*}"

	# cut the '## ' prefix
	branch="${header#\#\# }"
	#	cut the '..' suffix
	branch="${branch%%...*}"

	# parse ahead/behind counts
	case "$header" in
	    *ahead*)  ahead="${header#*ahead }"; ahead="${ahead%%[],]*}"; gitsigns="${gitsigns}↑" ;;
	esac
	case "$header" in
	    *behind*) behind="${header#*behind }"; behind="${behind%%[],]*}"; gitsigns="${gitsigns}↓" ;;
	esac

	# grab gitsigns
	case "$status" in
		# unstaged changes
		*$'\n'" "[MAR]*) gitsigns="${gitsigns}!" ;;
	esac
	case "$status" in
		# untracked files
		*$'\n'"??"*) gitsigns="${gitsigns}?" ;;
	esac
	case "$status" in
		# deleted files
		*$'\n'" "[D]*) gitsigns="${gitsigns}" ;;
	esac
	case "$status" in
		# staged changes
		*$'\n'[MADR]*) gitsigns="${gitsigns}+" ;;
	esac

	# unfortunately we need one more git fork
	local diff="$(git diff --shortstat 2>/dev/null)"

	local changed="" add="" del=""
	if [ -n "$diff" ]; then
		changed="${diff%% file*}"; changed="${changed##* }"
		case "$diff" in
			*insertion*) add="${diff#*, }"; add="${add%% *}" ;;
		esac
		case "$diff" in
			*deletion*) del="${diff% deletion*}"; del="${del##* }" ;;
		esac
	fi

	if [ -n "$gitsigns" ] || [ -n "$branch" ]; then
		# style gitsigns if not empty
		[ -n "$gitsigns" ] && gitsigns="\e[1;31m[$gitsigns]"
		# style changed/deleted/added text
		[ -n "$changed" ] && [ "$changed" -gt 0 ] && changed="\e[1;34m~$changed \e[0m"
		[ -n "$add" ] && [ "$add" -gt 0 ] && add="\e[1;32m+$add \e[0m"
		[ -n "$del" ] && [ "$del" -gt 0 ] && del="\e[1;31m-$del\e[0m"

		# echo the final product
		echo -n "\e[1;34m┃ \e[1;35m $branch$gitsigns\e[0m $changed$add$del\n"
	fi

}
prompt_jobs_line() {
	local job_count="$(echo -p '\j')"
	if [ "$job_count" -gt 0 ]; then
	  echo -n "\e[1;34m┃ \e[1;33m󰒓 $job_count job(s) running\e[0m\n"
	fi

}
prompt_mode() {
	local mode=""
	local normal_fg='\e[0m\e[30m\e[1;43m'
	local normal_bg='\e[0m\e[33m'
	local insert_fg='\e[0m\e[30m\e[1;46m'
	local insert_bg='\e[0m\e[36m'
	local command_fg='\e[0m\e[30m\e[1;42m'
	local command_bg='\e[0m\e[32m'
	local visual_fg='\e[0m\e[30m\e[1;45m'
	local visual_bg='\e[0m\e[35m'
	local replace_fg='\e[0m\e[30m\e[1;41m'
	local replace_bg='\e[0m\e[31m'
	local search_fg='\e[0m\e[30m\e[1;47m'
	local search_bg='\e[0m\e[39m'
	local complete_fg='\e[0m\e[30m\e[1;47m'
	local complete_bg='\e[0m\e[39m'

	# shed exposes it's current vi mode as a variable
	case "$SHED_VI_MODE" in
	  "NORMAL")
	    mode="$normal_bg${normal_fg}NORMAL$normal_bg\e[0m"
	  ;;
	  "INSERT")
	    mode="$insert_bg${insert_fg}INSERT$insert_bg\e[0m"
	  ;;
	  "COMMAND")
	    mode="$command_bg${command_fg}COMMAND$command_bg\e[0m"
	  ;;
	  "VISUAL")
	    mode="$visual_bg${visual_fg}VISUAL$visual_bg\e[0m"
	  ;;
	  "REPLACE")
	    mode="$replace_bg${replace_fg}REPLACE$replace_bg\e[0m"
	  ;;
	  "VERBATIM")
	    mode="$replace_bg${replace_fg}VERBATIM$replace_bg\e[0m"
	  ;;
	  "COMPLETE")
	    mode="$complete_bg${complete_fg}COMPLETE$complete_bg\e[0m"
	  ;;
	  "SEARCH")
	    mode="$search_bg${search_fg}SEARCH$search_bg\e[0m"
	  ;;
	  *)
	    mode=""
	  ;;
	esac

	echo -en "$mode\n"

}
prompt_pwd_line() {
	# the -p flag exposes prompt escape sequences like '\W'
	echo -p "\e[1;34m┣━━ \e[1;36m\W\e[1;32m/"

}
prompt_ssh_line() {
	local ssh_server="$(echo $SSH_CONNECTION | cut -f3 -d' ')"
	[ -n "$ssh_server" ] && echo -n "\e[1;34m┃ \e[1;39m🌐 $ssh_server\e[0m\n"

}
prompt_stat_line() {
	local last_exit_code="$?"
	local last_cmd_status
	local last_cmd_runtime
	if [ "$last_exit_code" -eq "0" ]; then
	  last_cmd_status="\e[1;32m"
	else
	  last_cmd_status="\e[1;31m"
	fi
	local last_runtime_raw="$(echo -p "\t")"
	if [ -z "$last_runtime_raw" ]; then
	  return 0
	else
	  last_cmd_runtime="\e[1;38;2;249;226;175m󰔛 ${last_cmd_status}$(echo -p "\T")\e[0m"
	fi

	echo -n "\e[1;34m┃ $last_cmd_runtime\e[0m\n"

}
prompt_topline() {
	local user_and_host="\e[0m\e[1m$USER\e[1;36m@\e[1;31m$HOST\e[0m"
	local mode_text="$(prompt_mode)"
	echo -n "\e[1;34m┏━ $user_and_host $mode_text\n"

}
shed_ver() {
	shed --version

}

export PS1="\@prompt "
# PSR is the text that expands on the right side of the prompt
export PSR='\e[36;1m$\@shed_ver\e[0m'
