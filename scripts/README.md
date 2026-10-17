# scripts

Random scripts I wrote to aid in the development of mslicer... Written in [YSH](https://oils.pub/ysh.html).

Most are YSH (`.ysh`). The bash exceptions:

- `viewport-smoke.sh` — builds and launches the app headlessly, screenshots the 3D viewport and
  reports the model's visible bounding box plus any panic/wgpu errors.
- `ui-input.sh` — synthetic pointer/keyboard input (`dotool` + `ydotool` + `kdotool`) for driving
  the interface; see `.omp/skills/mslicer-viewport/SKILL.md` for what works on KWin Wayland.
