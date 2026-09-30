# My current `shedrc` as of 09-30-26
#
# I use the nix module defined in ../nix/module.nix, so this file is technically generated from my nix config.
# A lot of the stuff in here relies on scripts defined in my nix config, so it should be used as more of a reference
# than something to directly use. It won't work on your machine.
#
# my actual nix config can be found here if you want to see how I have my shed config set up
# https://github.com/km-clay/nixos-config/tree/main/modules/home/environment/shed

export BROWSER="firefox"
export FLAKEPATH="$HOME/.sysflake"
export LANG="en_US.UTF-8"
export PAGER="less"
export LESS="-R"
export LINE_SEP_LEFT=""  # status line separators
export LINE_SEP_RIGHT=""
export SCRY_DEFAULT_CMD="fd"
export SHED_CONFIG_HOME="$HOME/.config/shed"
export SHED_DATA_HOME="$HOME/.local/share/shed"
export SOUNDS_ENABLED="true"
export STATLINE_GIT="1"
export STEAMPATH="$HOME/.local/share/Steam"
export PATH="$PATH:$HOME/.cargo/bin"
export PS1="\@prompt"

alias cfgfilecount='find ".\.nix" $FLAKEPATH | wc -l | toilet -f 3d | lolcat'
alias cg=cargo
alias cp='cp -vr'
alias diff='diff --color=auto'
alias ga='playshellsound gitadd.wav; git add'
alias gcomm=gitcommit_sfx
alias gpull=gitpull_sfx
alias gpush=gitpush_sfx
alias grebase=gitrebase_sfx
alias gt=gtrash
alias gtp='playshellsound rm.wav && gtrash put'
alias iv=invoke
alias mix=pavucontrol
alias mkdir='mkdir -p'
alias mkexe='chmod +x'
alias mv='mv -v'
alias nix-shell='command nix-shell --command '\''exec shed'\'''
alias pk='pkill -9 -f'
alias psg='ps aux | grep -v grep | grep -i -e VSZ -e'
alias rustdev='nix develop github:km-clay/devshells#rust'
alias shortdate='date +%m-%d-%y'
alias sr='source $XDG_CONFIG_HOME/shed/shedrc'
alias suvi=sudoedit
alias suvide='EDITOR=neovide suvi'
alias svc='sudo systemctl'
alias svcu='systemctl --user'
alias vi=nvim
alias vide=neovide
alias videconf='EDITOR=neovide viconf'
alias y=yazi

excmd brace='normal!ggO{<ESC>Go}<ESC>v%='
excmd subsh='normal!ggO(<ESC>Go)<ESC>v%='

shopt line.viewport_height=100%
shopt line.scroll_offset=2
shopt line.tab_width=4
shopt line.linebreak_on_incomplete=true
shopt line.line_numbers=true
shopt line.auto_indent=true
shopt line.trim_on_submit=true
shopt line.auto_suggest=true
shopt core.dotglob=false
shopt core.nullglob=false
shopt core.autocd=true
shopt core.interactive_comments=true
shopt core.bell_enabled=true
shopt core.max_recurse_depth=1000
shopt core.xpg_echo=false
shopt core.compact_errors=true
shopt core.fork_trace=false
shopt core.max_read_limit=1gib
shopt core.pipeline_style='thread'
shopt core.lastpipe=true
shopt history.auto_save=true
shopt history.ignore_dupes=true
shopt history.ignore_space=true
shopt history.max_entries=-1
shopt history.enable_concat=true
shopt set.hashall=true
shopt set.vi=true
shopt set.allexport=false
shopt set.errexit=false
shopt set.ignoreeof=false
shopt set.noclobber=false
shopt set.monitor=true
shopt set.noglob=false
shopt set.noexec=false
shopt set.nolog=false
shopt set.notify=false
shopt set.nounset=false
shopt set.verbose=false
shopt set.xtrace=false
shopt set.pipefail=false
shopt prompt.leader='<Space>'
shopt prompt.trunc_prompt_path=4
shopt prompt.comp_limit=1000
shopt prompt.idle_timeout=10
shopt prompt.completion_ignore_case=true
shopt prompt.complete_style=fuzzy
shopt prompt.expand_aliases=false
shopt prompt.substitute=true
shopt statline.enable=true
shopt statline.left_string='\@stat_line_left'
shopt statline.middle_string='\e[39;2m$EDITOR_FILE\e[22m'
shopt statline.right_string='\@stat_line_right'
shopt highlight.enable="true"
shopt highlight.string="yellow"
shopt highlight.keyword="yellow"
shopt highlight.external_command="green"
shopt highlight.function="green"
shopt highlight.alias="green"
shopt highlight.builtin="green"
shopt highlight.directory="green"
shopt highlight.invalid_command="bold red"
shopt highlight.control_flow_keyword="magenta"
shopt highlight.argument="white"
shopt highlight.argument_file="underline white"
shopt highlight.variable="cyan"
shopt highlight.operator="bold magenta"
shopt highlight.comment="italic bright black"
shopt highlight.glob="bright cyan"

