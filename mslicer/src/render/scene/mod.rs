//! The bevy-rendered workspace viewport.
//!
//! Phase 1 replaces the hand-rolled wgpu workspace pipeline with a real bevy scene: a second
//! `Camera3d` renders into an `Image` that the docked Viewport tab draws with `painter.image()`,
//! models/supports are `Mesh3d` entities, and the build plate, grid, normals and debug lines are
//! `LineList` meshes.
//!
//! Two invariants shape this module:
//!
//! * The mslicer [`Camera`] stays the source of truth for the viewpoint. The bevy camera mirrors
//!   it exactly (`Transform::looking_at` plus a `PerspectiveProjection`/`OrthographicProjection`
//!   built from the same numbers), in mslicer's Z-up world space.
//! * `Model.mesh`'s `position`/`rotation`/`scale` fields stay authoritative and are applied by
//!   setting [`GlobalTransform`] **directly** from `Mesh::transformation_matrix()`. mslicer
//!   composes `T * S * R` while bevy's [`Transform`] is `T * R * S`, so the render transform must
//!   never go through `Transform`. [`sync_scene`] therefore runs in `PostUpdate` *after* bevy's
//!   transform propagation, which also keeps a freshly spawned entity's `GlobalTransform` from
//!   being overwritten by the identity `Transform` that `Mesh3d` drags along.

use std::{
    collections::{HashMap, HashSet},
    f32::consts::PI,
};

use bevy::{
    asset::RenderAssetUsages,
    camera::{ImageRenderTarget, RenderTarget, ScalingMode, visibility::RenderLayers},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    mesh::Indices,
    prelude::*,
    render::{
        render_resource::{
            Extent3d, PrimitiveTopology, TextureDimension, TextureFormat, TextureUsages,
        },
        storage::ShaderBuffer,
    },
};
use bevy_egui::{EguiTextureHandle, EguiUserTextures};
use common::{
    color::{LinearRgb, OkLab, SRgb},
    units::{Milimeter, Milimeters},
};
use egui::Theme;
use nalgebra::{Matrix4, Vector2, Vector3};
use slicer::mesh::{Mesh as SlicerMesh, MeshId};

use crate::{
    core::{
        App as MslicerApp,
        config::render::Projection as SliceProjection,
        project::model::{Model, ModelId},
        state::{GeometryHit, Tool},
    },
    render::camera::{Camera as MslicerCamera, FAR, NEAR},
};

mod gizmo;
mod material;

use self::material::{FaceSelection, ModelMaterial, ModelParams, write_selection};

/// Render layer the whole workspace scene lives on.
///
/// The primary (window) camera only exists to host the egui context; giving the scene its own
/// layer keeps bevy from drawing the models onto the window behind the UI.
pub const VIEWPORT_LAYER: usize = 1;

/// The colour the viewport camera clears to, matching what `viewport.rs` fills behind the image.
fn theme_background(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::srgb_u8(9, 9, 9),
        Theme::Light => Color::WHITE,
    }
}

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        material::register(app);
        gizmo::register(app);

        app.init_resource::<MeshCache>()
            .init_resource::<SharedMeshes>()
            .add_systems(Update, (ensure_scene, pick_hover.after(ensure_scene)))
            .add_systems(PostUpdate, sync_scene.after(TransformSystems::Propagate));
    }
}

/// One mirror of a piece of mslicer state in the bevy scene.
#[derive(Component, Debug, Copy, Clone, PartialEq, Eq, Hash)]
enum SceneNode {
    Model(ModelId),
    Supports(ModelId),
    SupportPreview,
    Marker,
    BuildPlate,
    Normals,
    SupportDebug,
}

/// Caches the bevy mesh built from a `slicer::mesh::Mesh`.
///
/// Keyed by [`MeshId`], which is the address of the mesh's `MeshInner` — so it changes whenever
/// `Model::replace_mesh` / `Supports::invalidate_cache` swap the geometry, and never otherwise.
#[derive(Resource, Default)]
struct MeshCache(HashMap<MeshId, Handle<Mesh>>);

