#!/usr/bin/env bash
# Synthetic input for verifying the mslicer interface on this Wayland session.
#
#   pointer      dotool  (uinput; supports press/release, so drags work)
#   keyboard     ydotool (uinput via ydotoold; the only tool here whose
#                        modifier chords reach the app)
#   window focus kdotool (KWin scripting)
#
# wtype is installed but unusable on KWin: it needs zwp_virtual_keyboard_v1,
# which KWin does not implement.
#
# Coordinates are pixels on the current screen; override with
# MSLICER_SCREEN_W/MSLICER_SCREEN_H if it is not 1920x1080.
#
# Usage:
#   scripts/ui-input.sh focus
#   scripts/ui-input.sh move X Y
#   scripts/ui-input.sh click X Y
#   scripts/ui-input.sh drag X1 Y1 X2 Y2
#   scripts/ui-input.sh type TEXT...
#   scripts/ui-input.sh key CHORD
#
# Examples:
#   scripts/ui-input.sh drag 1000 250 1200 310   # orbit the camera
#   scripts/ui-input.sh key ctrl+t               # load the teapot
#
# `key` takes dotool-free chords: modifiers (ctrl, shift, alt, super) joined
# with '+', plus a letter, digit, or a common key (enter, tab, escape, space,
# backspace, delete, home, end). It translates them to raw evdev keycodes
# because ydotool has no layout-agnostic key names.
#
# Keyboard actions need the ydotool daemon: `devenv up` starts it, or run
# `ydotoold &` once. /dev/uinput must be writable by the user (the uaccess ACL
# grants this on Plasma).

set -euo pipefail

screen_w=${MSLICER_SCREEN_W:-1920}
screen_h=${MSLICER_SCREEN_H:-1080}

pct() { awk -v v="$1" -v s="$2" 'BEGIN { printf "%.4f", v / s }'; }

focus() {
    kdotool search --class mslicer --limit 1 windowactivate >/dev/null 2>&1 || true
}

mouseto() {
    printf 'mouseto %s %s\n' "$(pct "$1" "$screen_w")" "$(pct "$2" "$screen_h")"
}

pointer() {
    focus
    sleep 0.2
    dotool
}

keyboard() {
    focus
    sleep 0.2
    if ! ydotool key "$@" 2>/tmp/ui-input.err; then
        cat /tmp/ui-input.err >&2
        echo "hint: start the daemon with 'ydotoold &' (or 'devenv up')" >&2
        return 1
    fi
}

modcode() {
    case $1 in
        ctrl | control) echo 29 ;;
        shift) echo 42 ;;
        alt) echo 56 ;;
        super | meta | win) echo 125 ;;
        *) return 1 ;;
    esac
}

keycode() {
    case $1 in
        a) echo 30 ;; b) echo 48 ;; c) echo 46 ;; d) echo 32 ;; e) echo 18 ;;
        f) echo 33 ;; g) echo 34 ;; h) echo 35 ;; i) echo 23 ;; j) echo 36 ;;
        k) echo 37 ;; l) echo 38 ;; m) echo 50 ;; n) echo 49 ;; o) echo 24 ;;
        p) echo 25 ;; q) echo 16 ;; r) echo 19 ;; s) echo 31 ;; t) echo 20 ;;
        u) echo 22 ;; v) echo 47 ;; w) echo 17 ;; x) echo 45 ;; y) echo 21 ;;
        z) echo 44 ;;
        0) echo 11 ;; 1) echo 2 ;; 2) echo 3 ;; 3) echo 4 ;; 4) echo 5 ;;
        5) echo 6 ;; 6) echo 7 ;; 7) echo 8 ;; 8) echo 9 ;; 9) echo 10 ;;
        enter | return) echo 28 ;; tab) echo 15 ;; escape | esc) echo 1 ;;
        space) echo 57 ;; backspace) echo 14 ;; delete) echo 111 ;;
        home) echo 102 ;; end) echo 107 ;;
        *) return 1 ;;
    esac
}

chord() {
    local chord=$1
    IFS=+ read -ra parts <<<"$chord"
    local last=$((${#parts[@]} - 1))
    local code mods=() codes=()
    for ((i = 0; i < last; i++)); do
        if ! local m=$(modcode "${parts[i]}"); then
            echo "unknown modifier: ${parts[i]}" >&2
            exit 2
        fi
        mods+=("$m")
    done
    code=$(keycode "${parts[last]}") || { echo "unknown key: ${parts[last]}" >&2; exit 2; }
    for m in "${mods[@]}"; do codes+=("$m:1"); done
    codes+=("$code:1" "$code:0")
    for ((i = ${#mods[@]} - 1; i >= 0; i--)); do codes+=("${mods[i]}:0"); done
    keyboard "${codes[@]}"
}

cmd=${1:-}
shift || true
case $cmd in
    focus) focus ;;
    move)
        mouseto "$1" "$2" | pointer ;;
    click)
        { mouseto "$1" "$2"; sleep 0.2; echo "buttondown left"; sleep 0.1; echo "buttonup left"; } | pointer ;;
    drag)
        x1=$1 y1=$2 x2=$3 y2=$4
        dx=$((x2 - x1)) dy=$((y2 - y1))
        steps=6
        # Events must be paced: egui only reports a drag once it sees motion
        # across frames, and dotool otherwise flushes the whole gesture at once.
        {
            mouseto "$x1" "$y1"
            sleep 0.2
            echo "buttondown left"
            sleep 0.15
            for ((i = 1; i <= steps; i++)); do
                printf 'mousemove %d %d\n' $((dx * i / steps)) $((dy * i / steps))
                sleep 0.12
            done
            echo "buttonup left"
        } | pointer ;;
    type)
        focus
        sleep 0.2
        ydotool type "$*" ;;
    key)
        chord "$1" ;;
    *)
        sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
        exit 2 ;;
esac
