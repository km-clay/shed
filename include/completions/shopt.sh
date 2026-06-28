_shopt_comp() {
    local cword=${COMP_WORDS[$COMP_CWORD]}
    local group option
    local groups=(
      highlight
      core
      history
      line
      set
      prompt
      statline
    )
    local groups_desc=(
      "Syntax highlighting options"
      "Core config options"
      "Command history options"
      "Line editor options"
      "POSIX set options"
      "Prompt config options"
      "Status line options"
    )
    local highlight=(
      enable
      check_files
      string
      keyword
      external_command
      builtin
      function
      alias
      directory
      invalid_command
      control_flow_keyword
      argument
      argument_file
      variable
      operator
      comment
      glob
    )
    local highlight_desc=(
      "enables/disables syntax highlighting"
      "underline arguments that refer to existing files (can be slow on network mounts)"
      "strings"
      "shell keywords like 'if' and 'for'"
      "external commands found in PATH"
      "builtin commands"
      "shell functions"
      "shell aliases"
      "directories (highlighted when core.autocd is enabled)"
      "invalid or unknown commands"
      "'break', 'return', and 'continue'"
      "command arguments"
      "arguments that refer to existing files"
      "variable references"
      "operators like pipes and redirects"
      "comments"
      "glob characters"
    )
    local core=(
      dotglob
      nullglob
      autocd
      interactive_comments
      bell_enabled
      max_recurse_depth
      xpg_echo
      bell_style
      compact_errors
    )
    local core_desc=(
      "glob patterns match on hidden files"
      "expand to nothing when no files are found"
      "executing a directory name moves to it"
      "allows for writing comments on the interactive prompt"
      "allows the shell to ring the terminal's bell"
      "maximum depth of nested function calls"
      "whether or not 'echo' expands escape sequences by default"
      "whether shed sends an audible bell, a visual one, or 'both'"
      "collapse error stack traces to a single call-site label"
    )
    local history=(
      auto_save
      ignore_dupes
      ignore_space
      max_entries
      enable_concat
    )
    local history_desc=(
      "saves executed commands to your history"
      "history ignores consecutive duplicate commands"
      "commands starting with a space are not saved to history"
      "maximum number of history entries (-1 for unlimited)"
      "enables joining history entries together using Ctrl/Shift+Up/Down"
    )
    local prompt=(
      trunc_prompt_path
      comp_limit
      leader
      idle_timeout
      completion_ignore_case
      complete_style
      expand_aliases
      substitute
    )
    local prompt_desc=(
      "maximum number of path segments in the prompt's expanded CWD"
      "maximum number of completion candidates per tab press"
      "the key sequence that the <leader> key alias refers to"
      "idle seconds before firing on-idle-timeout autocmds (0=off)"
      "if enabled, tab completion ignores case when matching"
      "choose how completion candidates are presented ('fuzzy' or 'grid')"
      "if enabled, aliases are expanded on the prompt instead of during execution"
      "if enabled, performs substitution (variables, command output, etc.) after expanding prompt sequences"
    )
    local line=(
      linebreak_on_incomplete
      trim_on_submit
      viewport_height
      line_numbers
      scroll_offset
      tab_width
      auto_indent
      auto_suggest
    )
    local line_desc=(
      "whether enter breaks a new line on incomplete input, or submits the command as is"
      "if enabled, trims leading/trailing whitespace on submission"
      "maximum number of visible lines, or maximum percentage of terminal rows"
      "render line numbers in the gutter for multi-line buffers"
      "how many lines away from the top or bottom the cursor must be before the viewport scrolls"
      "visual width of tab characters in the line editor"
      "indentation level is tracked and maintained automatically"
      "the line editor will suggest similar commands from your history or tab completion as you type"
    )
    local set=(
      hashall
      vi
      allexport
      errexit
      noclobber
      monitor
      noglob
      noexec
      nolog
      notify
      nounset
      pipefail
      verbose
      xtrace
    )
    local set_desc=(
      "makes the shell remember the full path of commands to speed up lookup"
      "enables modal (vi-style) line editing"
      "all assigned variables are automatically exported to the environment"
      "the shell exits immediately when any command returns non-zero"
      "'>' and '>>' redirections fail if the target file already exists"
      "jobs run in their own process groups and report status before the next prompt"
      "filename expansion (globbing) is disabled"
      "commands are not executed (useful for syntax checking)"
      "function definitions are not written to command history"
      "asynchronous job status info is printed when jobs exit or stop"
      "expanding an unset variable (other than \$* or \$@) is an error"
      "a pipeline's exit status is the last non-zero status from any stage"
      "the shell writes its input to stderr as it is read"
      "the shell prints a trace of each command before executing it"
    )
    local statline=(
      enable
      left_string
      middle_string
      right_string
    )
    local statline_desc=(
      "enables/disables the status line"
      "raw string used for the left side of the status line"
      "raw string used for the middle of the status line"
      "raw string used for the right side of the status line"
    )
    if group=${cword%.*} && option=${cword#*.}; then
        for opt_grp in $groups; do
            if [[ "$opt_grp" == "$group" ]]; then
                compadd -P "${opt_grp}." -S '=' -d ${opt_grp}_desc -a ${opt_grp}
            fi
        done
    else
        compadd -S '.' -d groups_desc -a groups
    fi
}
complete -F _shopt_comp shopt
