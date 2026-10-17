//! The transform gizmo: bevy's built-in [`bevy_gizmos::transform_gizmo`], bridged to mslicer.
//!
//! bevy's gizmo is the interaction authority — it owns hover/drag detection and writes the
//! focused entity's [`Transform`]. This module is the bridge around it, with three jobs.
//!
//! **An interaction camera, so the handles are picked where they are drawn.** The gizmo reads
//! `Window::cursor_position()` and converts it through `Camera::viewport_to_world`, which resolves
//! the cursor against `Camera::logical_viewport_rect()`. The viewport camera renders to an `Image`,
//! so its logical viewport rect *is* the image rect: window cursor coordinates would be read as
//! image pixels and each handle would be hit-tested somewhere the user is not pointing. The gizmo
//! therefore gets a second, window-space camera ([`InteractionCamera`]) whose [`Viewport`] is the
//! docked viewport's rect on the window, and whose `Transform`/`Projection` mirror the viewport
//! camera's. It is `is_active: false` — it draws nothing, it exists only so that
//! `viewport_to_world` turns window coordinates into the viewport's world rays.
//!
//! **The overlay camera's blit is confined to the handles.** `bevy_gizmos_render` spawns its own
//! always-on-top camera for the gizmo meshes and copies the gizmo camera's `viewport` and
//! transform onto it, which is exactly the rect the docked viewport image occupies on the window.
//! That camera draws after the egui pass and bevy blits its output back onto the window, so the
//! handles land on top of the viewport image. Two things keep that blit from touching anything
//! else: its clear is transparent (the blit is alpha-blended, so untouched pixels pass through)
//! and it owns a separate main texture (see [`mark_gizmo_overlay`]) so clearing it cannot wipe the
//! UI out of the texture it would otherwise share with the primary camera.
//!
//! **Selection and undo stay mslicer's.** The gizmo only ever sees a [`TransformGizmoFocus`] on
//! the scene entity of the single selected model, and the transform it writes on that entity is
//! translated back into the authoritative `Model.mesh` fields through the platform-aware setters,
//! capturing the pre-drag values once and committing one history entry per changed component when
//! the drag ends.

use bevy::{
    camera::{CameraMainTextureUsages, ClearColorConfig, Viewport},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    gizmos::transform_gizmo::{
        TransformGizmoCamera, TransformGizmoFocus, TransformGizmoMode, TransformGizmoPlugin,
        TransformGizmoSettings, TransformGizmoSpace, TransformGizmoState, TransformGizmoSystems,
    },
    prelude::*,
    render::render_resource::TextureUsages,
    window::{PrimaryWindow, Window},
};
use bevy_egui::PrimaryEguiContext;
use nalgebra::{Quaternion, Rotation3, UnitQuaternion, Vector2, Vector3};

use crate::{
    core::{
        App as MslicerApp,
        history::ModelAction,
        project::model::ModelId,
        state::{GizmoTransform, Tool},
    },
    render::scene::{SceneNode, ViewportCamera, camera_projection, camera_transform, sync_scene},
};

/// Rotation snap increment, in radians. The same 5.625° the previous `transform-gizmo`
/// integration used, so the toolbar's Snap toggle keeps feeling the same.
const SNAP_ANGLE: f32 = std::f32::consts::PI / 32.0;
/// Translation snap increment, in millimetres. Matches the previous integration.
const SNAP_DISTANCE: f32 = 0.1;
/// Scale snap increment. Matches the previous integration.
const SNAP_SCALE: f32 = 0.1;

/// The window-space camera the gizmo hit-tests through. See the module docs.
#[derive(Component)]
pub struct InteractionCamera;

/// Marks `bevy_gizmos_render`'s always-on-top camera for the gizmo meshes, so this module can find
/// it (it is spawned with a private marker of the plugin's) and keep its clear transparent.
#[derive(Component)]
struct GizmoOverlay;

/// The model transform captured when a drag starts, so the drag becomes a single history entry
/// per changed component instead of one per frame.
#[derive(Clone, Copy)]
struct DragSnapshot {
    model: ModelId,
    position: Vector3<f32>,
    rotation: Vector3<f32>,
    scale: Vector3<f32>,
}