impl MeshCache {
    fn get(&mut self, meshes: &mut Assets<Mesh>, mesh: &SlicerMesh) -> Handle<Mesh> {
        self.0
            .entry(mesh.mesh_id())
            .or_insert_with(|| meshes.add(mesh_from_slicer(mesh)))
            .clone()
    }

    /// Drops meshes no live node refers to any more.
    fn retain(&mut self, meshes: &mut Assets<Mesh>, live: &HashSet<MeshId>) {
        self.0.retain(|id, handle| {
            if live.contains(id) {
                return true;
            }
            meshes.remove(handle.id());
            false
        });
    }
}

/// Meshes that are generated once and shared by every node that needs them.
#[derive(Resource, Default)]
struct SharedMeshes {
    sphere: Option<Handle<Mesh>>,
}

/// The [`MeshId`] a node's `Mesh3d` was last built from.
#[derive(Component, Copy, Clone, PartialEq, Eq)]
struct SceneMesh(MeshId);

#[derive(Component)]
struct ViewportCamera;

/// Makes sure the render image, the viewport camera and one entity per scene node exist.
///
/// Entities are spawned bare here and filled in by [`sync_scene`], which runs in a later schedule
/// on the same frame.
fn ensure_scene(
    mut commands: Commands,
    app: Option<NonSendMut<Box<MslicerApp>>>,
    mut images: ResMut<Assets<Image>>,
    mut egui_textures: ResMut<EguiUserTextures>,
    nodes: Query<(Entity, &SceneNode)>,
    camera: Query<Entity, With<ViewportCamera>>,
) {
    let Some(mut app) = app else {
        return;
    };
    let app = &mut **app;

    // --- the image the viewport camera renders into ---------------------------
    let target = app.state.viewport.target_size;
    let size = Vector2::new(target.x.max(1), target.y.max(1));
    if app.state.viewport.image.is_none() || app.state.viewport.size != size {
        if let Some(old) = app.state.viewport.image.take() {
            egui_textures.remove_image(old.id());
            images.remove(old.id());
        }

        let background = theme_background(app.config.ui.theme);
        let srgb = background.to_srgba();
        let pixel = [
            (srgb.red * 255.0).round() as u8,
            (srgb.green * 255.0).round() as u8,
            (srgb.blue * 255.0).round() as u8,
            255,
        ];

        let mut image = Image::new_fill(
            Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &pixel,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_DST
            | TextureUsages::RENDER_ATTACHMENT;

        let handle = images.add(image);
        let id = egui_textures.add_image(EguiTextureHandle::Strong(handle.clone()));
        app.state.viewport.image = Some(handle);
        app.state.viewport.texture_id = Some(id);
        app.state.viewport.size = size;
    }
    let Some(image) = app.state.viewport.image.clone() else {
        return;
    };

    // --- the camera -----------------------------------------------------------
    if camera.is_empty() {
        let projection = camera_projection(
            &app.camera,
            app.config.render.projection,
            Vec2::new(size.x as f32, size.y as f32),
        );
        let transform = camera_transform(&app.camera, app.config.render.projection);

        commands
            .spawn((
                Camera3d::default(),
                Camera {
                    clear_color: ClearColorConfig::Custom(theme_background(
                        app.config.ui.theme,
                    )),
                    ..default()
                },
                RenderTarget::Image(ImageRenderTarget {
                    handle: image,
                    scale_factor: 1.0,
                }),
                projection,
                transform,
                Tonemapping::None,
                DebandDither::Disabled,
                RenderLayers::layer(VIEWPORT_LAYER),
            ))
            .insert(ViewportCamera);
    }

    // --- one entity per node ---------------------------------------------------
    let mut wanted = Vec::new();
    for model in app.project.models.iter_mut() {
        if model.hidden {
            continue;
        }
        wanted.push(SceneNode::Model(model.id));
        if model.supports.mesh().is_some() {
            wanted.push(SceneNode::Supports(model.id));
        }
    }
    wanted.push(SceneNode::BuildPlate);
    if app.config.render.normals {
        wanted.push(SceneNode::Normals);
    }
    if !app.state.line_support_debug.is_empty() {
        wanted.push(SceneNode::SupportDebug);
    }
    if app.state.workspace.is_moving {
        wanted.push(SceneNode::Marker);
    }
    if app.state.support_preview.is_some() {
        wanted.push(SceneNode::SupportPreview);
    }

    let wanted: HashSet<SceneNode> = wanted.into_iter().collect();
    let mut present = HashSet::new();
    for (entity, node) in &nodes {
        if wanted.contains(node) {
            present.insert(*node);
        } else {
            commands.entity(entity).despawn();
        }
    }

    for node in wanted.difference(&present) {
        commands.spawn((
            *node,
            Transform::default(),
            GlobalTransform::default(),
            Visibility::default(),
            RenderLayers::layer(VIEWPORT_LAYER),
        ));
    }
}

/// What [`sync_scene`] wants done with a node's mesh this frame.
enum Geometry {
    /// The `Mesh3d` is already the right one.
    Keep,
    /// A cached mesh built from a `slicer::mesh::Mesh`.
    Slicer(Handle<Mesh>, MeshId),
    /// A `LineList` mesh that is re-filled in place every frame.
    Lines(Handle<Mesh>, Vec<[f32; 3]>),
}

/// How [`sync_scene`] wants a node drawn this frame.
enum Appearance {
    /// A flat colour, for the lines, the build plate and the camera-target marker.
    Standard(Color),
    /// The model shader ([`ModelMaterial`]), for models and their supports.
    Model {
        params: ModelParams,
        selected: Vec<u32>,
    },
}

/// Pushes the authoritative mslicer state onto the scene entities.
fn sync_scene(
    mut commands: Commands,
    app: Option<NonSendMut<Box<MslicerApp>>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut model_materials: ResMut<Assets<ModelMaterial>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut cache: ResMut<MeshCache>,
    mut shared: ResMut<SharedMeshes>,
    mut nodes: Query<
        (
            Entity,
            &SceneNode,
            Option<&Mesh3d>,
            Option<&SceneMesh>,
            &mut GlobalTransform,
            Option<&MeshMaterial3d<StandardMaterial>>,
            Option<&MeshMaterial3d<ModelMaterial>>,
        ),
        Without<ViewportCamera>,
    >,
    mut camera: Query<
        (&mut Transform, &mut GlobalTransform, &mut Projection, &mut Camera),
        With<ViewportCamera>,
    >,
) {
    let Some(mut app) = app else {
        return;
    };
    let app = &mut **app;

    if let Some((mut transform, mut global, mut projection, mut bevy_camera)) =
        camera.iter_mut().next()
    {
        let size = app.state.viewport.size;
        let size = Vec2::new(size.x.max(1) as f32, size.y.max(1) as f32);

        let new = camera_transform(&app.camera, app.config.render.projection);
        if *transform != new {
            *transform = new;
        }
        // Propagation already ran this frame, so mirror it by hand and skip the one frame of lag.
        *global = GlobalTransform::from(new);

        *projection = camera_projection(&app.camera, app.config.render.projection, size);

        // The viewport has always used the theme's background (`viewport.rs` fills the same
        // colour under the image) rather than a fixed one.
        let background = theme_background(app.config.ui.theme);
        if !matches!(bevy_camera.clear_color, ClearColorConfig::Custom(c) if c == background) {
            bevy_camera.clear_color = ClearColorConfig::Custom(background);
        }
    }

    // The model material's pass-wide values, mirroring the old `BasePass::prepare`.
    let render_style = app.config.render.style as u32;
    let build_volume = app
        .project
        .slice_config
        .platform_size
        .map(|x| x.get::<Milimeter>());
    let (show_overhang, overhang_angle) = app.config.render.overhangs;
    let overhang_angle = show_overhang
        .then_some(overhang_angle.to_radians())
        .unwrap_or(f32::from_bits(u32::MAX));

    let mut live = HashSet::new();
    for (entity, node, mesh, scene_mesh, mut transform, material, model_material) in &mut nodes {
        let current = scene_mesh.map(|x| x.0);

        let (geometry, appearance, target) = match *node {
            SceneNode::Model(id) => {
                // Read the hover state before borrowing the model out of the project.
                let hovered_face = match app.state.hovered_geometry {
                    Some(hover)
                        if app.state.tool == Tool::Orient
                            && !hover.support
                            && hover.model == id =>
                    {
                        Some(hover.face)
                    }
                    _ => None,
                };

                let Some(model) = app.project.model(id) else {
                    continue;
                };
                let mesh_id = model.mesh.mesh_id();
                live.insert(mesh_id);

                let geometry = if current == Some(mesh_id) {
                    Geometry::Keep
                } else {
                    Geometry::Slicer(cache.get(&mut meshes, &model.mesh), mesh_id)
                };

                // The Orient tool highlights the face under the cursor.
                let mut selected = FaceSelection::new(model.mesh.face_count());
                if let Some(face) = hovered_face {
                    selected.insert(face);
                }

                let appearance = Appearance::Model {
                    params: ModelParams {
                        model_color: bevy_vec3(model.color.to_srgb().into()),
                        build_volume: bevy_vec3(build_volume),
                        render_style,
                        overhang_angle,
                    },
                    selected: selected.into_words(),
                };
                (
                    geometry,
                    appearance,
                    global_transform(model.mesh.transformation_matrix()),
                )
            }
            SceneNode::Supports(id) => {
                // Selected supports are highlighted face range by face range. Read the
                // selection before borrowing the supports mesh out of the project.
                let selected_supports = (app.state.selected_supports.iter())
                    .filter(|selected| selected.model == id)
                    .map(|selected| selected.support)
                    .collect::<Vec<_>>();

                let Some(model) = app.project.model(id) else {
                    continue;
                };
                let color = invert_color(model.color);
                let Some((mesh, face_map)) = model.supports.mesh() else {
                    continue;
                };
                let mesh_id = mesh.mesh_id();
                live.insert(mesh_id);

                let geometry = if current == Some(mesh_id) {
                    Geometry::Keep
                } else {
                    Geometry::Slicer(cache.get(&mut meshes, mesh), mesh_id)
                };

                // Selected supports are highlighted face range by face range.
                let mut selected = FaceSelection::new(mesh.face_count());
                for support in selected_supports {
                    if let Some(range) = face_map.get(&support) {
                        selected.insert_range(range.clone());
                    }
                }

                let appearance = Appearance::Model {
                    params: ModelParams {
                        model_color: bevy_vec3(color.into()),
                        build_volume: bevy_vec3(build_volume),
                        render_style,
                        overhang_angle,
                    },
                    selected: selected.into_words(),
                };
                (
                    geometry,
                    appearance,
                    global_transform(mesh.transformation_matrix()),
                )
            }
            SceneNode::SupportPreview => {
                let Some(preview) = &app.state.support_preview else {
                    continue;
                };
                let mesh_id = preview.mesh_id();
                live.insert(mesh_id);

                let geometry = if current == Some(mesh_id) {
                    Geometry::Keep
                } else {
                    Geometry::Slicer(cache.get(&mut meshes, preview), mesh_id)
                };
                (
                    geometry,
                    Appearance::Standard(Color::WHITE),
                    GlobalTransform::IDENTITY,
                )
            }
            SceneNode::BuildPlate => {
                let lines = build_plate_lines(
                    app.project.slice_config.platform_size,
                    app.config.render.grid_size,
                );
                let handle = ensure_lines(&mut commands, entity, mesh, &mut meshes);
                (
                    Geometry::Lines(handle, lines),
                    Appearance::Standard(line_color(app.config.ui.theme)),
                    GlobalTransform::IDENTITY,
                )
            }
            SceneNode::Normals => {
                let lines = normal_lines(&app.project.models);
                let handle = ensure_lines(&mut commands, entity, mesh, &mut meshes);
                (
                    Geometry::Lines(handle, lines),
                    Appearance::Standard(Color::srgb(0.5, 0.5, 1.0)),
                    GlobalTransform::IDENTITY,
                )
            }
            SceneNode::SupportDebug => {
                let lines = (app.state.line_support_debug.iter())
                    .flat_map(|[vertex, normal]| {
                        let end = vertex + normal;
                        [[vertex.x, vertex.y, vertex.z], [end.x, end.y, end.z]]
                    })
                    .collect::<Vec<_>>();
                let handle = ensure_lines(&mut commands, entity, mesh, &mut meshes);
                (
                    Geometry::Lines(handle, lines),
                    Appearance::Standard(Color::srgb(1.0, 0.0, 0.0)),
                    GlobalTransform::IDENTITY,
                )
            }
            SceneNode::Marker => {
                let handle = shared
                    .sphere
                    .get_or_insert_with(|| meshes.add(Sphere::new(1.0).mesh().uv(16, 24)))
                    .clone();
                if mesh.map(|x| &x.0) != Some(&handle) {
                    commands.entity(entity).insert(Mesh3d(handle));
                }

                let target = app.camera.target;
                (
                    Geometry::Keep,
                    Appearance::Standard(Color::srgba(1.0, 0.0, 0.0, 0.25)),
                    GlobalTransform::from_translation(Vec3::new(target.x, target.y, target.z)),
                )
            }
        };

        match geometry {
            Geometry::Keep => {}
            Geometry::Slicer(handle, id) => {
                commands
                    .entity(entity)
                    .insert((Mesh3d(handle), SceneMesh(id)));
            }
            Geometry::Lines(handle, lines) => rebuild_lines(&mut meshes, &handle, lines),
        }
        *transform = target;

        match appearance {
            Appearance::Standard(color) => {
                let material = match material {
                    Some(material) => material.0.clone(),
                    None => {
                        let handle = materials.add(StandardMaterial {
                            base_color: color,
                            unlit: true,
                            cull_mode: None,
                            alpha_mode: alpha_mode(color),
                            ..default()
                        });
                        commands.entity(entity).insert(MeshMaterial3d(handle.clone()));
                        handle
                    }
                };
                if let Some(mut material) = materials.get_mut(&material) {
                    if material.base_color != color {
                        material.base_color = color;
                        material.alpha_mode = alpha_mode(color);
                    }
                }
            }
            Appearance::Model { params, selected } => {
                let material = match model_material {
                    Some(material) => material.0.clone(),
                    None => {
                        let handle = model_materials.add(ModelMaterial {
                            params,
                            selected: buffers.add(ShaderBuffer::new(
                                selected.clone(),
                                RenderAssetUsages::default(),
                            )),
                        });
                        commands.entity(entity).insert(MeshMaterial3d(handle.clone()));
                        handle
                    }
                };

                let Some(mut material) = model_materials.get_mut(&material) else {
                    continue;
                };
                if material.params != params {
                    material.params = params;
                }
                if let Some(mut buffer) = buffers.get_mut(&material.selected) {
                    write_selection(&mut buffer, &selected);
                }
            }
        }
    }

    cache.retain(&mut meshes, &live);
}

fn alpha_mode(color: Color) -> AlphaMode {
    if color.alpha() < 1.0 {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    }
}

/// Makes sure a `LineList` node has a mesh and returns its handle.
fn ensure_lines(
    commands: &mut Commands,
    entity: Entity,
    mesh: Option<&Mesh3d>,
    meshes: &mut Assets<Mesh>,
) -> Handle<Mesh> {
    match mesh {
        Some(mesh) => mesh.0.clone(),
        None => {
            let handle = meshes.add(Mesh::new(
                PrimitiveTopology::LineList,
                RenderAssetUsages::default(),
            ));
            commands.entity(entity).insert(Mesh3d(handle.clone()));
            handle
        }
    }
}

fn rebuild_lines(meshes: &mut Assets<Mesh>, handle: &Handle<Mesh>, positions: Vec<[f32; 3]>) {
    if let Some(mut mesh) = meshes.get_mut(handle) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    }
}

