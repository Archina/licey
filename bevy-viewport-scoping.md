# Scoping: port the viewport to bevy

Question: can the 3D viewport be re-implemented on bevy while the rest of mslicer keeps its
egui UI, so we can reuse bevy's gizmos and higher-level rendering and mirror models as
entities?

Scope of the survey: read-only. All `file:line` anchors are from the working tree at
`b1d5841` + the uncommitted gizmo work. No builds were run.

## Verdict

**Viable, but the seam is not "the viewport".** The real change is *who owns the window*:
bevy must own the winit event loop with `bevy_egui` hosting the existing panels, because
the opposite (bevy as a guest inside eframe's loop) runs into winit's one-`EventLoop`-per
-process rule. What you actually keep is the ~10.2k LOC of egui UI code, unchanged; what
you replace is `main.rs`'s eframe bootstrap (~50 lines), the host-owned `RenderState`
plumbing, and the viewport's ~3.9k LOC of hand-rolled wgpu. **egui itself survives** — see §9
for the follow-up: the widget surface this app actually uses has no stable bevy equivalent.

Three specific hopes, checked:

| Hope | Verdict | Why |
|---|---|---|
| Reuse existing bevy gizmo components | **Yes, better than expected** | bevy 0.20 ships an interactive `bevy_gizmos::transform_gizmo` module built in; `transform-gizmo-bevy` also exists but still targets bevy 0.19 |
| Get higher-level rendering | **Partly** | SSAO/FXAA/tonemapping/MSAA/shadows come free (replaces ~1k LOC of post passes); mslicer's overhang shading, per-face selection bitmask and `model_id`/face G-buffer need a custom `Material` + a picking path |
| Sync models ⇄ bevy entities | **Yes, with rules** | One id→`Entity` table plus explicit writeback rules for undo, `update_oob`, support transforms; the impedance is dual ownership, not the ECS |

## 1. What "the viewport" actually is

`mslicer/src/render/**` is 4315 LOC + 567 LOC of WGSL (14 files), all of it driven from
egui's paint list, none of it scheduled (`render/mod.rs:45-63` registers pipelines into
`egui_wgpu` `callback_resources`).

| Concern | LOC | Anchors | Bevy replacement |
|---|---|---|---|
| Model rendering (`base → ssao → blur → lighting → fxaa → composite`) | 2024 | `render/workspace/model/**` less `pick.rs` | Scene + `Mesh3d`/`MeshMaterial3d`, built-in `Msaa`/`ssao`/`fxaa`/tonemapping, custom `Material` for the mslicer-specific bits |
| Picking (`Rg32Uint` id/face G-buffer + 1×1 readback) | 125 | `render/workspace/model/pick.rs`, `bindings.rs:72-79` | `MeshRayCast` (`RayMeshHit.triangle_index`) |
| Lines (build plate, grid, normals, debug) | 465 | `render/workspace/line/**` | `Gizmos`/`LineList` meshes |
| Points (instanced spheres at camera target) | 273 | `render/workspace/point/**` | Mesh instances |
| Supports (preview mesh, alpha blended) | 136 | `render/workspace/support.rs` | `Mesh3d` |
| Basis/orientation widget | 184 | `render/interface/basis.rs` | `Gizmos` or a second gizmo view |
| Camera + spacenav | 290 | `render/camera/**` | `Camera3d` + custom controller; math is pure CPU and portable as-is |
| Callback wiring | 112 | `render/workspace/mod.rs` | `EguiBevyPaintCallbackImpl` re-wrapping |
| Plumbing (buffers, uniforms, bind-group helpers) | 296 | `render/{mod,consts,util}.rs` | Mostly obsolete under bevy |
| **Slice layer viewer + compute decompress** | **409** | `render/slice_preview/**` | *Not the 3D viewport* — a 2D layer viewer; keep on raw wgpu for now |

(rows sum to 4314; the last LOC is module declarations under `render/`)

Total UI code that stays: **10 184 LOC** (`core/**` 3281 + `interface/**` 6903); whole
`mslicer/src` = 16 990 LOC. So this replaces ~23% of `mslicer/src` (4315 minus the 409 LOC
slice viewer), not a port.

The viewport's UI half (`interface/panels/viewport.rs`, 456 LOC) is already portable: camera
movement, tool click handling, hover overlay and the transform gizmo are plain egui. The
non-portable part is the channel to the GPU.

## 2. The decisive constraint: one event loop per process

