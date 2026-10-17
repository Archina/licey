#!/usr/bin/env bash
# Headless capture and measurement of the mslicer 3D viewport.
#
# Launches the app on the current display, screenshots it, crops the window's
# content area, and reports:
#   - background probe colours (a working renderer paints #090909, not a
#     magenta/blue wgpu error surface),
#   - with --compare-empty, the bounding box and pixel count of what the model
#     adds over an empty scene (an invisible model shows up as a tiny bbox),
#   - panics / wgpu validation errors found in the app's stderr.
#
# Usage:
#   scripts/viewport-smoke.sh [options]
#
#   --model PATH        Mesh to open (default: mslicer/assets/teapot.stl)
#   --no-model          Start with an empty scene
#   --out FILE          Where to write the cropped screenshot
#                       (default: target/viewport-smoke/viewport.png)
#   --crop GEOMETRY     ImageMagick crop, WxH+X+Y (default: 1548x934+365+96)
#   --compare-empty     Also capture an empty scene and diff against it
#   --settle SECS       Wait before each screenshot (default: 10)
#   --no-build          Use the existing target/debug/mslicer
#   --keep              Keep the raw full-screen shots and logs
#
# Exit status is non-zero if the build fails, the app dies, a panic or wgpu
# validation error is logged, or the screenshot is empty.
#
# The default crop is the app window's content area on a 1920x1080 X11 session.
# If the window manager places the window elsewhere, pass --crop.

set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
model=$repo/mslicer/assets/teapot.stl
use_model=1
out=$repo/target/viewport-smoke/viewport.png
crop=${MSLICER_CROP:-1548x934+365+96}
settle=${MSLICER_SETTLE:-10}
compare=0
build=1
keep=0

while (($#)); do
    case $1 in
        --model) model=${2:?--model needs a path}; shift 2 ;;
        --no-model) use_model=0; shift ;;
        --out) out=${2:?--out needs a path}; shift 2 ;;
        --crop) crop=${2:?--crop needs WxH+X+Y}; shift 2 ;;
        --compare-empty) compare=1; shift ;;
        --settle) settle=${2:?--settle needs seconds}; shift 2 ;;
        --no-build) build=0; shift ;;
        --keep) keep=1; shift ;;
        -h|--help) sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
    esac
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/viewport-smoke.XXXXXX")
log=$tmp/mslicer.log
pidfile=$tmp/pid
fail=0

cleanup() {
    stop_app
    if ((keep)); then
        echo "kept: $tmp"
    else
        rm -rf "$tmp"
    fi
}
trap cleanup EXIT

start_app() {
    ( cd "$repo" && setsid ./target/debug/mslicer "$@" >"$log" 2>&1 & echo $! >"$pidfile" )
}

stop_app() {
    [[ -s $pidfile ]] || return 0
    local pid
    pid=$(cat "$pidfile")
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    for _ in {1..30}; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    kill -KILL -- "-$pid" 2>/dev/null || true
    pkill -f "$repo/target/debug/mslicer" 2>/dev/null || true
    rm -f "$pidfile"
}

# capture SCENE SHOT MODEL... — start the app, wait, screenshot, stop it.
capture() {
    local scene=$1 shot=$2
    shift 2
    : >"$log"
    start_app "$@"
    sleep "$settle"
    local pid
    pid=$(cat "$pidfile")
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "error: $scene: app exited before the screenshot" >&2
        tail -20 "$log" >&2
        return 1
    fi
    rm -f "$shot"
    spectacle -b -n -f -o "$shot" >/dev/null 2>&1
    stop_app
    [[ -s $shot ]] || { echo "error: $scene: no screenshot ($shot)" >&2; return 1; }
}

probe() { magick "$1" -alpha off -format "%[hex:p{$2}]" info:; }

[[ -x $repo/target/debug/mslicer ]] || build=1
if ((build)); then
    echo "== build"
    if ! ( cd "$repo" && cargo build -p mslicer ); then
        echo "error: build failed" >&2
        exit 1
    fi
fi

mkdir -p "$(dirname "$out")"
out=${out%.png}.png

echo "== capture"
if ((compare)); then
    raw_empty=$tmp/empty.png
    capture "empty scene" "$raw_empty" || exit 1
    baseline=$tmp/empty.viewport.png
    magick "$raw_empty" -crop "$crop" +repage "$baseline"
fi

raw_model=$tmp/model.png
if ((use_model)); then
    capture "model" "$raw_model" "$model" || exit 1
else
    capture "empty scene" "$raw_model" || exit 1
fi
magick "$raw_model" -crop "$crop" +repage "$out"

echo "   screenshot: $out"
echo "   crop:       $crop (of $(magick "$raw_model" -format '%wx%h' info:))"
echo "   probes:     (335,204)=#$(probe "$out" 335,204) (1135,704)=#$(probe "$out" 1135,704)"

if ((compare)); then
    echo "   model vs empty inside the crop:"
    echo -n "     "
    magick "$baseline" "$out" -compose difference -composite -colorspace Gray \
        -threshold 2% -format 'bbox=%@ px=%[fx:mean*w*h]\n' info:
fi

echo "== log"
if errors=$(grep -nE 'panicked|Validation Error' "$log"); then
    echo "$errors" | head -5
    fail=1
else
    echo "   no panics or validation errors"
fi

if ((!use_model)); then
    echo "   (empty scene: the model diff is not meaningful)"
fi

exit "$fail"
