#!/usr/bin/env python3
"""Drives the cauldron desktop app for the recording.

Every click and keystroke here is SCRIPTED INPUT standing in for a person. They are
real X11 pointer and key events (XTEST), aimed at positions the app publishes in its
hit map. The director never tells the app what to show: it only clicks and types.

usage: director.py [--stop-after SCENE] [--marks FILE]
"""
import json
import sys
import time

from Xlib import X, XK, display
from Xlib.ext import xtest

HITMAP = "/tmp/cauldron-gui-hitmap.json"
d = display.Display(":99")
pos = [640, 400]
t0 = time.time()
marks = []


def state():
    for _ in range(50):
        try:
            with open(HITMAP) as f:
                return json.load(f)
        except Exception:
            time.sleep(0.05)
    return {"hits": {}, "flags": {}}


def wait_hit(key, timeout=180):
    end = time.time() + timeout
    while time.time() < end:
        h = state()["hits"].get(key)
        if h:
            return h
        time.sleep(0.1)
    raise SystemExit(f"timed out waiting for control `{key}`")


def wait_flag(key, timeout=180):
    end = time.time() + timeout
    while time.time() < end:
        if state()["flags"].get(key):
            return
        time.sleep(0.1)
    raise SystemExit(f"timed out waiting for state `{key}`")


def move(x, y, secs=0.55):
    steps = max(8, int(secs * 40))
    sx, sy = pos
    for i in range(1, steps + 1):
        k = i / steps
        k = k * k * (3 - 2 * k)
        xtest.fake_input(d, X.MotionNotify, x=int(sx + (x - sx) * k), y=int(sy + (y - sy) * k))
        d.sync()
        time.sleep(secs / steps)
    pos[0], pos[1] = x, y


def click(key, pause_before=0.35, timeout=180):
    wait_hit(key, timeout)
    time.sleep(0.25)
    x, y = wait_hit(key, timeout)  # read again: the layout may have settled
    move(x, y)
    time.sleep(pause_before)
    xtest.fake_input(d, X.ButtonPress, 1)
    d.sync()
    time.sleep(0.08)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.3)


SHIFTED = {"?": "question", ":": "colon", '"': "quotedbl", "!": "exclam", "_": "underscore", "(": "parenleft", ")": "parenright"}
PLAIN = {" ": "space", ".": "period", ",": "comma", "/": "slash", "-": "minus", "'": "apostrophe", ";": "semicolon"}


def key(keysym_name, shift=False):
    code = d.keysym_to_keycode(XK.string_to_keysym(keysym_name))
    shift_code = d.keysym_to_keycode(XK.string_to_keysym("Shift_L"))
    if shift:
        xtest.fake_input(d, X.KeyPress, shift_code)
    xtest.fake_input(d, X.KeyPress, code)
    xtest.fake_input(d, X.KeyRelease, code)
    if shift:
        xtest.fake_input(d, X.KeyRelease, shift_code)
    d.sync()


def type_text(s, cps=22):
    for ch in s:
        if ch in SHIFTED:
            key(SHIFTED[ch], True)
        elif ch in PLAIN:
            key(PLAIN[ch])
        elif ch.isupper():
            key(ch.lower(), True)
        else:
            key(ch)
        time.sleep(1.0 / cps)
    time.sleep(0.4)


def mark(name):
    marks.append({"scene": name, "at": round(time.time() - t0, 2)})
    print(f"[{marks[-1]['at']:7.2f}s] {name}", flush=True)


def scenes():
    mark("0 welcome")
    wait_hit("welcome_clone")
    time.sleep(3.0)
    click("welcome_clone")
    yield "welcome"

    mark("1 clone")
    click("clone_repo")
    type_text("abhidya/GotYaForce")
    time.sleep(0.8)
    click("clone_go")
    mark("1 clone running")
    wait_hit("ingest_go", 600)
    mark("2 ingest")
    yield "clone"

    time.sleep(4.5)
    click("ingest_go")
    mark("4 surface")
    yield "ingest"

    wait_flag("conflict_ready")
    time.sleep(3.0)
    mark("5 board")
    click("conflict")
    wait_flag("board_asked")
    time.sleep(4.0)
    click("board_reply")
    type_text("Keep it as an issue. Do not quote the numbers.")
    click("board_post")
    wait_flag("board_done")
    time.sleep(3.5)
    yield "board"

    mark("6 review")
    click("sub_review")
    time.sleep(5.0)
    click("review_approve")
    wait_flag("approved_1")
    time.sleep(3.5)
    click("sub_surface")
    time.sleep(4.0)
    click("sub_tasks")
    time.sleep(3.0)
    yield "review"

    mark("3 settings")
    click("tab_settings")
    wait_flag("settings_loaded")
    time.sleep(7.0)
    yield "settings"

    mark("7 thread")
    click("tab_threads")
    click("thread_prompt")
    type_text("Relaunch the wasm-unit driver and clear the remaining queue.")
    click("thread_send")
    wait_flag("perm_ready")
    mark("8 cards")
    time.sleep(5.0)
    click("a2ui:permission_once")
    wait_flag("visual_ready")
    time.sleep(6.0)
    click("a2ui:parity_unknown")
    wait_flag("thread0_done")
    time.sleep(4.0)
    yield "thread"

    mark("9 session dump")
    click("thread_dump")
    wait_flag("session_dumped")
    time.sleep(2.5)
    click("tab_cauldron")
    click("sub_surface")
    wait_flag("session_ready")
    time.sleep(3.5)
    click("sub_review")
    time.sleep(4.0)
    click("review_approve")
    wait_flag("approved_2")
    time.sleep(3.0)
    yield "session"

    mark("10 next thread")
    click("tab_threads")
    click("thread_1")
    click("thread_prompt")
    type_text("What is still open on the ninja dash?")
    click("thread_send")
    wait_flag("thread1_done")
    time.sleep(7.0)
    mark("end")
    yield "next"


def focus_app():
    """Give the app's window the X input focus. There is no window manager on the virtual display to do it."""
    root = d.screen().root
    best = None
    for w in root.query_tree().children:
        try:
            g = w.get_geometry()
            if w.get_attributes().map_state == X.IsViewable and g.width >= 800:
                best = w
        except Exception:
            pass
    if best is not None:
        best.set_input_focus(X.RevertToPointerRoot, X.CurrentTime)
        d.sync()


def main():
    focus_app()
    stop = sys.argv[sys.argv.index("--stop-after") + 1] if "--stop-after" in sys.argv else None
    for name in scenes():
        if name == stop:
            break
    if "--marks" in sys.argv:
        with open(sys.argv[sys.argv.index("--marks") + 1], "w") as f:
            json.dump(marks, f, indent=1)


main()
