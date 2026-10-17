//! The material the scene's model and supports entities render with.
//!
//! This is bevy's counterpart of the pre-bevy viewport model pass: [`model.wgsl`](model.wgsl) is
//! the old shader (minus the picking/normal/world targets, which bevy's `MeshRayCast` picking
//! replaced), and [`ModelMaterial`] carries the per-node data that used to live in the pass'
//! uniform buffer and the selection bitmask in its storage buffer.
//!
//! Per-node values that vary at runtime (colour, render style, overhang angle, platform size,
//! the selected-face bitmask) travel as material data, so nothing about them needs a `Transform`
//! round-trip — the render transform stays a direct `GlobalTransform` from
//! `Mesh::transformation_matrix()` (see the module docs of [`super`]).

use std::range::Range;

use bevy::{
    asset::{AssetPath, embedded_asset, embedded_path},
    pbr::{Material, MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    reflect::TypePath,
    render::{
        mesh::MeshVertexBufferLayoutRef,
        render_resource::{
            AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
        },
        storage::ShaderBuffer,
    },
    shader::ShaderRef,
};

/// Registers the shader, the material type and its pipeline with the app.
pub fn register(app: &mut App) {
    embedded_asset!(app, "model.wgsl");
    app.add_plugins(MaterialPlugin::<ModelMaterial>::default());
}

/// The per-node values the fragment shader reads.
///
/// Field order and types are the shader's `ModelParams`.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct ModelParams {
    /// The node's colour in display (sRGB-encoded) space — `Model::color.to_srgb()`, or the
    /// inverted supports colour, exactly what the old pass' uniform carried.
    pub model_color: Vec3,
    /// The print platform size in millimetres, for the out-of-bounds tint.
    pub build_volume: Vec3,
    /// `RenderStyle`'s discriminant (`Normals` 0, `RandomTriangle` 1, `Rendered` 2).
    pub render_style: u32,
    /// The overhang threshold in radians, or `f32::from_bits(u32::MAX)` when overhang shading
    /// is disabled — the same "not a number" sentinel the old uniform used.
    pub overhang_angle: f32,
}

// Hand-written so the disabled-overhang sentinel (a NaN) compares equal to itself; a derived
// `PartialEq` would report every frame as changed and re-prepare the material bind group.
impl PartialEq for ModelParams {
    fn eq(&self, other: &Self) -> bool {
        self.model_color == other.model_color
            && self.build_volume == other.build_volume
            && self.render_style == other.render_style
            && self.overhang_angle.to_bits() == other.overhang_angle.to_bits()
    }
}

/// The render style of a node's faces, plus the per-face selection highlight.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct ModelMaterial {
    #[uniform(0)]
    pub params: ModelParams,
    /// One bit per face of the mesh this material draws: the Orient tool's hovered face for a
    /// model, the selected supports' face ranges for a supports mesh.
    #[storage(1, read_only, visibility(fragment))]
    pub selected: Handle<ShaderBuffer>,
}

impl Material for ModelMaterial {
    fn fragment_shader() -> ShaderRef {
        // The same path `embedded_asset!` in `register` registers `model.wgsl` under — both
        // macros are invoked from this file, so they agree by construction.
        ShaderRef::Path(embedded_asset_path("model.wgsl"))
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The old pass used `PrimitiveState::default()`, i.e. no culling: back faces are shaded
        // (grey, in the `Rendered` style) rather than dropped.
        descriptor.primitive.cull_mode = None;

        Ok(())
    }
}

/// The asset path `embedded_asset!` gives a file next to this module.
fn embedded_asset_path(file: &str) -> AssetPath<'static> {
    AssetPath::from_path_buf(embedded_path!(file)).with_source("embedded")
}

/// The selected faces of one mesh, one bit per face.
///
/// This is the bitmask encoding the old pass' storage buffer used, and it is what
/// [`ModelMaterial::selected`] is uploaded as.
#[derive(Default, Debug, Clone)]
pub struct FaceSelection(Vec<u32>);

impl FaceSelection {
    /// A cleared selection for a mesh with `faces` faces.
    pub fn new(faces: usize) -> Self {
        Self(vec![0; faces.div_ceil(32)])
    }

    /// Marks `face`, ignoring indices past the end of the mesh.
    pub fn insert(&mut self, face: u32) {
        if let Some(word) = self.0.get_mut((face / 32) as usize) {
            *word |= 1 << (face % 32);
        }
    }

    /// Marks every face in `range`, ignoring indices past the end of the mesh.
    pub fn insert_range(&mut self, range: Range<u32>) {
        range.into_iter().for_each(|face| self.insert(face));
    }

    /// The bitmask words to upload. An empty selection still needs one word, because a
    /// zero-sized buffer cannot be bound.
    pub fn into_words(mut self) -> Vec<u32> {
        if self.0.is_empty() {
            self.0.push(0);
        }
        self.0
    }
}

/// Keeps [`ModelMaterial::selected`] in sync with a [`FaceSelection`].
pub fn write_selection(buffer: &mut ShaderBuffer, words: &[u32]) {
    if buffer.cast_slice::<u32>() == Some(words) {
        return;
    }

    buffer.clear();
    buffer.extend_from_slice(words);
}

/// `bevy_pbr::material::MATERIAL_BIND_GROUP_INDEX`, which `model.wgsl` hardcodes as `@group(3)`.
const _: () = assert!(bevy::pbr::MATERIAL_BIND_GROUP_INDEX == 3);