/// Casts a ray from the pointer through the viewport and writes the hit into
/// `state.hovered_geometry`, replacing the old GPU `model_target` readback.
fn pick_hover(
    mut ray_cast: MeshRayCast,
    app: Option<NonSendMut<Box<MslicerApp>>>,
    nodes: Query<&SceneNode>,
) {
    let Some(mut app) = app else {
        return;
    };
    let app = &mut **app;

    let Some((position, direction)) = app.hovered_ray() else {
        app.state.hovered_geometry = None;
        return;
    };
    let Ok(direction) = Dir3::new(Vec3::new(direction.x, direction.y, direction.z)) else {
        app.state.hovered_geometry = None;
        return;
    };

    let ray = Ray3d::new(Vec3::new(position.x, position.y, position.z), direction);
    // `Any` because the scene's own visibility is authoritative here: a node's entity exists
    // exactly while its model is visible.
    let filter = |entity| nodes.contains(entity);
    let settings = MeshRayCastSettings::default()
        .with_visibility(RayCastVisibility::Any)
        .with_filter(&filter);

    app.state.hovered_geometry = ray_cast
        .cast_ray(ray, &settings)
        .first()
        .and_then(|(entity, hit)| {
            let face = hit.triangle_index? as u32;
            match nodes.get(*entity).ok()? {
                SceneNode::Model(model) => Some(GeometryHit {
                    model: *model,
                    support: false,
                    face,
                }),
                SceneNode::Supports(model) => Some(GeometryHit {
                    model: *model,
                    support: true,
                    face,
                }),
                _ => None,
            }
        });
}

