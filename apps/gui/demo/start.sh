#!/bin/bash
# Starts the desktop app on a virtual display against a fresh, scratch cauldron home.
# usage: start.sh   (env: GUI_BIN, CAULDRON_BIN)
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
export CAULDRON_BIN="${CAULDRON_BIN:-$HERE/../../../target/debug/cauldron}"
GUI_BIN="${GUI_BIN:-$HERE/../target/debug/cauldron-gui}"
export CAULDRON_HOME=/tmp/cauldron-demo-home
export CAULDRON_DEMO_DIR="$HERE" CAULDRON_DEMO_ENV=demo CAULDRON_DEMO_DEST=/tmp/cauldron-demo-projects/GotYaForce
export CAULDRON_GUI_HITMAP=/tmp/cauldron-gui-hitmap.json CAULDRON_GUI_POINTER=1
rm -rf "$CAULDRON_HOME" /tmp/cauldron-demo-projects "$CAULDRON_GUI_HITMAP"
mkdir -p "$CAULDRON_HOME" /tmp/cauldron-demo-projects
"$CAULDRON_BIN" env create demo >/dev/null 2>&1
pkill -x cauldron-gui 2>/dev/null; pkill -x Xvfb 2>/dev/null; sleep 0.5
Xvfb :99 -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &
sleep 1.5
export DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=gl WINIT_X11_SCALE_FACTOR=1
nohup "$GUI_BIN" >/tmp/cauldron-gui.log 2>&1 &
echo "started"