# stat line palette
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
  [stat]=18
)

# status line functions

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
__emit_bg() {
	local val="$1"
	if [[ "$val" == *";"* ]]; then
	  echo -en "\e[48;2;${val}m"
	else
	  echo -en "\e[48;5;${val}m"
	fi

}
__emit_fg() {
	local val="$1"
	if [[ "$val" == *";"* ]]; then
	  echo -en "\e[38;2;${val}m"
	else
	  echo -en "\e[38;5;${val}m"
	fi

}
__mod() {
	local key="$1" content="$2" sep="$3"
	__mod_dyn "${BG[$key]}" "${FG[$key]}" "$content" "$sep"

}
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
__mod_dyn_right() {
	local bg="$1" fg="$2" content="$3" sep="$4"
	[[ -z "$content" ]] && return
	if [[ -n "$__PREV_BG" ]]; then
	  # mid chain
	  __emit_fg "$bg"; __emit_bg "$__PREV_BG"
	  echo -n "$sep"
	else
	  __emit_fg "$bg"
	  echo -en "\e[49m$sep"
	fi
	__emit_bg "$bg"; __emit_fg "$fg"
	echo -en "\e[1m $content "
	__PREV_BG="$bg"

}
__mod_right() {
	__mod_dyn_right "${BG[$1]}" "${FG[$1]}" "$2" "$LINE_SEP_RIGHT"

}
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
	local edit_mode=$'\e'"[1m$SHED_EDIT_MODE"
	echo -en "\e[1m"
	__mod_dyn "$bg" "$fg" "$edit_mode" "$LINE_SEP_LEFT" "1"

}
git_stat_line() {
	if [[ -n "$GIT_STAT_DIR" ]] && [ "$PWD" = "${PWD#$GIT_STAT_DIR}" ]; then
	  export GIT_STAT_LINE=""
	  export GIT_STAT_DIR=""
	  return
	fi
	if [[ -z "$GIT_STAT_LINE" ]]; then
	  if [[ "${STATLINE_GIT:-0}" -eq 1 ]]; then
	    git_stat_line_update
	  fi
	fi
	echo -en "$GIT_STAT_LINE"

}
git_stat_line_update() {
	local status="$(git status --porcelain -b 2>/dev/null)" || return
	local diff="$(git diff --shortstat 2>/dev/null)"
	local cache_key="$status"$'\n'"$diff"
	if [[ -n "$LAST_DIFF" ]] && [[ "$LAST_DIFF" == "$cache_key" ]]; then
	  # its the same as last time. lets not do all this stuff actually
	  return 0
	fi
	LAST_DIFF="$cache_key"

	local branch="" gitsigns="" ahead=0 behind=0
	local header="${status%%$'\n'*}"

	# hope you like parameter expansion
	branch="${header#\#\# }"
	branch="${branch%%...*}"
	case "$header" in
	    *ahead*)  ahead="${header#*ahead }"; ahead="${ahead%%[],]*}"; gitsigns="${gitsigns}↑" ;;
	esac
	case "$header" in
	    *behind*) behind="${header#*behind }"; behind="${behind%%[],]*}"; gitsigns="${gitsigns}↓" ;;
	esac

	case "$status" in
	    *$'\n'" "[MAR]*) gitsigns="${gitsigns}!" ;;
	esac
	case "$status" in
	    *$'\n'"??"*) gitsigns="${gitsigns}?" ;;
	esac
	case "$status" in
	    *$'\n'" "[D]*) gitsigns="${gitsigns}" ;;
	esac
	case "$status" in
	    *$'\n'[MADR]*) gitsigns="${gitsigns}+" ;;
	esac

	local changed="" add="" del=""
	if [ -n "$diff" ]; then
	  changed="${diff%% file*}"; changed="${changed##* }"
	  case "$diff" in *insertion*) add="${diff#*, }"; add="${add%% *}" ;; esac
	  case "$diff" in *deletion*) del="${diff% deletion*}"; del="${del##* }" ;; esac
	fi

	if [[ -n "$branch" ]]; then
	  local out=" $branch"
	  [[ -n "$gitsigns" ]] && out="$out\e[38;5;9m[$gitsigns]"
	  [[ -n "$changed" ]] && [[ "$changed" -gt 0 ]] && out="$out \e[38;5;12m~$changed\e[39m"
	  [[ -n "$add" ]] && [[ "$add" -gt 0 ]] && out="$out \e[38;5;10m+$add\e[39m"
	  [[ -n "$del" ]] && [[ "$del" -gt 0 ]] && out="$out \e[38;5;9m-$del\e[39m"
	  export GIT_STAT_LINE="\e[1;38;5;13m$out"
	  export GIT_STAT_DIR="$(git rev-parse --show-toplevel 2>/dev/null)"
	fi

}
prompt_dollar_line() {
	local dollar="$(echo -p "\$ ")"
	local dollar="$(echo -e "\e[1;32m$dollar\e[0m")"
	echo -n "\e[1;34m┗━ $dollar"

}
prompt_jobs_line() {
	local job_count="$(echo -p '\j')"
	if [ "$job_count" -gt 0 ]; then
	  echo -n "\e[1;34m┃ \e[1;33m󰒓 $job_count job(s) running\e[0m\n"
	fi

}
prompt_pwd_line() {
	local pwd=$(echo -p "\W")
	[ "$pwd" = "/" ] && pwd=""
	echo -p "\e[1;34m┣━━ \e[1;36m$pwd\e[1;32m/"

}
prompt_ssh_line() {
	local ssh_server="$(echo $SSH_CONNECTION | cut -f3 -d' ')"
	[ -n "$ssh_server" ] && echo -n "\e[1;34m┃ \e[1;39m🌐 $ssh_server\e[0m\n"
	return 0

}
prompt_topline() {
	local user_and_host="\e[0m\e[1m$USER\e[1;36m@\e[1;31m$HOST\e[0m"
	echo -n "\e[1;34m┏━ $user_and_host\n"

}
shed_ver() {
	echo -en "\e[1;36m\\\$$(version -v)"

}
stat_line_left() {
	local last_exit="$?"
	__PREV_BG=""
	emit_mode && \
	__mod time "$(git_stat_line)" $LINE_SEP_LEFT && \
	__mod stat "$(cmd_status_line)" $LINE_SEP_LEFT && \
	__cap $LINE_SEP_LEFT "18"


}
stat_line_right() {
	__PREV_BG="18"
	__mod_right stat "$(stat_compose)"
	__mod_right stat "$(stat_lvl)"
	__mod_right time "$(shed_ver)"

}
stat_lvl() {
	local c
	local lvl="$((SHLVL - 1))" # -1 to ignore the login shell
	case $lvl in
	  1) c=32  ;;
	  2) c=33  ;;
	  3) c=208 ;;
	  *) c=31  ;;
	esac

	echo -en "\e[1;39mLVL \e[35m(\e[39m\e[1;${c}m$lvl\e[35m)\e[39m "

}
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
prompt() {
	local topline="$(prompt_topline)"
	local jobsline="$(prompt_jobs_line)"
	local sshline="$(prompt_ssh_line)"
	local pwdline="$(prompt_pwd_line)"
	local dollarline="$(prompt_dollar_line)"
	local prompt="\n$topline$jobsline$sshline$pwdline\n$dollarline"

	echo -en "$prompt"

}