// --- camera mirroring --------------------------------------------------------

/// The distance the eye orbits at, which differs per projection.
fn eye_distance(camera: &MslicerCamera, projection: SliceProjection) -> f32 {
    match projection {
        SliceProjection::Perspective => camera.distance,
        SliceProjection::Orthographic => FAR / 2.0,
    }
}

fn camera_transform(camera: &MslicerCamera, projection: SliceProjection) -> Transform {
    let eye = camera.position(eye_distance(camera, projection)) + camera.target;
    let target = camera.target;

    let eye = Vec3::new(eye.x, eye.y, eye.z);
    let target = Vec3::new(target.x, target.y, target.z);

    let up = camera.up();
    let mut up = Vec3::new(up.x, up.y, up.z);
    // `Transform::looking_at` panics when the view direction is parallel to `up` — the pole
    // mslicer's own `look_at_rh` degenerates at.
    if (target - eye).normalize_or_zero().cross(up).length_squared() < 1e-8 {
        up = Vec3::X;
    }

    Transform::from_translation(eye).looking_at(target, up)
}

fn camera_projection(camera: &MslicerCamera, projection: SliceProjection, size: Vec2) -> Projection {
    let aspect = size.x / size.y;
    match projection {
        SliceProjection::Perspective => Projection::Perspective(PerspectiveProjection {
            fov: camera.fov,
            aspect_ratio: aspect,
            near: NEAR,
            far: FAR,
            ..default()
        }),
        SliceProjection::Orthographic => {
            // mslicer builds `new_orthographic(-width, width, -height, height, -FAR, FAR)` from
            // the same `distance * sin(fov / 2)` formula.
            let height = camera.distance * (camera.fov / 2.0).sin();
            Projection::Orthographic(OrthographicProjection {
                near: -FAR,
                far: FAR,
                scaling_mode: ScalingMode::Fixed {
                    width: aspect * height * 2.0,
                    height: height * 2.0,
                },
                ..OrthographicProjection::default_3d()
            })
        }
    }
}

