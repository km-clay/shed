_trap_comp() {
  local -A flags=(
    [l]="list traps"
    [p]="print the trap commands"
  )
  local -a signals=( $(trap -l) )
  case "$2" in
    -*) compadd -A flags -P '-' ;;
    *)
      case "$3" in
        trap)
          compadd -A flags -P '-'
          compadd -a signals -P 'SIG'
          ;;
        *)    compadd -a signals -P 'SIG' ;;
      esac
    ;;
  esac
}
complete -F _trap_comp trap
