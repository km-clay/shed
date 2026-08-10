# This is the config I use for my status line.
#
# It makes use of a function called '__mod' that handles
# styling and separation of statusline components.
# There are left and right variants of this function.
#
# Each one emits content via "echo -n" or "echo -en" to build up
# the status line components one segment at a time
#
# The end result looks something like:
#
#  NORMAL  main ~3 +1 -2  󰔛 10ms -> [0]                       $shed v0.18.1
# └─mode─┘└───git───────┘└─cmd_status──┘                     └──version────┘
#           left side                                          right side

# these are nerd font glyphs if you cant see them
export LINE_SEP_LEFT=""
export LINE_SEP_RIGHT=""

# color palette
# the names don't really mean much
declare -A BG=(
  [mode]=33
  [path]=39
  [git]=76
  [time]="33;33;33"
  [stat]=18
)

declare -A FG=(
  [mode]=15
  [path]=15
  [git]=15
  [time]=15
  [stat]=15
)

# emit a "module". takes a palette key, the content, and a separator.
# 'sep' is assumed to be something like the separator chars exported above
# these characters are weird. we have to invert the current foreground and background
# in order for them to look correct. That's basically the main complexity of this whole thing.
__mod() {
	local key="$1" content="$2" sep="$3"
	__mod_dyn "${BG[$key]}" "${FG[$key]}" "$content" "$sep"
}

# the inner logic
# __mod is just a convenience wrapper for this
# this is where we flip the background/foreground colors
# for the separator. We start with emitting the previous segments separator
# and then print our content.
__mod_dyn() {
	local bg="$1" fg="$2" content="$3" sep="$4"
	[[ -z "$content" ]] && return

	if [[ -n "$__PREV_BG" ]]; then
	  __emit_fg "$__PREV_BG"; __emit_bg "$bg"
	  echo -n "$sep"
	fi
	__emit_bg "$bg"; __emit_fg "$fg"
	echo -n " $content "
	__PREV_BG="$bg"
}

# The "end" of the left side modules.
# Prints the previous separator and then exits
__cap() {
	local sep="$1" reset="$2"
	if [ -n "$__PREV_BG" ]; then
	  if [ -n "$reset" ]; then
	    __emit_bg "$reset"
	  else
	    echo -en "\e[49m"
	  fi
	  __emit_fg "$__PREV_BG"
	  echo -n "$sep"
	fi
	__PREV_BG="$reset"
}

# same deal as __mod, but on the right side
# The logic is different enough to warrant separate functions
__mod_right() {
	__mod_dyn_right "${BG[$1]}" "${FG[$1]}" "$2" "$LINE_SEP_RIGHT"
}

__mod_dyn_right() {
	local bg="$1" fg="$2" content="$3" sep="$4"
	[[ -z "$content" ]] && return
	if [[ -n "$__PREV_BG" ]]; then
	  # mid chain
	  __emit_fg "$bg"; __emit_bg "$__PREV_BG"
	  echo -n "$sep"
	else
    # print the leading separator
    # this is what '__cap' does for the left side
	  __emit_fg "$bg"
	  echo -en "\e[49m$sep"
	fi
	__emit_bg "$bg"; __emit_fg "$fg"
	echo -en "\e[1m $content "
	__PREV_BG="$bg"
}

# emitters for foreground and background colors. These handle both 256 color and true color values.
__emit_bg() {
	local val="$1"
	if [[ "$val" == *";"* ]]; then
    # if the value contains a semicolon, we assume it's a true color value in the form "R;G;B"
	  echo -en "\e[48;2;${val}m"
	else
    # otherwise, we assume it's a 256 color value
	  echo -en "\e[48;5;${val}m"
	fi

}

# same as above, but for foreground colors
__emit_fg() {
	local val="$1"
	if [[ "$val" == *";"* ]]; then
	  echo -en "\e[38;2;${val}m"
	else
	  echo -en "\e[38;5;${val}m"
	fi

}

# command status
# shows up as '󰔛 10ms -> [0]'
# where the boxed digit on the right is the exit status
# and the timer shows the command runtime.
# note: this checks $last_exit which is a local set in stat_line_left
cmd_status_line() {
	local last_cmd_stat
	local last_cmd_runtime
	if [[ "$last_exit" == "0" ]]; then
	  last_cmd_stat="\e[1;32m"
	else
	  last_cmd_stat="\e[1;31m"
	fi
	local last_runtime="$(echo -p "\t")"
	if [[ -z "$last_runtime" ]]; then
	  return 0
	else
	  last_cmd_runtime="\e[1;38;2;249;226;175m󰔛 ${last_cmd_stat}$(echo -p "\T")\e[39m"
	fi
	echo -en "$last_cmd_runtime \e[1m-> [${last_cmd_stat}${last_exit}\e[39m]"

}

# edit mode
# picks a different bg color per mode
emit_mode() {
	local bg fg=0
	case "$SHED_EDIT_MODE" in
	  NORMAL)                 bg=3 ;;
	  INSERT|"(insert)")      bg=6 ;;
	  COMMAND)                bg=2 ;;
	  VISUAL)                 bg=5 ;;
	  REPLACE|VERBATIM|EMACS) bg=1 ;;
	  SEARCH|REMOTE|COMPLETE) bg=7 ;;
	  *) return ;;
	esac
	echo -en "\e[1m"
	__mod_dyn "$bg" "$fg" "$SHED_EDIT_MODE" "$LINE_SEP_LEFT" "1"

}