// --- geometry ----------------------------------------------------------------

fn mesh_from_slicer(mesh: &SlicerMesh) -> Mesh {
    let positions = (mesh.vertices())
        .iter()
        .map(|v| [v.x, v.y, v.z])
        .collect::<Vec<_>>();
    let indices = (mesh.faces())
        .iter()
        .flat_map(|f| f.iter().copied())
        .collect::<Vec<_>>();

    let mut out = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    out.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    out.insert_indices(Indices::U32(indices));
    out
}

/// The build plate box edges and bed grid, matching the old `line/build_plate.rs`.
fn build_plate_lines(bed_size: Vector3<Milimeters>, grid_size: f32) -> Vec<[f32; 3]> {
    let bed = bed_size.map(|x| x.raw());
    let (a, b) = (bed / 2.0, -bed / 2.0);
    let z = bed.z;

    let mut lines: Vec<[f32; 3]> = Vec::new();
    let mut line = |start: Vector3<f32>, end: Vector3<f32>| {
        lines.push([start.x, start.y, start.z]);
        lines.push([end.x, end.y, end.z]);
    };

    // Bottom plane
    line(Vector3::new(a.x, a.y, 0.0), Vector3::new(b.x, a.y, 0.0));
    line(Vector3::new(a.x, b.y, 0.0), Vector3::new(b.x, b.y, 0.0));
    line(Vector3::new(a.x, a.y, 0.0), Vector3::new(a.x, b.y, 0.0));
    line(Vector3::new(b.x, a.y, 0.0), Vector3::new(b.x, b.y, 0.0));
    // Top plane
    line(Vector3::new(a.x, a.y, z), Vector3::new(b.x, a.y, z));
    line(Vector3::new(a.x, b.y, z), Vector3::new(b.x, b.y, z));
    line(Vector3::new(a.x, a.y, z), Vector3::new(a.x, b.y, z));
    line(Vector3::new(b.x, a.y, z), Vector3::new(b.x, b.y, z));
    // Vertical lines
    line(Vector3::new(a.x, a.y, 0.0), Vector3::new(a.x, a.y, z));
    line(Vector3::new(b.x, a.y, 0.0), Vector3::new(b.x, a.y, z));
    line(Vector3::new(a.x, b.y, 0.0), Vector3::new(a.x, b.y, z));
    line(Vector3::new(b.x, b.y, 0.0), Vector3::new(b.x, b.y, z));

    // Grid on the bottom plane
    for x in 0..(bed.x / grid_size).ceil() as i32 {
        let x = x as f32 * grid_size + b.x;
        line(Vector3::new(x, a.y, 0.0), Vector3::new(x, b.y, 0.0));
    }

    for y in 0..(bed.y / grid_size).ceil() as i32 {
        let y = y as f32 * grid_size + b.y;
        line(Vector3::new(a.x, y, 0.0), Vector3::new(b.x, y, 0.0));
    }

    lines
}

