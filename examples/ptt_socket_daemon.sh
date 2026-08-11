#!/usr/bin/env shed

# This is a push-to-talk daemon
# that lets you swap between push-to-talk and push-to-mute
# It uses the `sock`, `listen`, and `accept` builtins to run
# a simple socket server that listens for requests.
#
# Might not be instantly executable in your environment since it
# relies on stuff like wpctl (pipewire), waybar, and other stuff.
# This exists mainly to serve as an example for how to use the socket builtins.

: "${XDG_RUNTIME_DIR:?runtime dir not set}"
LOCK="$XDG_RUNTIME_DIR/ptt.lock" # lock file, ensures that only one daemon runs at a time
SOCK="$XDG_RUNTIME_DIR/ptt.sock" # socket file path
STATE_FILE="$XDG_RUNTIME_DIR/ptt.state" # state file that contains current state of the microphone

# 0 = push-to-talk
# 1 = push-to-mute
declare -i MODE=1

if [ -n "$1" ]; then
  case "$1" in
    key_down|key_up|toggle_mode)
      # We have received a valid request.
      # Let's open a connection to the socket file and write the request to it.
      sock -U "$SOCK" 3

      # sock reports distinct statuses for distinct connect failures, which
      # lets us tell "daemon never started" apart from "daemon crashed and left
      # a stale socket behind".
      case $? in
        0) ;; # connected
        3) echo "ptt daemon not running (no socket at $SOCK)" >&2; exit 1 ;;
        4) echo "ptt daemon not running (stale socket, nothing listening)" >&2; exit 1 ;;
        7) echo "permission denied opening $SOCK" >&2; exit 1 ;;
        *) echo "could not reach ptt daemon" >&2; exit 1 ;;
      esac

      # sock has now opened a client-side connection to the socket on fd 3.
      # now we can redirect to fd 3 to write to it.
      printf '%s\n' "$1" >&3

      # don't forget to close the fd so that the server sees EOF.
      exec 3>&-
      exit 0
      ;;
    *)
      echo "invalid request"
      exit 1
      ;;
  esac
fi

# time in seconds to keep holding after hotkey release
HANGOVER="0.150"
# PID of current hangover
HANGOVER_PID=""

cancel_hangover() {
  # kill the existing hangover process
  if [ -n "$HANGOVER_PID" ]; then
    kill "$HANGOVER_PID" 2>/dev/null
    HANGOVER_PID=""
  fi
}

mute() {
  # mute the microphone
  wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 1;
  echo "muted" > "$STATE_FILE"
  pkill -RTMIN+8 waybar
}
unmute() {
  # unmute the microphone
  wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 0;
  echo "unmuted" > "$STATE_FILE"
  pkill -RTMIN+8 waybar
}

# the function to execute when the hotkey is released
resting() { if (( MODE == 0 )); then mute; else unmute; fi; }
# the function to execute when the hotkey is pressed
active() { if (( MODE == 0 )); then unmute; else mute; fi; }

on_press() {
  cancel_hangover
  active
}

on_release() {
  cancel_hangover
  # background the hangover
  { sleep "$HANGOVER"; resting; } &
  HANGOVER_PID=$!
}

# the function used to serve a single request
serve_one() {
  # accept will block until a connection is opened
  # with `-v conn`, the connection will be stored in `$conn`
  accept "$1" -v conn 2>/dev/null || return

  defer exec "$conn">&- # defer closing the connection, ensuring cleanup

  # handle the request
  while IFS= read -r cmd; do
    case "$cmd" in
      toggle_mode)
        MODE=$(( (MODE + 1) % 2 ))
        cancel_hangover
        resting
        ;;
      key_*)
        case "${cmd#key_}" in
          up)
            on_release
            ;;
          down)
            on_press
            ;;
        esac
        ;;
    esac
  done <&"$conn" # the while loop reads from the socket

  # the only time this function returns non-zero is if accept fails
  return 0
}

# open the lock file on fd 9
exec 9>"$LOCK"
# call flock on it to hog the resource
flock -n 9 || { echo "ptt daemon already running" >&2; exit 1; }

# call this once to start with so we start in the correct state
resting

# call `listen` to open the socket.
# `-v lfd` will store the listening socket in the variable `$lfd`
listen -U "$SOCK" -v lfd
defer rm -f "$SOCK" # defer removing the socket file on exit

while true; do
  if serve_one "$lfd"; then
    continue
  fi

  # if we are here, accept failed for some reason.
  case $? in
    2) reason="listen socket is no longer valid" ;;
    7) reason="permission denied" ;;
    8) reason="connection reset" ;;
    9) reason="connection aborted" ;;
    *) reason="unknown error" ;;
  esac
  printf 'failed to accept connection: %s\n' "$reason" >&2
  exit 1
done # loop forever, serving requests