winit's `EventLoopBuilder::build` documents *"the EventLoop must be created on the main
thread, and only once per application"* and enforces it with a `static EVENT_LOOP_CREATED`
guard returning `EventLoopError::RecreationAttempt`
([winit v0.30.13 `src/event_loop.rs:82-93,118-120`](https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/src/event_loop.rs)).
`eframe::run_native` owns that loop today (`main.rs:74`); bevy's `WinitPlugin` builds its
own unconditionally
([bevy v0.20.0 `crates/bevy_winit/src/lib.rs:12,63-64,85,156`](https://raw.githubusercontent.com/bevyengine/bevy/v0.20.0/crates/bevy_winit/src/lib.rs)).

So the two hosts cannot both be real applications in the same process. There is no
`bevy-inside-eframe` mode; there is only `egui-inside-bevy`.

## 3. Integration options

### A. Bevy owns the window; the existing UI runs under `bevy_egui` — **recommended**

bevy 0.20.0 + `bevy_egui` 0.43.1 both target the egui 0.36 line the UI is already on
(`Cargo.toml:44-52`), including `egui_dock 0.21.1`, `egui_extras 0.36.2`, `egui_plot 0.37`,
`egui-phosphor 0.14` — every one of them declares `egui ^0.36`
([egui_dock](https://docs.rs/crate/egui_dock/0.21.1), [egui_plot](https://docs.rs/crate/egui_plot/0.37.0),
[egui-phosphor](https://docs.rs/crate/egui-phosphor/0.14.0)).

The existing `render/**` pipelines do **not** have to be rewritten on day one: bevy_egui
exposes `EguiBevyPaintCallback` / `EguiBevyPaintCallbackImpl`, documented as *"Callback to
execute custom 'wgpu' rendering inside `egui_pass`"*, and `TrackedRenderPass::wgpu_pass()`
hands you the native `wgpu::RenderPass`
([bevy_egui render docs](https://docs.rs/bevy_egui/0.43.1/bevy_egui/render/index.html)).
The drawing bodies survive; the `egui_wgpu` `callback_resources` model does not, so each
callback's resource ownership moves into bevy `Resource`s and its `prepare`/`paint` split is
re-wrapped. bevy_egui silently drops foreign `PaintCallback`s (*"Unsupported Egui paint
callback type"*), so all four callbacks must be re-wrapped — no half-measure.

eframe surface to delete: `main.rs:74-130` (`run_native`, `NativeOptions`, viewport builder,
creator closure), `core/mod.rs:144-171` (`eframe::App::ui` + unconditional
`request_repaint`), `core/mod.rs:160-170` (`Drop` on eframe shutdown). eframe `Storage` /
`App::save` / `load` are unused; persistence is manual `config.toml`, so nothing is lost.
Window title (`ViewportCommand::Title`, `core/mod.rs:130`) → `Window::title`; quit
(`shortcuts.rs:159`) → `AppExit`; panic hook on the `task_thread` name (`main.rs:32-36`) is
host-independent.

Effort, days-to-weeks, not a rewrite: the mechanical part is re-wrapping the 4 `CallbackTrait`
impls (`workspace/mod.rs:39,77`, `slice_preview/mod.rs:31`, `basis.rs:140`) and moving
`MeshBuffers` out of `App.gcx()`.

### B. Bevy as a windowless guest inside eframe's loop

Technically constructible: `.disable::<WinitPlugin>()` +
`WindowPlugin { primary_window: None }` gives a windowless `App` (bevy's
[`headless_renderer` example](https://raw.githubusercontent.com/bevyengine/bevy/v0.20.0/examples/app/headless_renderer.rs)),
bevy can adopt eframe's device through `RenderCreation::Manual`
([`RenderCreation`](https://docs.rs/bevy_render/latest/bevy_render/settings/enum.RenderCreation.html);
`egui_wgpu::RenderState` exposes public `device`/`queue`/`instance` fields), and bevy can
render into a texture view created outside bevy and registered as
`ManualTextureViews` → `RenderTarget::TextureView`
([`ManualTextureViews`](https://docs.rs/bevy_render/0.20.0/bevy_render/texture/struct.ManualTextureViews.html),
[`RenderTarget`](https://docs.rs/bevy_camera/latest/bevy_camera/enum.RenderTarget.html)).
Both stacks resolve to wgpu 30.x, so no cross-device handoff is needed — which is good,
because there is no portable way to import a texture created by a *different* device.

Costs that make this worse than A: you must drive `App::update()` from
`eframe::App::ui` (the `ScheduleRunnerPlugin` mechanism, but no first-party doc covers a
foreign host); bevy's pipelined renderer runs on its own thread, so the texture you sample
during egui's frame is from a previous frame; and the mslicer `App` would have to hand out
its device instead of owning it. **Recommendation: try it only as a throwaway spike, not as
the migration target.** It is the only shape that matches "change nothing but the viewport".

### C. Keep the current stack

Do nothing. Note the current cost: the `WorkspaceRenderCallback { app: *mut App }` raw
pointer (`viewport.rs:247-251`, dereferenced `unsafe` in `render/workspace/mod.rs:49,67`
with `unsafe impl Send/Sync` at `:108-112`), hover data that is one frame stale by
construction (`pick.rs:120-124` writes state read next frame at `viewport.rs:54,76`), and a
composite pass that rewrites `frag_depth` (`composite.wgsl:10-16`) purely to keep egui's
gizmo overlay consistent with the offscreen depth.

## 4. What bevy actually gives you

**Transform gizmo, built in.** bevy 0.20 added an opt-in `bevy_gizmos::transform_gizmo`
module: *"Interactive transform gizmo for translating, rotating, and scaling entities …
allowing the user to click-and-drag to translate, rotate, or scale it"* —
`TransformGizmoPlugin`, `TransformGizmoCamera`, `TransformGizmoFocus`,
`TransformGizmoSettings/Mode/Space/Axis`, hover/drag state in `TransformGizmoState`. It
uses `ButtonInput<MouseButton>` + cursor position + `Ray3d` math and does **not** depend on
`bevy_picking`
([bevy_gizmos::transform_gizmo](https://docs.rs/bevy_gizmos/latest/bevy_gizmos/transform_gizmo/index.html)).
This replaces the uncommitted `transform-gizmo-egui` integration
(`state.rs:62-97`, `viewport.rs:260-425`) 1:1, including the local/global toggle and
snapping, and drops two third-party deps.

`transform-gizmo-bevy` 0.11.0 also exists, but its deps are `bevy_* ^0.19`; it would pin
the whole app one bevy release behind
([crates.io](https://crates.io/crates/transform-gizmo-bevy)).

**Picking with the face index.** `bevy_picking`'s `MeshRayCast` system parameter returns
`(Entity, RayMeshHit)` sorted nearest-first, and `RayMeshHit` carries
`triangle_index: Option<usize>`, `normal`, `barycentric_coords`, `uv` and world-space
`point` ([0.20 source](https://docs.rs/bevy_picking/0.20.0/src/bevy_picking/mesh_picking/ray_cast/intersections.rs.html)).
That covers `GeometryHit { model, face, support }` (`state.rs`, consumed at
`viewport.rs:54,76` and by the Orient tool at `viewport.rs:76-88`) without the GPU readback
state machine in `pick.rs`. Note `HitData` (the *event* path via `MeshPickingPlugin`) does
**not** carry the face index — only `camera/depth/position/normal/extra`
([docs](https://docs.rs/bevy_picking/latest/bevy_picking/backend/struct.HitData.html)) — so
use `MeshRayCast` directly rather than the picking events.

**Higher-level rendering.** bevy's PBR pipeline, `Msaa`, built-in
`ssao`/`fxaa`/tonemapping/shadows subsume the 982 LOC of
`render/workspace/model/pass/{ssao,blur,lighting,fxaa,composite}.rs` and the hand-written
`GBuffer` in `bindings.rs` (10 textures, MSAA-1, own `Depth32Float`).

## 5. Parity gaps to plan for

| mslicer behaviour | Where | Bevy path |
|---|---|---|
| `render_style` (flat/preview look), `overhang_angle` shading, `model_color` | `pass/base.rs:41-52`, `base.wgsl` | Custom `Material` (`#import bevy_pbr::…`); the WGSL logic ports, the binding model doesn't |
| Per-face selection bitmask via storage buffer | `pass/base.rs:59-83`, `selected.rs` | Vertex/instance attribute, or a storage buffer in the custom material |
| `model_id`/face written to `Rg32Uint` for picking | `bindings.rs:72-79`, `base.rs:396-405` | Not needed if `MeshRayCast` is used; needs `Bevel`-free mesh + `Aabb` |
| `POLYGON_MODE_LINE` for the grid | `main.rs:95`, `line/mod.rs:167` | `PrimitiveTopology::LineList` meshes (no device feature needed) |
| `IMMEDIATES` + `PRIMITIVE_INDEX` (only for slice-preview decompress + base pass) | `main.rs:96-97,102` | Retained if the raw-wgpu slice preview is kept; set via bevy `WgpuSettings.features` |
| MSAA-4 egui pass + `Depth24PlusStencil8` + `frag_depth` composite trick | `main.rs:80`, `composite.wgsl:10-16` | Obsolete: the viewport becomes a texture, overlays composite in 2D |
| egui texture manager for slice thumbnails | `util/management.rs:30-68` | `bevy_egui::EguiUserTextures` |
| OS drag-and-drop feeding `ctx.input.raw.dropped_files` | `main.rs:79`, `elements/drag_and_drop.rs:8-18` | **Unknown** — must verify bevy_egui forwards window file-drop events into egui raw input |

Untouched by any of this (no window/render dependency): `rfd` dialogs, `notify-rust`,
`open`, `windows-registry`, the `task/**` thread system + panic hook, `spacenav`, the
tracking allocator, `--install`, config persistence.

## 6. Model ⇄ entity sync

`Model` owns its GPU buffers today (`core/project/model.rs:41,146-150`), `Supports` too
(`supports.rs:23,126-133`), and `PreviewModel` clones them (`state.rs:158-159`); the slice
runner reaches into `render_state.device` (`core/slice/runner.rs:57-61`). Under bevy these
become `Assets<Mesh>` handles, which means:

- One `ModelId → Entity` map, plus a `Supports` entity per model (`supports.mesh()` builds
  one combined mesh, `supports.rs:70-125`).
- Transform is authoritative in `Model.mesh` (`slicer/src/mesh.rs:9-19`, `T*S*R` cached at
  `:143-152`); gizmo drags must write back through the platform-aware setters
  (`project/model.rs:222-235`) so `update_oob`, support transform propagation and
  `OutOfBounds` warnings still fire, not straight into `Transform`.
- Undo must keep the drag-capture pattern: one history entry per changed component on drag
  end (`viewport.rs:367-421`, `ModelAction` in `history.rs:71-83`).
- Geometry uploads are already per-edit and cached; invalidation points are
  `replace_mesh` (`project/model.rs:152-178`) and `Supports::invalidate_cache`
  (`supports.rs:29-53`).

This is the part where bugs will live. Keeping `Project` as the single source of truth and
treating entities as a derived view (push transform → entity each frame; pull only from the
gizmo) is the boring option and avoids two-way sync.

**Transform convention trap (confirmed in a Phase 1 attempt).** `slicer/src/mesh.rs:143-152`
composes the model matrix as `T * S * R`, while bevy's `Transform` is
`Mat4::from_scale_rotation_translation` = `T * R * S`
(`bevy_transform-0.20.0/src/components/transform.rs:267`), and `Transform::from_matrix`
decomposes under that assumption — so `T*S*R` round-trips only for uniform scale. Resolution:
keep the `position/rotation/scale` *fields* authoritative, drive the gizmo from a `Transform`
built from those fields (exactly what `gizmo_target` does today), and render the exact matrix by
setting `GlobalTransform` directly from `Model.mesh.transformation_matrix()`. Do not route the
render transform through `Transform`.

## 7. Risks, ranked

1. **bevy 0.20.0 is 8 days old** (published 2026-10-08), `bevy_gizmos::transform_gizmo` is
   new in it, and `transform-gizmo-bevy` lags at 0.19. Expect ecosystem churn and a
   dependency graph that moves under you.
2. **EguiBevyPaintCallback has no users in this codebase** and little ecosystem mileage;
   the whole "keep the slice preview on raw wgpu" de-risk rests on it. Verify early.
3. **Face-index picking parity** — resolved on paper via `RayMeshHit::triangle_index`, but
   supports are a *separate* mesh per model with a `FaceMap` (`supports.rs:70-125`), so the
   entity→(model, support) mapping must be re-derived.
4. **Custom material** for overhang shading + selection bitmask + per-model color; the only
   part where bevy's higher-level rendering doesn't hand you the answer.
5. **Dev-loop cost**: bevy is enormous next to the current stack; the debug binary is
   already 869 MB. Plan `dynamic_linking` for dev builds.
6. **File drag-and-drop** via bevy_egui is unverified (see §5).

## 8. Suggested phasing

- **Phase 0 — host swap, zero visual change.** bevy + bevy_egui + `bevy_gizmos`; wrap all
  the 4 existing `CallbackTrait` impls in `EguiBevyPaintCallbackImpl`; move `MeshBuffers`
  ownership off `egui_wgpu::RenderState`; delete the eframe bootstrap. Success = the app
  looks and behaves exactly as today. This alone proves or kills the hybrid.
- **Phase 1 — viewport scene.** `Camera3d` + controller, models/supports/points/lines as
  entities, `bevy_gizmos::transform_gizmo` for manipulation, `MeshRayCast` for hover;
  delete `ModelPipeline`, its six passes, `pick.rs`, `line/**`, `point/**`, `support.rs` and
  the `model_target` G-buffer.
- **Phase 2 — optional.** Basis widget onto `Gizmos`; slice preview onto bevy if the raw
  path annoys.

**Decisive experiment before committing to either A or B:** a throwaway spike that boots a
windowless bevy `App` on eframe's device (`RenderCreation::Manual`), renders one triangle to
a `ManualTextureView` created from eframe's device, steps it with `app.update()` from
`eframe::App::ui`, and shows it in egui. If that works, option B is real and the port can be
truly viewport-only; if it fights back, option A is the only shape and the "viewport only"
framing should be dropped.

## 9. Addendum: does egui survive a full move to bevy?

Only the egui surface this project actually uses is considered below. Counts are exact
call-site counts over `mslicer/src` (import lines excluded).

### 9.1 The used surface

| egui item | sites | hot spots |
|---|---|---|
| `DragValue` (draggable numeric) | **103** | dragger 11, slice_config 19, workspace 15, supports 9, phonograph 9, pcb 8, auto_layout 7 |
| `ui.button` 79 + `Button::new` 20 + 2 selectable | **101** | top_bar 18, sliced 14, remote_print 11 |
| `on_hover_text/on_hover_ui/on_hover_cursor` | **52** | every panel |
| `Grid::new` 9 + local `grid()` 37 | **46** | forms everywhere; helper `components/mod.rs:33` |
| `CollapsingHeader::new` 9 + `ui.collapsing` 30 | **39** | sliced 10, workspace 9 |
| `ui.selectable_value` 27 / `selectable_label` 3 | **30** | workspace 7, phonograph 4 |
| `ComboBox` | **23** | workspace 6, sliced 3 |
| `ui.menu_button` 17 (+13 app-local `menu_button` helper) | **30** | `top_bar.rs:311` |
| `ui.checkbox` 11, `toggle_value` 2 | **13** | |
| `TextEdit` (singleline) | **7** | names, ports, rename |
| Custom painting: `painter*` 24, `allocate_exact_size` 7, `allocate_painter` 1, `ui.interact` 1, `Mesh` pie chart 1, wgpu callbacks 4 | **≈38** | viewport, outliner, chart, top bar spinner, sliced preview |
| Hand-rolled `Popup` framework (`interface/popup.rs`) | **20** | all 10 tools + confirms |
| `egui_plot::Plot` (Line/Points/VLine) | **5** | FPS, surface area, defects, one with draggable control points (`slice_config.rs:598-663`) |
| `TableBuilder` 3 + `Column` 13 | **3 tables** | `slice_config.rs:434,534`, `pcb.rs:71` |
| `ProgressBar` 5 + `Spinner` 1 | **6** | sliced, remote_print, top_bar |
| `DragAndDrop` payload API | **5** | model/collection reordering + insertion line |
| `ScrollArea::vertical` 2, `Area` 3, `Window` 2, panels (top/right/bottom) 3, `CentralPanel` 1 | **11** | dock shell, popup host, 3 viewport overlays |
| `color_edit_button_rgb` | **2** | `workspace.rs:182`, `model.rs:307` |
| `Slider::new` | **1** | `sliced.rs:465` — and it is custom-painted, not the stock widget |
| `Image` 2 + `include_image!` 1, `hyperlink_to` 4 + `open_url` 1 | **8** | welcome, sliced preview |
| `egui_dock` `DockArea`/`DockState`/`TabViewer`/`Tree<Tab>` | 1 + machinery | `interface/mod.rs:19-26`, `panels/mod.rs`, `panels/tabs.rs`, `core/config/ui.rs:15` |
| `egui_tracing::Logs` | 1 | `panels/logs.rs:6` |
| `egui-phosphor` glyphs | 21 files | icons concatenated into labels via `concatcp!` |
| Layout containers (`horizontal`/`with_layout`/`vertical_*`/`wrapped`/`centered_and_justified`) | **≈142** | everywhere |

Total porting surface ≈ **1100-1200 `ui.` call sites**; densest files `sliced.rs` 226,
`workspace.rs` 153, `slice_config.rs` 116, `remote_print.rs` 96, `top_bar.rs` 69.

Not used at all: `egui::Modal`, `context_menu` (deliberately disabled at
`interface/mod.rs:24`), `radio_value`, the `dnd_*` helpers.

### 9.2 bevy 0.20 coverage of exactly those items

bevy_ui is retained/ECS (Flexbox + CSS Grid, `Node` components, `Interaction` + picking
backend, no immediate-mode API).

| used item | bevy 0.20 |
|---|---|
| Button / menu button / checkbox / toggle | `bevy_ui_widgets` Button, MenuButton, MenuItem, Checkbox (+ `FeathersButton`/`FeathersToggleSwitch`) |
| `DragValue` (101 sites — the workhorse) | `FeathersNumberInput`, scrubbable as of 0.20 |
| `color_edit_button` (2 sites) | `FeathersColorInput` (+ wheel/plane/slider/swatch children) |
| ComboBox | `FeathersSelect`, or ListBox + MenuPopup + popover |
| TextEdit | `bevy_ui_widgets::TextInput` (on `bevy_text::EditableText`) |
| Slider | `bevy_ui_widgets::Slider` |
| CollapsingHeader | `FeathersDisclosureToggle` |
| ScrollArea | `bevy_ui_widgets::ScrollArea` + `Scrollbar` |
| Window/Area + the Popup framework | `bevy_ui_widgets` Dialog/modal/popover; `FeathersFloatingDialog` |
| tooltips | `bevy_ui_widgets::popover` |
| Image | `ImageNode` + `AssetServer` |
| **docking / tabs / layout persistence** | **none** — open issue [#25958](https://github.com/bevyengine/bevy/issues/25958): headless split-pane and tabs landed, dock tree / re-parenting / drag-docking **not done** |
| **tables** (`TableBuilder`, striped/resizable columns) | **none** (CSS-Grid layout primitive only) |
| **plots** (`egui_plot`, incl. draggable control points) | **none** first-party; community is stale (`bevy_plot` last release 2022-02-19) |
| **ProgressBar / Spinner** | **none** |
| **hyperlink** | **none** |
| **general drag-and-drop payloads** (model reordering) | **none** (only the tab widget has drag) |
| **tracing log panel** | **none** first-party (`bevy_debug_log` 0.9.0 exists, bevy version unverified) |
| **custom painting + pointer ownership** (≈38 sites) | no widget analogue; becomes hand-built nodes + `bevy_picking` |
| `egui-phosphor` | not an egui dependency — same glyph font loads into bevy `TextFont` |

`bevy_ui_widgets` ships button/checkbox/radio/slider/text_input/list/menu/dialog/modal/
popover/scrollarea/scrollbar/tabs and is **feature-gated and self-declared unstable**
("patterns shown here are likely to change substantially as the `bevy_ui_widgets` crate
matures"). `bevy_feathers` is **experimental** and editor-oriented.

### 9.3 Verdict: keep egui

1. **Everything the app is actually built from is either missing or unstable in bevy.**
   The three heaviest non-form features — docking with serde layout persistence, the two
   data tables, and the plots — have no first-party equivalent, and the widgets that do
   exist live in a crate that tells you its API will change substantially.
2. **The cost is not where it looks.** Color pickers (2 sites) and sliders (1 custom-painted
   site) are the cheapest things to port. The expense is `DragValue`-driven forms (103),
   grids (46), collapsing sections (39), custom-painted rows/charts (≈38) and the popup
   framework (20) — plus the model inversion itself: immediate-mode closures that borrow
   `&mut App` and read/write state inline become spawn/update systems with component state.
   The 1134 `ui.` call sites (measured) understate that.
3. **There is nothing to gain.** `bevy_egui` 0.43.1 sits on bevy 0.20 and egui 0.36 — the
   exact stack already in use — so a bevy host keeps the entire UI layer (10 184 LOC) as-is,
   and the rewrite budget goes to the viewport, which is the part you actually wanted bevy
   for.

Reusable regardless of host: `EventCollector`/tracing layer, `TaskManager`, all
config/serde structs, plot data. The widget layer is what a rewrite would cost.

Revisit bevy_ui when `bevy_ui_widgets`/`bevy_feathers` stabilize *and* the dock-tree work in
#25958 lands.

Sources for this section: [bevy_ui 0.20](https://docs.rs/bevy_ui/0.20.0/bevy_ui/index.html),
[bevy_ui_widgets 0.20](https://docs.rs/bevy_ui_widgets/0.20.0/bevy_ui_widgets/),
[bevy_feathers](https://docs.rs/bevy_feathers/latest/bevy_feathers/),
[bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/),
[standard-widgets example](https://bevy.org/examples/ui-user-interface/standard-widgets/),
[docking tracking issue](https://github.com/bevyengine/bevy/issues/25958).

## 10. Phase 0 outcome (executed)

Phase 0 is implemented on top of commit `041fb5a` (the previously-uncommitted gizmo work, which
the first delegation committed as a baseline). All migration work is **uncommitted**.

### What changed

| Area | Change |
|---|---|
| Manifests | `bevy 0.20` (+ `bevy_egui 0.43.1`) added; `eframe`/`egui-wgpu` removed; `winit 0.30` added as a direct dep (window icon) |
| `main.rs` | bevy `App` replaces `run_native`; `WindowPlugin` with title/inner size 1920x1080/centred; MSAA-4 and the old custom device limits dropped in favour of bevy's adapter-derived limits; `EguiPlugin`; startup system builds the mslicer `App`; per-frame system runs the existing UI |
| `core/mod.rs` | `App` split into `App { core: AppCore, render: RenderResources }` with `Deref/DerefMut`, needed to satisfy the borrow checker; per-frame order preserved; save-on-exit preserved |
| `render/mod.rs` | New `Canvas` (window-sized MSAA-4 colour + resolve + `Depth24PlusStencil8`), `Blitter`, `blit()`; format pinned to `Rgba8Unorm` |
| `render/{workspace,slice_preview,interface}` | The 4 `egui_wgpu::CallbackTrait` impls re-implemented on bevy_egui's paint callback; they render into the shared canvas in `update` and blit in `render` |
| UI files | ~15 files touched for borrow-only destructuring (`let AppCore { .. } = &mut app.core`) — no logic changes |
| New file | `mslicer/shaders/blit.wgsl` |

### Why a canvas instead of the paint callback directly

bevy_egui's egui pass has **no depth attachment and no MSAA**
(`depth_stencil_attachment: None`, `get_unsampled_color_attachment()`), while every mslicer
final-stage pipeline is built with `sample_count: 4` + `Depth24PlusStencil8`. So the stages
render into one shared offscreen canvas (which also preserves the inter-callback depth
sharing the old egui pass provided) and the resolved image is blitted into the egui pass.
`DefaultPlugins.disable::<PipelinedRenderingPlugin>()` is required by the existing
`*mut App` callback pattern.

### Verified

- `cargo build -p mslicer`: clean (incremental 33 s; the pre-handoff state had 101 errors).
- Runs on the real GPU (`Acquired wgpu device: "AMD Radeon Graphics (RADV RENOIR)"`), loads
  `teapot.stl` (9438 faces), no panics, no wgpu validation errors, no "Unsupported Egui paint
  callback type".
- **Viewport renders correctly**, proved by measurement rather than eyeballing: the teapot
  occupies 57x34 px, which is exactly `17.58 x 10.93 mm` x `3.33 px/mm` for the default
  camera (distance 150, fov π/2, 1000 px viewport); an ASCII rendering of that region shows
  the body, spout, handle and specular highlight. Build-plate grid and volume outline render,
  the basis widget shows the correct axis colours (`#CF0001` red, `#1051FB` blue), viewport
  toolbars, dock panels and menu bar all present.
- The pre-migration app was rebuilt from `041fb5a` in a worktree and also launches; the same
  viewport elements (wireframe plate, tiny teapot, 3-icon toolbar, axis gizmo, same tabs) are
  present in both.

### Not verified (be honest about these)

- **Pixel-exact equivalence.** The captures were confounded: the pre-migration window captured
  at 1199x767 and the migrated one at 2063x1180, so window geometry/scale differed between the
  two runs and no controlled A/B was possible. Both show the same viewport elements, but
  "visually identical" is still open — someone should compare them side by side on the real
  display.
- **Interactive paths.** This session has no input-injection tooling (no xdotool/ydotool/dotool,
  Wayland), so gizmo drags, clicks, text input and file drag-and-drop were exercised only by
  code inspection. OS drag-and-drop was verified to be forwarded by bevy_egui.
- **Window sizing/positioning behaviour**, given the capture-size discrepancy above.

### Deliberate deviations to accept or reject

1. **Canvas is `Rgba8Unorm` + gamma→linear blit** (rather than an sRGB pass-through).
   Opaque pixels round-trip byte-exactly; canvas pixels with alpha < 1 blend against the egui
   background in linear rather than gamma space.
2. **Pipelined rendering disabled** (required by the `*mut App` pattern + single canvas).
3. **wgpu limits are adapter-derived** — `WgpuSettings.limits` is ignored at the default
   `WgpuSettingsPriority::Functionality`, so the old explicit `max_color_attachment_bytes_per_sample: 40`
   / `max_immediate_size: 4` are gone (adapter limits are a superset). Requested `features` are
   still honoured.
4. **Window icon** is restored through `bevy::winit::WINIT_WINDOWS` + a direct `winit` dep,
   because bevy 0.20 has no window-icon API.
5. **Log filter default changed `OFF` → `WARN`.** The old default hid all third-party logs —
   including the blit shader compile failure that made the viewport silently empty during
   development.

### Phase 1, attempt 1 (not landed; architecture proven)

The attempt built the smallest end-to-end path, measured it, then reverted everything rather
than ship a regressed viewport — the tree is back to Phase 0 and `cargo build -p mslicer` is
clean. What it established is worth keeping:

**Proven by measurement**
- A second `Camera3d` with `RenderTarget::Image` + `bevy_egui::EguiUserTextures::add_image` +
  `painter.image()` inside the docked Viewport tab **works alongside**
  `disable::<PipelinedRenderingPlugin>()` and the Phase 0 canvas/blit callbacks — no panics, no
  wgpu validation errors. A pure-green clear filled 1,436,818 px spanning (365,96)-(1912,1029).
- Camera mirroring is exact: mslicer `Camera` → `Transform::looking_at` + `PerspectiveProjection`
  renders the teapot at 58x34 px (expected 57x34 at 3.33 px/mm).
- Models as `Mesh3d`/`MeshMaterial3d` entities work, geometry cached per `MeshId`, transform
  pushed from `model.mesh.transformation_matrix()`.

**Traps for the next attempt**
- `Mesh::insert_attribute(ATTRIBUTE_POSITION, Vec<f32>)` **panics** (`bevy_mesh-0.20.0/src/mesh.rs:472`); it needs `Vec<[f32; 3]>`.
- `run_app` runs in `EguiPrimaryContextPass` = `EguiPostUpdateSet::EndPass`, i.e. inside
  `PostUpdate` *after* transform propagation and visibility. Scene sync that must affect the
  same frame has to be its own `Update` system.

**Open gaps**
1. `bevy_gizmos` rendered **0 near-white pixels** in the viewport over 640 frames with the config
   store's default group pointed at the camera's `RenderLayers` (both layer 0 and 1) — not
   root-caused. `LineList` meshes are the sanctioned fallback for the plate/grid/normals.
2. `bevy_gizmos::transform_gizmo` reads `Window::cursor_position()` and converts through
   `Camera::logical_viewport_rect()`, which for `RenderTarget::Image` is the *image* rect — so in
   a docked tab it hit-tests where it is not drawn. Fix (designed, unlanded): a second
   window-space "interaction camera" (`viewport` = the egui rect, empty `RenderLayers`,
   `TransformGizmoCamera`).
3. The `T*S*R` vs `T*R*S` convention difference — see §6.

### Phase 1, attempt 2 (landed: scene + picking)

Attempt 1's design was kept and the gaps closed. New module `mslicer/src/render/scene/mod.rs`
(~700 LOC) owns the whole viewport scene:

- `ensure_scene` (`Update`) creates/resizes the `Image` the viewport camera renders into,
  registers it with `EguiUserTextures`, spawns the `Camera3d`, and spawns one bare entity per
  `SceneNode` (model, supports, build plate, normals, support-debug lines, camera-target marker,
  support preview).
- `sync_scene` (`PostUpdate`, **after** `TransformSystems::Propagate`) fills those entities in:
  cached `MeshId`-keyed bevy meshes, `GlobalTransform` set directly from
  `Mesh::transformation_matrix()` (never through `Transform`, see §6), and a `StandardMaterial`
  (`unlit: true`, double-sided) per node for colour. Running after propagation is what stops
  `sync_simple_transforms` from overwriting the exact matrix with the identity `Transform` that
  `Mesh3d` requires — and it removes the need for the `Transform` round-trip entirely.
- `pick_hover` (`Update`) drives `MeshRayCast` from `Camera::hovered_ray` and writes the same
  `GeometryHit` the UI reads.
- Scene entities and the camera live on `RenderLayers` layer 1 so the primary (window) camera,
  which exists only to host the egui context, does not draw the scene onto the window.

Superseded and deleted: `workspace/line/**`, `workspace/point/**`, `workspace/support.rs`,
`workspace/model/pick.rs`, `ModelPipeline`'s viewport half (`prepare`, `Target::Viewport`, the
viewport `MultiStage`, the `Rg32Uint` picking target in `bindings.rs`/`base.rs`/`base.wgsl`), the
dead `Model::try_get_buffers` / `Supports::{get_buffers,try_get_buffers}` / `AppCore::view_projection`
/ `Selected{,Supports}` helpers, and `workspace/{line,point,support}.wgsl`.

`ModelPipeline` itself is **kept**: the sliced panel's offscreen model preview
(`process_previews` → `preview.rs` → `PreviewRenderCallback`) still renders through it, so only
its viewport half could go. Same reason `interface/basis.rs` stays — it is the live basis widget,
unrelated to the workspace scene.

Measured (1920×1080 window, `ppp = 1.0`): teapot bbox **58×33 px** at (1110,539)–(1167,571)
(expected 57×34 at 3.33 px/mm); viewport background exactly `#090909`; 32,163 line-coloured
pixels spanning the full 1548 px viewport width (build plate + bed grid); `MeshRayCast` through
the viewport centre returns `model 0, face 3311` at distance 143.16.

Dropped (not expressible with `StandardMaterial`, all non-default so invisible at defaults):
overhang shading, the `Normals`/`Triangles` render styles, per-face selection highlight (Orient
hover and selected-supports ranges), the out-of-bounds red tint, and the back-face grey. A custom
`Material` porting `base.wgsl` restores them; it is the one remaining parity item.

**Independently re-verified after landing** (separate session, on the landed tree): clean build from
scratch; running with the teapot logs 0 errors; viewport background samples exactly `(9,9,9)` at
interior points; the teapot measures 58x34 px at (1110,538); 51,704 px above 15% luminance and
34,324 above 40% confirm the plate/grid/model draw; `GlobalTransform::from(...)` is fed straight
from `transformation_matrix()` with no decompose.

### Phase 1, increment 3 (landed: the model material)

The dropped render paths are back, as a bevy custom `Material` rather than a pass of their own:
`mslicer/src/render/scene/material.rs` (the material, its params and the selection bitmask) and
`mslicer/src/render/scene/model.wgsl` (the shader, registered as an embedded asset), used by the
`Model` and `Supports` nodes. The sync architecture is untouched — no `Transform` round-trip, the
same `MeshId`-keyed mesh cache — because everything that varies per node now travels as material
data:

| Old `BasePass` uniform / buffer | Now |
|---|---|
| `transform`, `model_transform` (vertex stage) | bevy's `world_position` from the direct `GlobalTransform` |
| `model_color` | `ModelParams::model_color` (display/sRGB space, as before) |
| `build_volume` | `ModelParams::build_volume` |
| `render_style` | `ModelParams::render_style` |
| `overhang_angle` | `ModelParams::overhang_angle` (`f32::from_bits(u32::MAX)` sentinel when off) |
| `id` (picking target), `selected_offset` | dropped: picking is `MeshRayCast`, and the bitmask buffer is per material |
| `selected` storage buffer | one `ShaderBuffer` per material, `#[storage(1, read_only, visibility(fragment))]` |
| `primitive_index` for the picked face | unchanged: `@builtin(primitive_index)` is the draw's triangle index, `mesh_from_slicer` keeps `Mesh::faces()` order |
| `screen_normal(world_position)` | unchanged: `dpdx`/`dpdy` of the world position (the scene meshes carry no normals) |

Two deliberate differences from the old pipeline: the fragment stage works in display space (the old
target was non-sRGB, bevy's is `Rgba8UnormSrgb`, so the hand-picked `OVERHANG_COLOR`,
`SELECTED_COLOR` and back-face grey keep their old bytes and the stage decodes once at the end), and
`Material::specialize` sets `cull_mode: None` to keep the old double-sided `PrimitiveState::default()`.
`RenderStyle::RandomTriangle` ("Triangles") is a per-face *random colour*, not `POLYGON_MODE_LINE` —
the old model pipeline never set a polygon mode; `POLYGON_MODE_LINE` belonged to the always-line
debug pipeline, so no pipeline permutation is needed for it.

**Measured** (`spectacle -b -n -f`, 1920×1080 screen, viewport crop `1548x933+365+96`):

- **No regression, same colour.** A temporary env-var override rendered one build twice, once with
  the old `StandardMaterial` and once with `ModelMaterial`, both with the model colour forced to
  `LinearRgb::splat(0.6)`: **0 differing pixels** in the viewport (`compare` of the two captures).
- **No regression, default settings.** Baseline capture of the landed build vs the ported build:
  the only viewport differences are the model's own 1172 pixels — mslicer randomises the model hue
  per run (`Model::with_random_color`). The teapot's fill area is 839 px in both, and inside its
  58×34 box every other colour is byte-identical (656× `(9,9,9)`, 50× `(187,187,187)`,
  50× `(138,138,138)`, 44× `(188,188,188)`); background and plate/grid unchanged.
- **Out of bounds** (`MSLICER_FORCE_VOLUME=1`): 839 px of exactly `(255,0,0)` vs 0 by default.
- **Back-face grey** (model mirrored, so every triangle is back-facing): 843 px of exactly
  `(128,128,128)` in the teapot box vs 3 by default, and the model fill drops to 1 px.
- **Overhang shading** (config `overhangs = [true, 180.0]`, forced colour): 205 px in the teapot box
  with `g - b ≥ 25` vs 0 by default, and the fill walks from `(203,203,203)` towards
  `(171,166,97)` as `mix(model_color, OVERHANG_COLOR, 1 - smoothstep(0, angle, acos(-n.z)))`
  predicts (e.g. `(194,192,171)`). At the default 30° only grazing pixels tint, as the shader's
  threshold implies for a model seen from above.
- **Selection highlight**: forcing faces `0-4718` paints **573 px** of exactly `(51,51,255)`
  (the old shader's display `(0.2,0.2,1.0)` written raw); forcing the single face `3311` — the face
  `MeshRayCast` reports through the viewport centre — paints 6 bluish px there and 0 elsewhere,
  which cross-checks `primitive_index` against the picker.
- **Render styles** (config): `Normals` and `RandomTriangle` both drop the flat fill to 0 px and
  produce 1163/1174 distinct colours in the teapot box (89 at default), 1172 px changed.

**Not verified.** Selected-*supports* highlighting: the range encoding is exercised (the same
`FaceSelection::insert_range` call), but no supports mesh can be built without UI input, so
`SelectedSupports → FaceRange` was only code-reviewed. The Orient-tool hover was driven by the
forced-face path, not a real pointer (this session has no input injection). At *default* settings the
teapot's STL sits ≥10 µm below the bed plane, so the restored out-of-bounds tint paints 8 px (two
2×2 quads at the bottom silhouette) red where the previous build drew the model colour — the old
shader did the same, and a huge forced build volume does not change it (the test is
`pos.z < -0.01`, which is volume-independent).

**Independently re-verified after landing** (separate session, on the landed tree): clean build;
running with the teapot logs 0 panic/validation/error lines; the teapot measures 58x34 px at
(1110,538) inside the viewport; viewport background samples exactly `(9,9,9)`; the model's fill is
839 px of its run-randomised colour plus the grey shading values `(138,138,138)`/`(187,187,187)`.

### Phase 1, increment 4 (landed: bevy's transform gizmo)

The user chose to replace the `transform-gizmo-egui` integration with bevy's built-in gizmo.
`render/scene/gizmo.rs` (~360 LOC) is the bridge: `TransformGizmoPlugin`/`TransformGizmoFocus`,
`TransformGizmoSettings` (axis length, snap 0.1 mm / pi-32 / 0.1), `confine_cursor: false`.

- **Interaction camera.** `Camera3d` + `TransformGizmoCamera` + `Camera { is_active: false,
  clear_color: None, viewport: Some(rect) }`, re-pointed each frame at the mslicer camera and at the
  viewport panel's rect in physical pixels. This is what makes the gizmo pick where it draws:
  `Camera::viewport_to_world` resolves `Window::cursor_position()` against
  `Camera::logical_viewport_rect()`, which for an image render target would otherwise be the image.
- Overlay camera handling: bevy's private gizmo overlay camera is identified (the `Camera3d` that is
  neither ours nor the primary) and given `clear_color = Custom(Color::NONE)` +
  `CameraMainTextureUsages::default().with(COPY_DST)` so it gets its *own* main texture,
  `Tonemapping::None`, `DebandDither::Disabled`.
- Readback: `TransformGizmoState.entity` → `SceneNode::Model(id)`; pre-drag `position/rotation/scale`
  captured once, writes go through the platform-aware setters, and one `ModelAction` per changed
  component is committed on drag end. `state.gizmo_hot` still suppresses camera orbit; the mode /
  local / snap toolbar and `GizmoTransform` are unchanged.
- Removed: `transform-gizmo` + `transform-gizmo-egui` deps, `UiState::gizmo`, `GizmoDrag`,
  `paint_gizmo`, `gizmo_modes`, `gizmo_target`, `mint_matrix`.

**Two real bugs found and fixed en route**
1. The workspace's bevy features lacked **`bevy_gizmos_render`** — without it `GizmoRenderPlugin` and
   `TransformGizmoRenderPlugin` are never added, so `TransformGizmoPlugin` alone spawns no handles.
   This also explains attempt 1's "bevy_gizmos rendered 0 pixels" gap.
2. The overlay camera **shared the primary's main texture** (bevy keys main textures by render
   target) so its default clear blended a translucent veil over the viewport: background measured
   `#090909` -> `#0B0B0B`. Fixed with its own main texture plus a fully transparent clear.

**Measured** (programmatic selection via temporary instrumentation, since removed): translate X =
44 px of exactly `(255,0,125)` (trim 36x3 at (1149,562)), Z = 40 px of `(0,125,255)` (5x28 at
(1137,529)), Y = 0 because at the default camera azimuth `+Y` and `+Z` project onto the same screen
direction and the nearer Z handle occludes Y; rotate mode X = 91 px, Y = 161 px, Z = 161 px, all
exact; the control run with the overlay camera off had 0 axis pixels. Handle span matches
`axis_length 1.0 x (distance x 0.1) = 15 px/unit`.

No regression: teapot still 58x34 at (1110,538) with an 839 px fill, background exactly `(9,9,9)`,
and the whole-viewport diff against the overlay-off control is 1306 px confined to
`76x44+745+462` — the teapot ∪ handles box, 0 px differing with that box masked out.

**Needs a human**: drag feel, snapping, the mode/Local/Global toolbar toggles, one undo entry per
drag, hover highlighting, the hover-outside-the-viewport edge case, gizmo focus under a
supports-selection, and orthographic projection (mirrored generically, only perspective was run).

**Independently re-verified after landing** (separate session): clean build; deps and dead plumbing
gone (`grep` clean); teapot 58x34 at (1110,538); background `(9,9,9)` at two interior points;
0 panic/error/validation lines.

### Bug fix: model transforms reset when manipulated in the viewport (reported after increment 4)

**Symptom.** Transforms set in the Model tab were reset once the model was moved, rotated or
scaled in the viewport.

**Cause.** `sync_scene` only ever wrote `GlobalTransform` (set directly from the exact `T*S*R`
matrix to dodge the convention mismatch), so each scene entity's `Transform` component stayed at
the identity `Transform` that `Mesh3d`'s required components spawn it with. bevy's transform gizmo
snapshots `state.start_transform = *transform` when a drag begins
(`bevy_gizmos-0.20.0/src/transform_gizmo.rs:474`) and composes the drag from that snapshot, using
`GlobalTransform` only for placement and axis math. So every drag started from the origin with
identity rotation/scale, and `apply_gizmo_drag` wrote those origin-relative values straight back
into `Model.mesh`.

**Fix.** `sync_scene` now also maintains `Transform` from the same authoritative fields
(`model_transform(&SlicerMesh)`: nalgebra `Rotation3::from_euler_angles` -> `Quat`, the same
convention as the render matrix), guarded by `if *local != local_transform` so the component is not
dirtied every frame. `GlobalTransform` still receives the exact matrix directly, so rendering is
untouched — `Transform` exists only so interactive tools start from the real transform.

**Verified** with a temporary harness (there is no input injection here): with the model forced to
position `(10,-20,3)`, euler `(0.3,-0.2,0.1)`, scale `1.5`, the entity's `Transform` at rest
measured exactly that, and a fake drag (the +5 x the gizmo writes) left the model at
`(15,-20,3)` with rotation and scale preserved. Before the fix the drag base was identity, so the
same drag would have produced `(5,0,0)` with identity rotation and scale. The harness is removed;
the standard no-regression metrics were re-checked afterwards.

### Bug fix: gizmo viewport overflowed the window on resize

**Symptom** (reported): resizing the main window aborted the app —
`bevy_core_pipeline::upscaling` failed with
`Scissor Rect { x: 365, y: 66, w: 1549, h: 935 } is not contained in the render target (1910, 1002, 1)`.

**Cause.** `sync_gizmo_cameras` set the interaction camera's viewport from
`app.state.viewport.rect`, the egui layout rect in physical pixels. That rect trails the window by
a frame while it is being resized, and `bevy_gizmos_render` copies the interaction camera's viewport
onto the always-on-top overlay camera (`transform_gizmo_render.rs:437`), which renders into the
window — so the overlay's upscaling scissor no longer fitted the shrunk target.

**Fix.** `sync_gizmo_cameras` now clamps the rect to the window's physical size and sets the clamped
viewport on *both* the interaction and the overlay camera, so the plugin's copy order cannot matter.
It logs at `trace!` when the clamp bites.

**Verified** with a synthetic resize (the window resolution changed programmatically at runtime —
there is no input injection here): the clamp engaged with measured values
`rect [[365,66]]+[[1549,935]] -> [[365,66]]+[[955,814]]` against a 1320x880 target, and the reported
`set_scissor_rect` error no longer appears. Caveat: changing the resolution outside the compositor's
event path also provokes *other* transient validation errors (an egui-pass scissor computed from a
stale screen rect, and a depth/colour attachment size mismatch), which persisted when the change was
moved earlier in the frame — so a real WM-driven resize could not be simulated faithfully, and the
end-to-end check is the user's to make. Standard no-regression metrics were re-checked after cleanup
(58x34 teapot at (1110,538), background `(9,9,9)`, 0 error lines).

**Rejected experiment.** Dropping the overlay camera's private main texture and using
`ClearColorConfig::None` on the shared texture (to remove the non-default `CameraMainTextureUsages`
entry) measured the veil returning — background `(11,11,11)` — while the gizmo still drew
(44 px X / 40 px Z, unchanged). The overlay camera therefore keeps its own main texture and
transparent clear.

## Sources

Primary sources for external claims are linked inline. Repo anchors are `file:line` into
`/home/archina/Dev/licey` (branch `feature_egui.36` + uncommitted gizmo work).