/// Face normals of every visible model, matching the old `line/normals.rs`.
fn normal_lines(models: &[Model]) -> Vec<[f32; 3]> {
    let mut lines = Vec::new();

    for model in models.iter().filter(|x| !x.hidden) {
        let (faces, vertices) = (model.mesh.faces(), model.mesh.vertices());

        for (idx, face) in faces.iter().enumerate() {
            let center = (face.iter())
                .map(|&idx| vertices[idx as usize])
                .sum::<Vector3<f32>>()
                / 3.0;
            let center = model.mesh.transform(&center);
            let normal = model.mesh.transform_normal(&model.mesh.normal(idx));
            let end = center + normal * 0.2;

            lines.push([center.x, center.y, center.z]);
            lines.push([end.x, end.y, end.z]);
        }
    }

    lines
}

fn global_transform(matrix: &Matrix4<f32>) -> GlobalTransform {
    GlobalTransform::from(Mat4::from_cols_slice(matrix.as_slice()))
}

/// bevy's `Vec3` from the nalgebra vectors mslicer's colour and unit types convert to.
fn bevy_vec3(v: Vector3<f32>) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

fn line_color(theme: Theme) -> Color {
    match theme {
        Theme::Dark => Color::WHITE,
        Theme::Light => Color::BLACK,
    }
}

fn invert_color(linear: LinearRgb<f32>) -> SRgb<f32> {
    OkLab::from_linear_srgb(linear)
        .hue_shift(PI)
        .to_linear_srgb()
        .to_srgb()
}