# git info
# this one is *really* heavy, especially on network mounts
# so we have some caching and escape hatches going on.
git_stat_line() {
	if [[ -n "$GIT_STAT_DIR" ]] && [ "$PWD" = "${PWD#$GIT_STAT_DIR}" ]; then
    # GIT_STAT_DIR is set, but stripping it as a prefix left PWD unchanged, so
    # it is not a prefix of PWD: we are no longer in the git directory.
    # clear the cache and return
	  export GIT_STAT_LINE=""
	  export GIT_STAT_DIR=""
	  return
	fi
	if [[ -z "$GIT_STAT_LINE" ]] && [[ "${STATLINE_GIT:-0}" -eq 1 ]]; then
    # no cached line, statline_git is enabled
    # refresh the cache
    git_stat_line_update
	fi
  # no-op if statline_git is disabled
	echo -en "$GIT_STAT_LINE"

}
# now we set an autocmd to update the cache after every command
# this prevents the line from trying to update it on every redraw
autocmd post-cmd 'if [ "${STATLINE_GIT:=0}" -eq 1 ]; then git_stat_line_update; else export GIT_STAT_LINE=""; fi'


# I hope you like parameter expansion c:
git_stat_line_update() {
  # get the status, if this fails just return
	local status="$(git status --porcelain -b 2>/dev/null)" || return

  # the first line of 'git status -b' contains the branch info and the ahead/behind counts.
  # the rest of the lines contain file status info, which we also want to parse for signs of changes.
	local branch="" gitsigns="" ahead=0 behind=0
	local header="${status%%$'\n'*}" # split at line

	branch="${header#\#\# }" # remove the leading "## " from the branch info
	branch="${branch%%...*}" # remove any remote tracking info, which starts with "..."

  # the ahead/behind info is also in the header line, in the form "[ahead N]", "[behind N]", or "[ahead N, behind M]"
	case "$header" in
	    *ahead*)  ahead="${header#*ahead }"; ahead="${ahead%%[],]*}"; gitsigns="${gitsigns}↑" ;;
	esac
	case "$header" in
	    *behind*) behind="${header#*behind }"; behind="${behind%%[],]*}"; gitsigns="${gitsigns}↓" ;;
	esac

	case "$status" in
      # check unstaged changes
	    *$'\n'" "[MAR]*) gitsigns="${gitsigns}!" ;;
	esac
	case "$status" in
      # check untracked files
	    *$'\n'"??"*) gitsigns="${gitsigns}?" ;;
	esac
	case "$status" in
      # check deleted files
	    *$'\n'" "[D]*) gitsigns="${gitsigns}" ;;
	esac
	case "$status" in
      # check staged changes
	    *$'\n'[MADR]*) gitsigns="${gitsigns}+" ;;
	esac

  # and now we get our current deltas
	local diff="$(git diff --shortstat 2>/dev/null)"
	local changed="" add="" del=""

	if [ -n "$diff" ]; then
	  changed="${diff%% file*}"; changed="${changed##* }"
	  case "$diff" in *insertion*) add="${diff#*, }"; add="${add%% *}" ;; esac
	  case "$diff" in *deletion*) del="${diff% deletion*}"; del="${del##* }" ;; esac
	fi

  # and now we can finally construct our styled output
	if [[ -n "$branch" ]]; then
	  local out=" $branch"
	  [[ -n "$gitsigns" ]] && out="$out\e[38;5;9m[$gitsigns]"
	  [[ -n "$changed" ]] && [[ "$changed" -gt 0 ]] && out="$out \e[38;5;12m~$changed\e[39m"
	  [[ -n "$add" ]] && [[ "$add" -gt 0 ]] && out="$out \e[38;5;10m+$add\e[39m"
	  [[ -n "$del" ]] && [[ "$del" -gt 0 ]] && out="$out \e[38;5;9m-$del\e[39m"

    # cache the line and directory
	  export GIT_STAT_LINE="\e[1;38;5;13m$out"
	  export GIT_STAT_DIR="$(git rev-parse --show-toplevel 2>/dev/null)"
	fi

}

# just styles the output of version -v a little bit
shed_ver() {
	echo -en "\e[1;36m\$$(version -v)"
}

# the actual left-side status line function
# the output of this is what gets expanded in shopt statline.left_string
stat_line_left() {
	local last_exit="$?"
	__PREV_BG=""
	emit_mode && \
	__mod time "$(git_stat_line)" $LINE_SEP_LEFT && \
	__mod stat "$(cmd_status_line)" $LINE_SEP_LEFT && \
	__cap $LINE_SEP_LEFT "18"
}

# right-side status line functionn
# you may notice that we hardcode __PREV_BG to the arg passed in __cap
# in stat_line_left. this lets us inherit the 'final color' of the left side
stat_line_right() {
	__PREV_BG="18"
	__mod_right time "$(shed_ver)"

}

# and finally, set the status line strings to point to our functions
# the '\@' prompt escape expands to the output of a given function
shopt statline.left_string='\@stat_line_left'
shopt statline.middle_string='' # unused
shopt statline.right_string='\@stat_line_right'

shopt statline.enable=true # and we're done here
