width() {
  # compatibility shim for the 'width' builtin
  # that was replaced by the 'len' builtin
  len -w -- "$@"
}
