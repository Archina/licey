# mslicer — bevy fork

Fork of [mslicer](https://github.com/connorslade/mslicer) (branch `feature_egui.36`) porting the
workspace viewport from a hand-written wgpu renderer to bevy. bevy owns the window; the existing
egui/eframe interface runs inside `bevy_egui`, so the UI is still egui.

## Build & run

- Enter the dev shell first (`devenv shell`). It provides nightly Rust, mold, and the runtime
  libraries winit `dlopen`s (vulkan-loader, wayland, libxkbcommon, …); `devenv.nix` sets
  `LD_LIBRARY_PATH` in `enterShell`.
- Build: `cargo build -p mslicer`. `default-members = ["mslicer"]`, so plain `cargo build` /
  `cargo run` also target the app. Incremental builds take ~40s; `[profile.dev]` compiles all
  dependencies and the slicing/format crates at `opt-level = 3`.
- Run: `./target/debug/mslicer [mesh]`. No argument starts with an empty scene. The first frame
  takes a few seconds (wgpu adapter init).
- `[patch.crates-io] rustix = "vendor/rustix"` removes a cfg emission that nightly rejects. Do not
  drop the vendor copy without dropping the patch.
- Tests: `cargo test -p <crate>`; the format and slicing crates hold most of the suite.

## Verifying rendering changes

There is no synthetic-input tooling on this machine (no xdotool/ydotool/wtype), so the renderer is
only observable as pixels. Use `scripts/viewport-smoke.sh` (also exposed as the `mslicer_viewport`
tool and described by the `mslicer-viewport` skill):

```
scripts/viewport-smoke.sh --compare-empty   # build, launch, screenshot, diff, scan the log
```

For interaction-dependent behaviour (gizmo drags, hover), add temporary instrumentation to the app
(an extra arg, a once-per-frame system, a log line), verify, then remove it and re-grep for
residue before committing.

## bevy port map

- `mslicer/src/render/scene/mod.rs` — the bevy scene (`sync_scene`): models, supports, build plate,
  bed grid, camera-target marker, hover/face picking, material assignment.
- `mslicer/src/render/scene/material.rs` — model material (overhang shading, render styles,
  per-face selection, out-of-bounds tint, back-face colour).
- `mslicer/src/render/scene/gizmo.rs` — interactive transform gizmo and drag → `Model.mesh` sync.
- `mslicer/src/render/camera/mod.rs` — mslicer camera state ↔ bevy transform/projection (Z-up).
- `mslicer/src/render/slice_preview/`, `mslicer/src/render/interface/basis.rs` — existing wgpu
  pipelines drawn through an offscreen canvas + blit, because bevy_egui's egui pass has neither a
  depth attachment nor MSAA.
- `bevy-viewport-scoping.md` — design record of the port, with the measurements behind each step.

## Invariants that bit us

- mslicer composes `T * S * R`; bevy's `Transform` is `T * R * S`. Rendering writes
  `GlobalTransform` directly from `Mesh::transformation_matrix()`, and `sync_scene` runs in
  `PostUpdate` after bevy's propagation so the write survives the frame. Still mirror the same
  fields into `Transform`: bevy's gizmo snapshots and writes `Transform`, so a stale one makes
  every drag start from the origin and reset the model.
- The gizmo overlay camera copies the interaction camera's viewport but not its projection, and it
  renders into the window. Clamp overlay viewports to the window size; the egui layout rect trails a
  resize by a frame, and an oversized scissor makes wgpu abort the app.
- bevy_egui renders into the main pass, so pipelines needing depth/MSAA must render offscreen
  first.

## Conventions

- User-facing changes get an entry in `Changelog.md` under `v0.10.0 — Coming Soon™`.
- `cargo build -p mslicer` must pass before committing. Keep commits single-purpose; split
  mechanical porting from behaviour fixes.
- No debug instrumentation, screenshots, or absolute local paths in commits. `target/` is ignored;
  the smoke script writes captures there.
