#!/usr/bin/env bash
# Visual-iteration harness for the reclass GPUI app on a private Xvfb display.
# Lets me run + screenshot + drive the real UI headlessly (renders via the GPU/Vulkan).
#
#   scripts/ui.sh start [project.rcx]   # ensure Xvfb :99, (re)launch app, wait for first frame
#   scripts/ui.sh shot  [out.png]       # screenshot the whole virtual screen (default /tmp/shot.png)
#   scripts/ui.sh click X Y             # move mouse to X,Y and left-click
#   scripts/ui.sh key   <keys>          # send keys via xdotool (e.g. ctrl+p, Escape, "hello")
#   scripts/ui.sh stop                  # kill the app (leaves Xvfb up for the next run)
#   scripts/ui.sh restart [project]     # stop + build + start
set -u
export LIBRARY_PATH="${LIBRARY_PATH:-/usr/lib/gcc/x86_64-redhat-linux/16}"
export DISPLAY=:99
APP=/home/loke/reclass-rs/target/debug/reclass

ensure_xserver() {
  if ! xdpyinfo >/dev/null 2>&1; then
    setsid bash -c 'Xvfb :99 -screen 0 1680x1050x24 -ac +extension GLX +render -noreset' </dev/null >/tmp/xvfb.log 2>&1 &
    sleep 2
    setsid bash -c 'DISPLAY=:99 openbox' </dev/null >/tmp/openbox.log 2>&1 &
    sleep 1
  fi
}

case "${1:-}" in
  start)
    ensure_xserver
    pkill -x reclass 2>/dev/null; sleep 1
    setsid env -u WAYLAND_DISPLAY DISPLAY=:99 XDG_RUNTIME_DIR=/run/user/1000 "$APP" ${2:+"$2"} </dev/null >/tmp/reclass_run.log 2>&1 &
    sleep 10
    if pgrep -x reclass >/dev/null; then echo "app up (pid $(pgrep -x reclass | head -1))"; else echo "APP EXITED:"; tail -20 /tmp/reclass_run.log; fi
    ;;
  shot)
    out="${2:-/tmp/shot.png}"; import -window root "$out" 2>/dev/null && echo "shot -> $out ($(stat -c%s "$out")B)" || echo "capture failed";;
  click) xdotool mousemove "$2" "$3" click 1; echo "click $2,$3";;
  key)   shift; xdotool key "$@"; echo "key $*";;
  type)  shift; xdotool type "$*"; echo "type $*";;
  stop)  pkill -x reclass 2>/dev/null; echo "stopped";;
  restart)
    pkill -x reclass 2>/dev/null
    ( cd /home/loke/reclass-rs && LIBRARY_PATH="$LIBRARY_PATH" cargo build 2>&1 | tail -2 )
    exec "$0" start "${2:-}";;
  *) echo "usage: ui.sh {start|shot|click|key|type|stop|restart}"; exit 1;;
esac
