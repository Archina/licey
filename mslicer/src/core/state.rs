use std::{sync::Arc, time::Instant};

use bevy::prelude::{Handle, Image};
use common::color::SRgb;
use egui::{Rect, TextureId, Vec2};
use egui_phosphor::regular::{CURSOR_CLICK, LINE_SEGMENT, TRIANGLE};
use egui_tracing::EventCollector;
use nalgebra::{Matrix4, Vector2, Vector3};
use slicer::mesh::Mesh;
use wgpu::Device;

use crate::{
    core::{
        config::peripherals::Webhook,
        project::model::{Model, ModelId},
        selected::{SelectedModel, SelectedPrinter, SelectedSupports},
    },
    interface::tools::Tools,
    render::util::MeshBuffers,
};

#[derive(Default)]
pub struct UiState {
    pub event_collector: EventCollector,
    pub line_support_debug: Vec<[Vector3<f32>; 2]>,
    pub queue_reset_ui: bool,
    pub tool: Tool,
    pub viewport: ViewportRender,

    // support stuff
    pub workspace: WorkspaceHover,
    pub hovered_geometry: Option<GeometryHit>,
    pub support_mode: bool,

    pub selected: SelectedModel,
    pub selected_printer: SelectedPrinter,
    pub selected_supports: SelectedSupports,
    pub support_preview: Option<Mesh>,

    pub selected_remap_point: Option<u8>,

    // remote send ui
    pub working_address: String,
    pub working_filename: String,
    pub remote_print_connecting: RemotePrintConnectStatus,
    pub shared_webhook: Arc<SharedPrintCompletion>,

    // slice preview
    pub preview_layer: usize,
    pub last_preview_layer: usize,
    pub preview_offset: Vector2<f32>,
    pub preview_scale: f32,
    pub layer_count: (usize, u8),

    pub preview_mode: PreviewMode,
    pub last_preview_interact: Option<Instant>,

    pub anisotropic_aa: bool,

    pub tools: Tools,
    pub move_timeout: u32,

    // transform gizmo
    pub gizmo_mode: GizmoTransform,
    pub gizmo_local: bool,
    pub gizmo_snap: bool,
    /// Whether the gizmo owned the pointer input on the previous frame. Used to
    /// stop the camera from orbiting while a handle is hovered or being dragged.
    pub gizmo_hot: bool,
}

/// The bevy render target the workspace scene draws into, and the egui handle to it.
///
/// The viewport panel writes [`Self::target_size`] (in physical pixels) every frame; the scene's
/// sync system keeps [`Self::image`] at that size and hands [`Self::texture_id`] back for
/// `painter.image()`.
///
/// `rect` is where that image lands on the window, which is what the transform gizmo needs: its
/// interaction camera uses the physical rect as its `Camera::viewport`, so that
/// `Camera::viewport_to_world` resolves window cursor coordinates into the viewport's world rays,
/// and the logical rect to tell whether the pointer is over the viewport at all.
pub struct ViewportRender {
    pub image: Option<Handle<Image>>,
    pub texture_id: Option<TextureId>,
    /// Physical size of the image currently allocated.
    pub size: Vector2<u32>,
    /// Physical size the viewport panel wants.
    pub target_size: Vector2<u32>,
    /// Where the viewport sits on the window, in physical pixels and in points.
    pub rect: ViewportRect,
    /// Whether the Viewport tab was drawn this frame. The gizmo stays hidden while it is not: a
    /// hidden tab has no rect to hit-test against and nothing to draw the handles over.
    pub painted: bool,
}

impl Default for ViewportRender {
    fn default() -> Self {
        Self {
            image: None,
            texture_id: None,
            size: Vector2::zeros(),
            target_size: Vector2::zeros(),
            rect: ViewportRect::default(),
            painted: false,
        }
    }
}

/// The viewport panel's rect on the window, as last painted.
#[derive(Clone, Copy)]
pub struct ViewportRect {
    /// Physical (pixel) position on the window.
    pub min: Vector2<u32>,
    /// Physical size.
    pub size: Vector2<u32>,
    /// The same rect in egui points, i.e. in window logical coordinates.
    pub logical: Rect,
}

impl Default for ViewportRect {
    fn default() -> Self {
        Self {
            min: Vector2::zeros(),
            size: Vector2::zeros(),
            logical: Rect::ZERO,
        }
    }
}

/// Which handles the transform gizmo shows.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GizmoTransform {
    #[default]
    Translate,
    Rotate,
    Scale,
}

impl GizmoTransform {
    pub const ALL: [Self; 3] = [Self::Translate, Self::Rotate, Self::Scale];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Translate => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }
}

pub struct PreviewModel {
    pub mesh: MeshBuffers,
    pub faces: usize,

    pub transform: Matrix4<f32>,
    pub color: SRgb<f32>,
}

#[derive(Default, Copy, Clone, PartialEq, Eq)]
pub enum PreviewMode {
    Interactive,
    #[default]
    Static,
}

#[derive(Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Support,
    Orient,
}

#[derive(Default)]
pub struct SharedPrintCompletion {
    pub webhook: Webhook,
    pub alert: bool,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct GeometryHit {
    pub model: ModelId,
    pub support: bool,

    pub face: u32,
}

#[derive(Default)]
pub struct WorkspaceHover {
    pub is_moving: bool,
    pub aspect: f32,
    pub uv: Vector2<f32>,
}

#[derive(Default, PartialEq, Eq)]
pub enum RemotePrintConnectStatus {
    #[default]
    None,
    Connecting,
    Scanning,
}

impl PreviewModel {
    pub fn for_model(model: &mut Model, device: &Device) -> Self {
        Self {
            mesh: model.get_buffers(device).clone(),
            faces: model.mesh.face_count(),

            transform: *model.mesh.transformation_matrix(),
            color: model.color.to_srgb(),
        }
    }
}

impl WorkspaceHover {
    pub fn new(is_moving: bool, aspect: f32, uv: Vec2) -> Self {
        Self {
            is_moving,
            aspect,
            uv: Vector2::new(uv.x, uv.y),
        }
    }

    pub fn hovered(&self) -> bool {
        self.uv.x >= 0.0 && self.uv.y >= 0.0
    }
}

impl PreviewMode {
    pub fn other(&self) -> Self {
        match self {
            PreviewMode::Interactive => PreviewMode::Static,
            PreviewMode::Static => PreviewMode::Interactive,
        }
    }
}

impl Tool {
    pub const ALL: [Self; 3] = [Self::Select, Self::Support, Self::Orient];

    pub fn icon(&self) -> &str {
        match self {
            Tool::Select => CURSOR_CLICK,
            Tool::Support => LINE_SEGMENT,
            Tool::Orient => TRIANGLE,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Tool::Select => "Select (Default)",
            Tool::Support => "Support Placement",
            Tool::Orient => "Orient to Face",
        }
    }
}
