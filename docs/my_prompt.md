## Prompt example

This is the `shed` code for the prompt that I currently use. Note that the scripting language for `shed` is essentially identical to bash. This prompt code uses the `\!` escape sequence which lets you use the output of a function as your prompt.

Also note that in `shed`, the `echo` builtin has a new `-p` flag which expands prompt escape sequences. This allows you to access these escape sequences in any context.

```bash
prompt_topline() {
  local user_and_host="\e[0m\e[1m$USER\e[1;36m@\e[1;31m$HOST\e[0m"
  echo -n "\e[1;34m┏━ $user_and_host\n"
}

prompt_stat_line() {
  local last_exit_code="$?"
  local last_cmd_status
  local last_cmd_runtime
  if [ "$last_exit_code" -eq "0" ]; then
    last_cmd_status="\e[1;32m\e[0m"
  else
    last_cmd_status="\e[1;31m\e[0m"
  fi
  local last_runtime_raw="$(echo -p "\t")"
  if [ -z "$last_runtime_raw" ]; then
    return 0
  else
    last_cmd_runtime="\e[1;38;2;249;226;175m󰔛 $(echo -p "\T")\e[0m"
  fi

  echo -n "\e[1;34m┃ $last_cmd_runtime ($last_cmd_status)\n"
}

prompt_git_line() {
  git rev-parse --is-inside-work-tree > /dev/null 2>&1 || return

  local gitsigns
  local status="$(git status --porcelain 2>/dev/null)"
  local branch="$(git branch --show-current 2>/dev/null)"

  [ -n "$status" ] && echo "$status" | command grep -q '^ [MADR]' && gitsigns="$gitsigns!"
  [ -n "$status" ] && echo "$status" | command grep -q '^??' && gitsigns="$gitsigns?"
  [ -n "$status" ] && echo "$status" | command grep -q '^[MADR]' && gitsigns="$gitsigns+"

  local ahead="$(git rev-list --count @{upstream}..HEAD 2>/dev/null)"
  local behind="$(git rev-list --count HEAD..@{upstream} 2>/dev/null)"
  [ $ahead -gt 0 ] && gitsigns="$gitsigns↑"
  [ $behind -gt 0 ] && gitsigns="$gitsigns↓"

  if [ -n "$gitsigns" ] || [ -n "$branch" ]; then
    if [ -n "$gitsigns" ]; then
      gitsigns="\e[1;31m[$gitsigns]"
    fi
    echo -n "\e[1;34m┃ \e[1;35m ${branch}$gitsigns\e[0m\n"
  fi
}

prompt_jobs_line() {
  local job_count="$(echo -p '\j')"
  if [ "$job_count" -gt 0 ]; then
    echo -n "\e[1;34m┃ \e[1;33m󰒓 $job_count job(s) running\e[0m\n"
  fi
}

prompt_ssh_line() {
  local ssh_server="$(echo $SSH_CONNECTION | cut -f3 -d' ')"
  [ -n "$ssh_server" ] && echo -n "\e[1;34m┃ \e[1;39m🌐 $ssh_server\e[0m\n"
}

prompt_pwd_line() {
  echo -p "\e[1;34m┣━━ \e[1;36m\W\e[1;32m/"
}

prompt_dollar_line() {
  local dollar="$(echo -p "\$ ")"
  local dollar="$(echo -e "\e[1;32m$dollar\e[0m")"
  echo -n "\e[1;34m┗━ $dollar "
}

prompt() {
  local statline="$(prompt_stat_line)"
  local topline="$(prompt_topline)"
  local gitline="$(prompt_git_line)"
  local jobsline="$(prompt_jobs_line)"
  local sshline="$(prompt_ssh_line)"
  local pwdline="$(prompt_pwd_line)"
  local dollarline="$(prompt_dollar_line)"
  local prompt="$topline$statline$gitline$jobsline$sshline$pwdline\n$dollarline"

  echo -en "$prompt"
}

export PS1="\!prompt "
```
