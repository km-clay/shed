_set_comp() {
  local -A flags=(
    [a]='export all variables'
    [b]='notify of job termination immediately'
    [C]='prevent overwriting of files by redirection'
    [e]='exit immediately if a command exits with a non-zero status'
    [f]='disable filename expansion (globbing)'
    [h]='remember the location of commands as they are looked up'
    [m]='monitor background jobs'
    [n]='read commands but do not execute them'
    [u]='treat unset variables as an error when substituting'
    [v]='print shell input lines as they are read'
    [x]='print commands and their arguments as they are executed'
    [o]='set an option by name'
  )
  local -A options=(
    [allexport]='export all variables'
    [emacs]='use emacs-style line editing keybinds'
    [errexit]='exit immediately if a command exits with a non-zero status'
    [hashall]='remember the location of commands as they are looked up'
    [ignoreeof]='ignore EOF (Ctrl+D) as a signal to exit the shell'
    [monitor]='monitor background jobs'
    [noclobber]='prevent overwriting of files by redirection'
    [noexec]='read commands but do not execute them'
    [noglob]='disable filename expansion (globbing)'
    [nolog]='do not save function definitions in the history file'
    [notify]='notify of job termination immediately'
    [nounset]='treat unset variables as an error when substituting'
    [pipefail]='return the exit status of the last command in the pipe that failed'
    [verbose]='print shell input lines as they are read'
    [vi]='use vi-style line editing keybinds'
    [xtrace]='print commands and their arguments as they are executed'
  )
  case "$3" in
    -o|+o)
      compadd -A options
      ;;
    *)
      case "$2" in
        +*) compadd -P '+' -A flags ;;
        -*) compadd -P '-' -A flags ;;
        *) case "$3" in
          set)
          for flag in "${!flags[@]}"; do
            local prefix
            if [[ "$-" == *"$flag"* ]]; then
              prefix='+'
            else
              prefix='-'
            fi
            compadd -P "$prefix" -D "${flags[$flag]}" "$flag"
          done
      esac
      ;;
  esac
  ;;
esac
}
complete -F _set_comp set