# other stuff

__ls_no_sound() {
	local SQR_TABLE_JUSTIFY="left"

	local output=$(command eza -l --color=always --icons=always --group-directories-first "$@")
	if [ -z "$output" ]; then
	  return
	fi

	vice --lines --quoted --sep 'w' <<< "$output" \
	  -c 'viW' \
	  -r 1:2 \
	  -c 'WEE' \
	  -c '$' \
	  | qname mode size user date name \
	  | qselect -n name size user date mode \
	  | __emit_sqr -n

}
_edit_line() {
	tmp="$(mktemp)"
	echo -n "$_BUFFER" > "$tmp"
	$EDITOR "$tmp"
	_BUFFER="$(cat "$tmp")"
	rm "$tmp"

}
_enum_chars() {
	local i=0
	[ -z "$1" ] && return 1
	[ "${#1}" -eq 1 ] && echo "0 $1" && return 0

	while [ "$i" -lt ${#1} ]; do
	  echo -n "$i ${1:$i:1} "
	  i=$((i + 1))
	  [ $i -ge "${#1}" ] && break
	  echo -n " "
	done
	echo

}
_enum_chars_rev() {
	local i=$((${#1} - 1))
	[ -z "$1" ] && return 1
	[ "${#1}" -eq 1 ] && echo "0 $1" && return 0

	while [ "$i" -ge 0 ]; do
	  echo -n "$i ${1:$i:1} "
	  i=$((i - 1))
	  [ $i -lt 0 ] && break
	  echo -n " "
	done
	echo

}
_get_surround_target() {
	readkey -v _s_ch
	case "$_s_ch" in
	  \(|\)) _sl='('; _sr=')' ;;
	  \[|\]) _sl='['; _sr=']' ;;
	  \{|\}) _sl='{'; _sr='}' ;;
	  \<|\>) _sl='<'; _sr='>' ;;
	  *) _sl="$_s_ch"; _sr="$_s_ch" ;;
	esac

}
_read_obj() {
	_obj=""
	while readkey -v key; do
	  if [[ "${#_obj}" -ge 3 ]]; then return 1; fi
	  case "$key" in
	    i|a)
	      if [ -n "$_obj" ]; then return 1; fi
	      _obj="$key"
	      ;;
	    b|e)
	      if [ -n "$_obj" ]; then return 1; fi
	      _obj="$key"
	      break
	      ;;
	    w|W)
	      _obj="$_obj$key"
	      break
	      ;;
	    f|F)
	      readkey -v char
	      _obj="$key$char"
	      break
	    ;;
	    \(|\)|\[|\]|\{|\}|\"|\')
	      if [ -z "$_obj" ]; then return 1; fi
	      _obj="$_obj$key"
	      break
	      ;;
	  esac
	done

}
_scan_left() {
	local needle="$1"
	local haystack="$2"
	local i=$((${#haystack} - 1))


	while [ "$i" -ge 0 ]; do
	  ch="${haystack:$i:1}"
	  if [ "$ch" = "$needle" ]; then
	    left=$i
	    return 0
	  fi
	  i=$((i - 1))
	done

	return 1

}
_scan_right() {
	local needle="$1"
	local haystack="$2"
	local i=0


	while [ "$i" -lt "${#haystack}" ]; do
	  ch="${haystack:$i:1}"
	  if [ "$ch" = "$needle" ]; then
	    right="$i"
	    return 0
	  fi
	  i=$((i + 1))
	done

	return 1

}
_surround_1() {
	local _obj
	_read_obj
	_get_surround_target
	_KEYS="v$_obj"

}
_surround_2() {
	local start
	local end
	if [ "$_ANCHOR" -lt "$_CURSOR" ]; then
	  start=$_ANCHOR
	  end=$_CURSOR
	else
	  start=$_CURSOR
	  end=$_ANCHOR
	fi
	end=$((end + 1))

	delta=$((end - start))

	left="${_BUFFER:0:$start}"
	mid="${_BUFFER:$start:$delta}"
	right="${_BUFFER:$end}"
	_BUFFER="$left$_sl$mid$_sr$right"
	_CURSOR=$start

}
_surround_del() {
	_get_surround_target
	local left_buf="${_BUFFER:0:$_CURSOR}"
	local right_buf="${_BUFFER:$left}"
	local left=""
	local right=""
	_scan_left $_sl "$left_buf"

	if [ "$?" -ne 0 ]; then
	  _scan_right $_sl "$right_buf"

	  [ "$?" -ne 0 ] && echo "No match found in left or right scan for char '$_sl' on $left_buf" 1>&2 && return 1
	  left=$right
	fi

	mid_start=$((left + 1))
	right=""
	left_buf="${_BUFFER:0:$left}"
	right_buf="${_BUFFER:$mid_start}"
	_scan_right $_sr "$right_buf"

	[ "$?" -ne 0 ] && echo "No match found in right scan for char '$_sr'" 1>&2 && return 1

	mid_end=$((mid_start + right))
	right_start=$((mid_end + 1))
	new_left_buf="${_BUFFER:0:$left}"
	new_mid_buf="${_BUFFER:$mid_start:$right}"
	new_right_buf="${_BUFFER:$right_start}"


	_BUFFER="$new_left_buf$new_mid_buf$new_right_buf"

}
backup_history() {
	if [ -f ~/.local/state/shed/last_backup ]; then
	  local curr_time=$(date +%s)
	  local old_time=$(thru ~/.local/state/shed/last_backup)
	  if (( curr_time - old_time <= (24 * 60 * 60) )); then
	    return 0
	  fi
	fi

	hist --json > ~/.local/state/shed/shed_hist_backup.json
	date +%s > ~/.local/state/shed/last_backup
	msg "shell history backed up [$(stat ~/.local/state/shed/shed_hist_backup.json -c '%S')]"

}
decrypt() {
	if [ -z "$1" ]; then
	  gpg --decrypt --quiet 2>/dev/null
	else
	  echo "$1" | gpg --decrypt --quiet
	fi

}
encrypt() {
	if [ -z "$1" ]; then
	  echo "Usage: encrypt <text> [recipient]"
	  return 1
	fi
	if [ -z "$2" ]; then
	  gpg --encrypt --armor -r "$1"
	else
	  echo "$1" | gpg --encrypt --armor -r "$2"
	fi

}

# git wrappers

gitcheckout_sfx() {
	if git checkout "$@"; then
	  playshellsound gitcheckout.wav
	else
	  status="$?"
	  playshellsound error.wav
	  return $status
	fi

}
gitcommit_sfx() {
	local output="$(git commit "$@")"
	if [ "$?" -eq "0" ]; then
	  playshellsound gitcommit.wav
	  echo "$output" | /nix/store/k2dzyczhlvvp9arhmvzk4x6avj0q4p57-color-commit/bin/color-commit
	  return 0
	else
	  playshellsound error.wav
	  echo "$output"
	  return 1
	fi

}
gitpull_sfx() {
	if git pull "$@"; then
	  playshellsound gitpull.wav
	else
	  status="$?"
	  playshellsound error.wav
	  return $status
	fi

}
gitpush_sfx() {
	if git push "$@"; then
	  playshellsound gitpush.wav
	else
	  status="$?"
	  playshellsound error.wav
	  return $status
	fi

}
gitrebase_sfx() {
	if git rebase "$@"; then
	  playshellsound gitrebase.wav
	else
	  status="$?"
	  playshellsound error.wav
	  return $status
	fi

}
grimblast() {
	if command grimblast "$@"; then
	  playshellsound screenshot.wav
	fi
}

hyprsock() {
	socat -U - UNIX-CONNECT:$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock
}
ls() {
	playshellsound ls.wav

	__ls_no_sound "$@"
}
lvl() {
	echo $SHLVL
}
mkcd() {
	command mkdir -p "$1" && builtin cd "$1"
}
neoscry() {
	local file
	if ! file=$(fd | scry -p "pick a file..."); then
	  return
	fi

	echo "$file" | xargs neovide
}
neovide() {
	playshellsound nvim.wav
	command neovide "$@"

}
nvim() {
	playshellsound nvim.wav
	command nvim "$@"

}
reboot() {
	echo "Really? enter = yes"
	readkey -v res
	case "$res" in
	  "<Enter>")
	    command reboot
	  ;;
	  *)
	    echo "Canceling reboot."
	  ;;
	esac

}

# compose mode - enter stops submitting
# makes text editing in the prompt easy
stat_compose() {
	(( SHED_COMPOSE )) && echo -en "\e[1;39m[COMPOSE]\e[39m "

}
toggle_compose() {
	if (( SHED_COMPOSE )); then
	  keymap -i --remove '<Enter>'
	  keymap -i --remove '<S-Enter>'
	  SHED_COMPOSE=0
	else
	  keymap -i '<Enter>' '<CMD>breakline'
	  keymap -i '<S-Enter>' '<CMD>submit'
	  SHED_COMPOSE=1
	fi
}
# toggle with F7
keymap -i '<F7>' '<CMD>!toggle_compose'

# search upward for a file
# useful for stuff like Cargo.toml or flake.nix in project directories
upfind() {
	until [[ "$#" -eq 0 ]]; do
	  target="$1"
	  (
	    until [[ -e "./$target" ]]; do
	      builtin cd ..
	      if [[ "$PWD" == "/" ]]; then
	        echo "upsearch: failed to find file '$target' in this directory or any parent directories." 1>&2
	        exit 1
	      fi
	    done
	    realpath "./$target"
	  )
	  if [[ "$?" -ne 0 ]]; then
	    return
	  fi
	  shift 1
	done

}
viflake() {
	filename="$(upfind flake.nix)"
	if [ -n "$filename" ]; then
	  nvim "$filename"
	else
	  echo "No flake.nix found in this directory or any parent directories."
	  return 1
	fi

}

# keymaps!!!!!!

## canned commands
## hitting the associated keymap will write the stashed command at the cursor's position

stash --save "commit"     $'gitcommit_sfx -m ""' 18
keymap -n '<leader>gc' '<CMD>stash apply commit<CR>i'

stash --save "if_stmt"    $'if :; then\n\t:\nfi' 0
keymap -n '<leader>if' '<CMD>stash insert if_stmt<CR>/:'

stash --save "while_loop" $'while :; do\n\t:\ndone' 0
keymap -n '<leader>wh' '<CMD>stash insert while_loop<CR>/:'

stash --save "until_loop" $'until :; do\n\t:\ndone' 0
keymap -n '<leader>un' '<CMD>stash insert until_loop<CR>/:'

stash --save "func_def"   $':() {\n\t:\n}' 0
keymap -n '<leader>fn' '<CMD>stash insert func_def<CR>/:'

stash --save "for_loop"   $'for : in :; do\n\t:\ndone' 0
keymap -n '<leader>for' '<CMD>stash insert for_loop<CR>/:'

stash --save "case_stmt"  $'case : in\n\t*)\n\t\t:\n\t;;\nesac' 0
keymap -n '<leader>ca' '<CMD>stash insert case_stmt<CR>/:'

keymap -n '<leader>yy' '<CMD>w!wl-copy' # copy the current line buffer
keymap -n '<leader>hp' '<CMD>!hist --pull' # pull commands from other sessions

# search command history, and insert a picked command at the cursor
keymap -n '!' '<CMD>r!hist --no-dupes --quoted -n | scry -q -n -p "insert a history entry"'

# use these with visual mode
# Ctrl+C - copy
keymap -v '<C-c>' '<CMD>!tee >(wl-copy) 2> /dev/null<CR><ESC>'
# Ctrl+X - cut
keymap -v '<C-x>' '<CMD>!wl-copy 2> /dev/null<CR><ESC>'
# Ctrl+V - paste
keymap -i '<C-v>' '<CMD>r!wl-paste -n 2> /dev/null'

# automatically close delimiters
keymap -i '(' '()<left>'
keymap -i '[' '[]<left>'
keymap -i '{' '{}<left>'
keymap -i '"' '""<left>'

keymap -n '<leader>d' '<CMD>!msg $(date)'  # check the date
keymap -n '<leader>lg' '<CMD>!lazygit'     # open lazygit
keymap -n '<leader>e' '<CMD>expand'        # force-expand the current buffer
keymap -n '<C-o>' '<CMD>!prevd -n 2> /dev/null' # go back one directory
keymap -n '<C-i>' '<CMD>!nextd -n 2> /dev/null' # go forward one directory
keymap -n '<leader>m' '<CMD>!zd'      # zd shortcut
keymap -n '<leader>e' '<CMD>!neoscry' # list files, open picked file in editor

# list files in home directory, write picked path at the current cursor position
keymap -n '<leader>p' '<CMD>r!fd . ~ | scry -n -p "pick a path..." | quote -n'

# quick access to leader-keymaps from insert mode
keymap --recursive -i '<C-leader>' '<C-o><leader>'

# autocmds

## update git status line module
autocmd post-cmd 'if [ "${STATLINE_GIT:-0}" -eq 1 ]; then git_stat_line_update; else export GIT_STAT_LINE=""; fi'
autocmd on-idle-timeout 'if [ "${STATLINE_GIT:-0}" -eq 1 ]; then LAST_DIFF=""; git_stat_line_update; else export GIT_STAT_LINE=""; fi'

## sound effects for history search/tab completion
autocmd on-history-open 'if [ -n "$NUM_MATCHES" ] && [ "$NUM_MATCHES" -gt 0 ]; then playshellsound "ls.wav"; fi'
autocmd on-completion-start 'if [ -n "$NUM_MATCHES" ] && [ "$NUM_MATCHES" -gt 0 ]; then playshellsound "ls.wav"; fi'

autocmd pre-change-dir 'playshellsound '\''cd.wav'\'''
autocmd pre-change-dir '__ls_no_sound "$NEW_DIR"'

if [ -n "$DISPLAY" ] || [ -n "$WAYLAND_DISPLAY" ]; then
  # if we are in a graphical session, use GUI editor
  export EDITOR="neovide" SUDO_EDITOR="neovide" VISUAL="neovide"
else
  # otherwise, use neovim
  export EDITOR="nvim" SUDO_EDITOR="nvim" VISUAL="nvim"
fi

# add my `invoke` script to the syntax highlighter's list of "exec wrappers"
# these get highlighted as keywords, and the word after them is highlighted as a command
SHED_EXEC_WRAPPERS+=("invoke")

alias fds='lsfd -p $$ -Q "FD >=0" -o FD,XMODE,TYPE,NAME'

autoload -p "$HOME/scripts/shed_autoload"

if [ "${STATLINE_GIT:-0}" -eq 1 ]; then
  git_stat_line_update
else
  export GIT_STAT_LINE=""
fi

if [ -n "$LS_COLORS" ]; then unset LS_COLORS; fi

if [ -f "$HOME/.shedrc_mut" ]; then
  # i use nix to generate my config, so I can't edit my rc file directly
  # i can edit this `.shedrc_mut` file though, so i use this for testing edits before committing
  source "$HOME/.shedrc_mut"
fi

backup_history # script defined in my nix config
