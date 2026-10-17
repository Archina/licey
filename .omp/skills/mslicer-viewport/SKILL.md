---
name: mslicer-viewport
description: Build, launch and visually verify the mslicer bevy 3D viewport headlessly — screenshot the window, crop the viewport, diff a model against an empty scene, and scan for panics or wgpu validation errors. Use after changing rendering, scene sync, camera, gizmo, plate/grid or material code.
---

# Verify the mslicer viewport

The renderer is only observable as pixels on this machine: there is no synthetic input tooling
(no xdotool/ydotool/wtype) and no headless render target. This skill is the loop used to verify
every bevy-port change: build → launch → screenshot → crop → measure.

## Fast path

```sh
scripts/viewport-smoke.sh --compare-empty
```

It builds (`cargo build -p mslicer`), launches the app twice (empty scene, then
`mslicer/assets/teapot.stl`), screenshots the desktop each time, crops the app window, and prints:

```
   screenshot: target/viewport-smoke/viewport.png
   crop:       1548x934+365+96 (of 1920x1080)
   probes:     (335,204)=#090909 (1135,704)=#090909
   model vs empty inside the crop:
     bbox=58x34+745+442 px=1170
   no panics or validation errors
```

Same thing as a model tool: `mslicer_viewport` (params: `model`, `compareEmpty`, `build`,
`settle`, `crop`, `out`). Takes ~25s.

## What good looks like

| Signal | Healthy | Broken |
| --- | --- | --- |
| model diff bbox | ≈ `58x34+745+442` for the teapot (crop coords) | empty/1x1 bbox → model not drawn; huge bbox → wrong camera or garbage |
| changed pixels | ≈ 1100–1200 | 0 → nothing rendered; tens of thousands → background/clear colour differs |
| probes | `#090909` | magenta/blue → wgpu error surface; other colours → wrong clear/layout |
| log | no `panicked` / `Validation Error` | any match means the app aborted or wgpu rejected a draw |

The probes are the viewport background, but note that **the dock panels use the same `#090909`**,
so colour alone cannot delimit the viewport. Use the fixed crop instead.

## Manual recipe

```sh
cargo build -p mslicer
./target/debug/mslicer mslicer/assets/teapot.stl >/tmp/mslicer.log 2>&1 &
sleep 10                                   # first frame needs ~4–6s
spectacle -b -n -f -o /tmp/shot.png        # full screen, background mode
pkill -f "licey/target/debug/mslicer"
magick /tmp/shot.png -crop 1548x934+365+96 +repage /tmp/vp.png
magick /tmp/vp.png -alpha off -format "%[hex:p{335,204}]\n" info:   # background probe
grep -nE 'panicked|Validation Error' /tmp/mslicer.log
```

Diff a model against an empty scene (capture the empty scene the same way first):

```sh
magick empty.png model.png -compose difference -composite -colorspace Gray \
  -threshold 2% -format 'bbox=%@ px=%[fx:mean*w*h]\n' info:
```

## Geometry on this machine

| Thing | Value |
| --- | --- |
| screen | 1920x1080, `DISPLAY=:0` (Wayland session; spectacle still captures) |
| app window content | 1548x934 at (365, 96) |
| viewport centre | ≈ (1110, 538) absolute |
| teapot (default camera) | bbox `58x34` at (1110, 538) absolute |
| build plate grid line | ≈ `#A2A2A2` |

If the window manager moves the window, the crop no longer matches: pass `--crop WxH+X+Y`.
`spectacle -a` (active window) is an alternative but the in-window offsets are not known, so the
full-screen crop stays the default.

## Pitfalls

- **Settle before capturing**: the window exists before the first 3D frame; `sleep 10` is the
  tested default (`--settle`). Too early → black/partial viewport.
- **Stale instances steal the capture**: always kill the app before the next run; the script uses
  `setsid` + a process-group kill, plus a `pkill -f` fallback.
- **Benign log noise**: `Skipping this driver` (vulkan dzn) and `Couldn't get monitor selected with:
  Primary` are normal; only `panicked` and `Validation Error` are failures.
- **No synthetic input**: to test gizmo drags, hover, or clicks, add temporary app-side
  instrumentation (an arg that injects a drag, a once-per-frame system, log lines), then remove it
  and re-grep for residue before committing.
- **The debug binary is slow**: incremental builds ≈ 40s, startup ≈ 4–6s. `--no-build` skips the
  build once it is current.
- **Empty scene ≠ welcome screen**: with no model argument the app still shows the full dock layout;
  that is what the empty baseline captures.
- **Resize races**: if you are testing window resizing, shrink the window and watch the log for
  scissor validation errors — the gizmo overlay viewport must stay clamped to the window.