/// Adds the gizmo and the systems that bridge it to mslicer's selection and `Model.mesh` fields.
pub fn register(app: &mut App) {
    app.add_plugins(TransformGizmoPlugin)
        .add_systems(
            Update,
            (ensure_interaction_camera, mark_gizmo_overlay),
        )
        .add_systems(
            PostUpdate,
            (
                sync_gizmo_settings,
                sync_gizmo_cameras,
                set_gizmo_focus,
                apply_gizmo_drag,
            )
                .chain()
                // After the gizmo has run (it writes the focus entity's `Transform` and settles
                // its hover/drag state) and before the scene sync pushes mslicer's authoritative
                // transforms back onto the entities — the readback here is the only pull.
                .after(TransformGizmoSystems)
                .before(sync_scene),
        );
}

/// Spawns the window-space interaction camera once.
fn ensure_interaction_camera(
    mut commands: Commands,
    existing: Query<(), With<InteractionCamera>>,
) {
    if !existing.is_empty() {
        return;
    }

    commands.spawn((
        InteractionCamera,
        TransformGizmoCamera,
        Camera3d::default(),
        Camera {
            // Nothing is drawn through this camera; it exists for `viewport_to_world` only.
            is_active: false,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        // Overwritten every frame by [`sync_gizmo_cameras`]; this is only what the camera needs
        // before the first frame does so.
        Projection::Perspective(PerspectiveProjection::default()),
        Transform::default(),
        GlobalTransform::default(),
    ));
}

/// Marks `bevy_gizmos_render`'s gizmo-mesh camera and takes over its clear colour.
fn mark_gizmo_overlay(
    mut commands: Commands,
    cameras: Query<
        (Entity, &mut Camera),
        (
            With<Camera3d>,
            Without<ViewportCamera>,
            Without<PrimaryEguiContext>,
            Without<InteractionCamera>,
            Without<GizmoOverlay>,
        ),
    >,
) {
    for (entity, mut camera) in cameras {
        // The overlay camera renders the handles over the window, on top of the egui pass, so its
        // output is blitted back onto the window — on top of the docked viewport image. Two things
        // make that blit draw *only* the handles:
        //
        // * Its clear is transparent, so everything it does not draw stays transparent and the
        //   alpha-blended blit leaves the UI underneath untouched. (A camera that clears with the
        //   app's `ClearColor` — the default — would blend a translucent clear-coloured veil over
        //   the whole viewport: measured as background `#090909` -> `#0B0B0B`; `ClearColorConfig::None`
        //   with a shared texture measured the same `#0B0B0B`, so the transparent clear of an own
        //   texture is what matters.)
        // * It gets its own main texture, so clearing it cannot wipe the primary camera's UI out of
        //   the texture they would otherwise share. Bevy keys main textures by render target,
        //   format, sample count and `CameraMainTextureUsages`, so the extra usage is what
        //   separates them.
        //
        // A window resize shrinks the target under this camera's viewport for a frame, which is
        // why `sync_gizmo_cameras` clamps that viewport to the window's physical size.
        camera.clear_color = ClearColorConfig::Custom(Color::NONE);

        commands.entity(entity).insert((
            GizmoOverlay,
            CameraMainTextureUsages::default().with(TextureUsages::COPY_DST),
            // Same as the primary and viewport cameras: the default `TonyMcMapFace` needs the
            // `tonemapping_luts` feature, which is not enabled.
            Tonemapping::None,
            DebandDither::Disabled,
        ));
    }
}

/// Mirrors the toolbar's mode/space/snap toggles onto bevy's gizmo settings.
fn sync_gizmo_settings(
    app: Option<NonSend<Box<MslicerApp>>>,
    mut settings: ResMut<TransformGizmoSettings>,
) {
    let Some(app) = app else {
        return;
    };

    settings.mode = match app.state.gizmo_mode {
        GizmoTransform::Translate => TransformGizmoMode::Translate,
        GizmoTransform::Rotate => TransformGizmoMode::Rotate,
        GizmoTransform::Scale => TransformGizmoMode::Scale,
    };
    settings.space = if app.state.gizmo_local {
        TransformGizmoSpace::Local
    } else {
        TransformGizmoSpace::World
    };

    let snap = app.state.gizmo_snap;
    settings.snap_translate = snap.then_some(SNAP_DISTANCE);
    settings.snap_rotate = snap.then_some(SNAP_ANGLE);
    settings.snap_scale = snap.then_some(SNAP_SCALE);
    // Cursor confinement is unsupported on Wayland (and the previous gizmo never grabbed the
    // pointer either), so a drag is not allowed to confine it.
    settings.confine_cursor = false;
}

/// Points the interaction camera at the docked viewport and keeps both gizmo cameras' projection
/// in step with the viewport camera's.
fn sync_gizmo_cameras(
    app: Option<NonSend<Box<MslicerApp>>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut interaction: Query<
        (
            &mut Camera,
            &mut Transform,
            &mut GlobalTransform,
            &mut Projection,
        ),
        (With<InteractionCamera>, Without<GizmoOverlay>),
    >,
    mut overlay: Query<
        (&mut Projection, &mut Camera),
        (
            With<GizmoOverlay>,
            Without<InteractionCamera>,
            Without<ViewportCamera>,
        ),
    >,
) {
    let Some(app) = app else {
        return;
    };

    let projection = app.config.render.projection;
    let transform = camera_transform(&app.camera, projection);
    let rect = app.state.viewport.rect;

    // The egui layout can trail the window by a frame while it is being resized, and
    // `bevy_gizmos_render` copies the interaction camera's viewport onto the always-on-top overlay
    // camera, which renders into the window (`transform_gizmo_render.rs:437`). A rect that no
    // longer fits that target makes `set_scissor_rect` fail validation and quits the app, so clamp
    // to the window's physical size — and set it on the overlay camera too, so the plugin's copy
    // order cannot matter.
    let target = windows
        .iter()
        .next()
        .map(|window| (window.physical_width(), window.physical_height()));
    let (min, size) = match target {
        Some((width, height)) => {
            // Component-wise by hand: `nalgebra`'s `Matrix::min()` shadows `Ord::min` here.
            let x = rect.min.x.min(width.saturating_sub(1));
            let y = rect.min.y.min(height.saturating_sub(1));
            (
                Vector2::new(x, y),
                Vector2::new(rect.size.x.min(width - x), rect.size.y.min(height - y)),
            )
        }
        None => (rect.min, rect.size),
    };
    let viewport = (size.x > 0 && size.y > 0).then(|| Viewport {
        physical_position: UVec2::new(min.x, min.y),
        physical_size: UVec2::new(size.x, size.y),
        ..default()
    });

    // The clamp only bites on the frames around a window resize, when the egui layout still
    // describes the previous size.
    if (min, size) != (rect.min, rect.size) {
        tracing::trace!(
            "clamped gizmo viewport {:?}+{:?} -> {:?}+{:?} ({target:?})",
            rect.min,
            rect.size,
            min,
            size
        );
    }

    // The aspect a projection needs is overwritten by bevy's `camera_system` from each camera's
    // own viewport, but the rect is the closest thing here to the viewport camera's image size.
    let aspect_size = Vec2::new(size.x.max(1) as f32, size.y.max(1) as f32);

    if let Some((mut camera, mut camera_transform, mut global, mut projection_slot)) =
        interaction.iter_mut().next()
    {
        if let Some(viewport) = viewport.clone() {
            camera.viewport = Some(viewport);
        }

        // The gizmo's rays must be the viewport camera's rays, or the handles would be picked at a
        // different depth and scale than they are drawn.
        *camera_transform = transform;
        *global = GlobalTransform::from(transform);
        *projection_slot = camera_projection(&app.camera, projection, aspect_size);
    }

    // `bevy_gizmos_render` copies the interaction camera's transform and viewport onto the overlay
    // camera but not its projection, which the handles are drawn with.
    for (mut overlay_projection, mut overlay_camera) in &mut overlay {
        *overlay_projection = camera_projection(&app.camera, projection, aspect_size);
        if let Some(viewport) = viewport.clone() {
            overlay_camera.viewport = Some(viewport);
        }
    }
}

/// Reads the transform the gizmo wrote on the focused entity back into `Model.mesh`, and commits
/// the drag's history entries once it ends.
fn apply_gizmo_drag(
    app: Option<NonSendMut<Box<MslicerApp>>>,
    gizmo: Res<TransformGizmoState>,
    nodes: Query<&SceneNode>,
    transforms: Query<&Transform>,
    mut snapshot: Local<Option<DragSnapshot>>,
) {
    let Some(mut app) = app else {
        return;
    };
    let app = &mut **app;

    // While the gizmo is dragging it is always on a model node: `set_gizmo_focus` only ever puts
    // the focus there.
    let dragged = gizmo
        .entity
        .filter(|_| gizmo.active)
        .and_then(|entity| Some((entity, *nodes.get(entity).ok()?)))
        .and_then(|(entity, node)| match node {
            SceneNode::Model(id) => Some((entity, id)),
            _ => None,
        });

    if let Some((entity, id)) = dragged {
        let Ok(transform) = transforms.get(entity) else {
            return;
        };
        let platform = app.project.slice_config.platform_size;

        // Capture the pre-drag transform once, so the whole drag is one history entry per changed
        // component rather than one per frame.
        if snapshot.is_none() {
            let Some(model) = app.project.model(id) else {
                return;
            };
            *snapshot = Some(DragSnapshot {
                model: id,
                position: model.mesh.position(),
                rotation: model.mesh.rotation(),
                scale: model.mesh.scale(),
            });
        }

        // The gizmo writes a full transform (its drag start plus a delta), so this is the new
        // transform of the model, not a delta to apply. Writing back through the setters keeps
        // `update_oob`, the support transform propagation and the out-of-bounds warning alive.
        let position = Vector3::new(
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        );
        let rotation = Quaternion::new(
            transform.rotation.w,
            transform.rotation.x,
            transform.rotation.y,
            transform.rotation.z,
        );
        let (roll, pitch, yaw) =
            Rotation3::from(UnitQuaternion::from_quaternion(rotation)).euler_angles();
        let scale = Vector3::new(transform.scale.x, transform.scale.y, transform.scale.z);

        if let Some(model) = app.project.model(id) {
            model.set_position(&platform, position);
            model.set_rotation(&platform, Vector3::new(roll, pitch, yaw));
            model.set_scale(&platform, scale);
        }

        app.state.gizmo_hot = true;
        return;
    }

    // Not dragging: the gizmo still owns the pointer while a handle is hovered, which is what
    // keeps the camera from orbiting under the pointer.
    app.state.gizmo_hot = gizmo.active || gizmo.hovered_axis.is_some();

    // The drag just ended: commit an undo entry per transform that actually changed.
    let Some(drag) = snapshot.take() else {
        return;
    };
    let Some(model) = app.project.model(drag.model) else {
        return;
    };

    let (position, rotation, scale) = (
        model.mesh.position(),
        model.mesh.rotation(),
        model.mesh.scale(),
    );

    if position != drag.position {
        app.history
            .track_model(drag.model, ModelAction::Position(drag.position));
    }
    if rotation != drag.rotation {
        app.history
            .track_model(drag.model, ModelAction::Rotation(drag.rotation));
    }
    if scale != drag.scale {
        app.history
            .track_model(drag.model, ModelAction::Scale(drag.scale));
    }
}

/// Puts bevy's [`TransformGizmoFocus`] on the scene entity of the single selected model, and
/// nowhere else: no focus means the gizmo neither draws nor picks anything.
fn set_gizmo_focus(
    mut commands: Commands,
    app: Option<NonSendMut<Box<MslicerApp>>>,
    mut gizmo: ResMut<TransformGizmoState>,
    window: Single<&Window, With<PrimaryWindow>>,
    nodes: Query<(Entity, &SceneNode, Has<TransformGizmoFocus>)>,
) {
    let Some(mut app) = app else {
        return;
    };
    let app = &mut **app;

    // Consumed, not just read: the Viewport tab sets it while it draws, and this system may run on
    // either side of the egui pass, so "was painted since we last looked" is the only stable
    // question to ask. A tab that is not in the layout has no rect to draw the handles over.
    let painted = std::mem::take(&mut app.state.viewport.painted);

    // The gizmo's hover test projects the handles onto the window and does not clip to the
    // viewport, so a handle whose axis projects past the viewport's edge would otherwise also be
    // grabbable from the docked panel next to it. Clearing the hover keeps the drag start inside
    // the viewport, exactly where the old egui gizmo's response was clipped to.
    let viewport = app.state.viewport.rect.logical;
    let over_viewport = window.cursor_position().is_some_and(|cursor| {
        cursor.x >= viewport.min.x
            && cursor.x <= viewport.max.x
            && cursor.y >= viewport.min.y
            && cursor.y <= viewport.max.y
    });
    if !over_viewport && !gizmo.active {
        gizmo.hovered_axis = None;
    }

    // Unlike the hover, visibility follows the selection alone: a model selected from the outliner
    // shows its handles whether or not the pointer is over the viewport, as it did before.
    let focused = (painted && app.state.tool == Tool::Select)
        .then(|| app.state.selected.single_model())
        .flatten();

    for (entity, node, has_focus) in &nodes {
        let model = match *node {
            SceneNode::Model(id) => Some(id),
            _ => None,
        };

        match (model.is_some_and(|id| Some(id) == focused), has_focus) {
            (true, false) => {
                commands.entity(entity).insert(TransformGizmoFocus);
            }
            (false, true) => {
                commands.entity(entity).remove::<TransformGizmoFocus>();
            }
            _ => {}
        }
    }
}
