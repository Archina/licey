use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

use bevy::{
    prelude::*,
    render::{
        render_phase::TrackedRenderPass,
        render_resource::{
            BindGroupLayoutDescriptor, BindGroupLayoutEntry, CachedRenderPipelineId,
            ColorTargetState, ColorWrites, FragmentState, MultisampleState, PipelineCache,
            PrimitiveState, RenderPipelineDescriptor, ShaderStages, VertexState,
        },
        renderer::RenderDevice,
    },
    shader::Shader,
};
use bevy_egui::{egui, egui::PaintCallbackInfo, render::EguiPipelineKey};
use nalgebra::Vector2;
use wgpu::{
    AddressMode, BindGroupDescriptor, BindGroupEntry, BindingResource, BindingType, BlendState,
    Color, CommandEncoder, Device, Extent3d, FilterMode, LoadOp, MipmapFilterMode, Operations, Queue,
    RenderPass, RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor,
    Sampler, SamplerBindingType, SamplerDescriptor, StoreOp, TextureDescriptor, TextureDimension,
    TextureFormat, TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor,
    TextureViewDimension, VertexAttribute, VertexBufferLayout, VertexFormat, VertexStepMode,
};

use crate::render::{
    consts::DEPTH_TEXTURE_FORMAT,
    interface::basis::BasisPipeline,
    slice_preview::{SlicePreviewPipeline, SlicePreviewRenderResources},
    workspace::{WorkspaceRenderResources, model::ModelPipeline},
};

pub mod camera;
pub mod consts;
pub mod interface;
pub mod scene;
pub mod slice_preview;
pub mod util;
pub mod workspace;

pub const VERTEX_BUFFER_LAYOUT: VertexBufferLayout = VertexBufferLayout {
    array_stride: 4 * 3,
    step_mode: VertexStepMode::Vertex,
    attributes: &[VertexAttribute {
        format: VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    }],
};

/// The format of everything the mslicer passes render into.
///
/// Previously this was the eframe/egui-wgpu surface format, which `egui_wgpu` picks as the first
/// non-sRGB `Rgba8Unorm`/`Bgra8Unorm` the surface offers (egui-wgpu's shader even calls the
/// matching entry point "what we prefer"). The mslicer shaders are authored for that space — they
/// pass display-referred values straight through (`LinearRgb::to_srgb` is applied to the preview
/// background in `ModelPipeline::prepare_preview`, and `base.wgsl` writes hand-picked colours like
/// `vec3f(0.67, 0.65, 0.38)`), and `preview::download_preview` reads the format back raw.
/// So the canvas must stay non-sRGB, and [`blit`] converts its contents into whatever space the
/// egui pass' target expects.
pub const RENDER_TARGET_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

/// The number of MSAA samples the mslicer pipelines are built with.
pub const RENDER_SAMPLES: u32 = 4;

pub struct Gcx {
    pub device: Device,
    pub queue: Queue,
    pub texture: TextureFormat,
}

/// All the GPU pipelines the mslicer passes use. Created once at startup.
pub struct RenderResources {
    pub workspace: WorkspaceRenderResources,
    pub slice_preview: SlicePreviewRenderResources,
    pub basis: BasisPipeline,

    /// Sampler used when presenting a canvas into the egui pass.
    pub blit_sampler: Arc<Sampler>,

    /// Screen sized shared render target for the current frame.
    pub canvas: Canvas,
}

impl RenderResources {
    pub fn new(device: &Device, format: TextureFormat) -> Self {
        Self {
            workspace: WorkspaceRenderResources {
                model: ModelPipeline::new(device, format),
            },
            slice_preview: SlicePreviewRenderResources {
                slice_preview_pipeline: SlicePreviewPipeline::new(device, format),
            },
            basis: BasisPipeline::new(device, format),

            blit_sampler: Arc::new(device.create_sampler(&SamplerDescriptor {
                label: Some("mslicer blit"),
                address_mode_u: AddressMode::ClampToEdge,
                address_mode_v: AddressMode::ClampToEdge,
                address_mode_w: AddressMode::ClampToEdge,
                mag_filter: FilterMode::Nearest,
                min_filter: FilterMode::Nearest,
                mipmap_filter: MipmapFilterMode::Nearest,
                ..Default::default()
            })),

            canvas: Canvas::default(),
        }
    }
}

/// A screen sized offscreen target the mslicer passes render into.
///
/// All of a frame's callbacks share one canvas, exactly like they used to share the egui pass'
/// MSAA color + depth attachments, so inter-callback depth testing (e.g. the basis widget
/// against viewport geometry) still happens.
#[derive(Default)]
pub struct Canvas {
    inner: Option<CanvasTextures>,
    started: bool,
}

struct CanvasTextures {
    size: Vector2<u32>,
    color: TextureView,
    resolve: Arc<TextureView>,
    depth: TextureView,
}

impl Canvas {
    /// Make sure the canvas exists and matches `size`, and start a new frame.
    pub fn prepare(&mut self, device: &Device, size: Vector2<u32>) {
        self.started = false;

        if let Some(inner) = &self.inner
            && inner.size == size
        {
            return;
        }
        if size.x == 0 || size.y == 0 {
            self.inner = None;
            return;
        }

        let extent = Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        };

        let color = device.create_texture(&TextureDescriptor {
            label: Some("mslicer canvas color"),
            size: extent,
            mip_level_count: 1,
            sample_count: RENDER_SAMPLES,
            dimension: TextureDimension::D2,
            format: RENDER_TARGET_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolve = device.create_texture(&TextureDescriptor {
            label: Some("mslicer canvas resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: RENDER_TARGET_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&TextureDescriptor {
            label: Some("mslicer canvas depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: RENDER_SAMPLES,
            dimension: TextureDimension::D2,
            format: DEPTH_TEXTURE_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        self.inner = Some(CanvasTextures {
            size,
            color: color.create_view(&TextureViewDescriptor::default()),
            resolve: Arc::new(resolve.create_view(&TextureViewDescriptor::default())),
            depth: depth.create_view(&TextureViewDescriptor::default()),
        });
    }

    /// The size of the canvas in physical pixels, or zero if there is no canvas this frame (the
    /// window is minimized).
    pub fn size(&self) -> Vector2<u32> {
        self.inner.as_ref().map_or(Vector2::zeros(), |x| x.size)
    }

    pub fn resolve_view(&self) -> Option<Arc<TextureView>> {
        self.inner.as_ref().map(|x| x.resolve.clone())
    }

    /// Begins a render pass over the canvas. The color/depth are cleared on the first pass of a
    /// frame and loaded afterwards, so the callbacks in a frame accumulate like they used to.
    pub fn begin_pass<'a>(&mut self, encoder: &'a mut CommandEncoder) -> Option<RenderPass<'a>> {
        let inner = self.inner.as_ref()?;

        let load = if self.started {
            LoadOp::Load
        } else {
            LoadOp::Clear(Color::TRANSPARENT)
        };
        let depth_load = if self.started {
            LoadOp::Load
        } else {
            LoadOp::Clear(1.0)
        };
        let stencil_load = if self.started {
            LoadOp::Load
        } else {
            LoadOp::Clear(0)
        };
        self.started = true;

        Some(encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("mslicer canvas"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &inner.color,
                depth_slice: None,
                resolve_target: Some(&inner.resolve),
                ops: Operations {
                    load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &inner.depth,
                depth_ops: Some(Operations {
                    load: depth_load,
                    // Kept so later callbacks in the same frame can depth test against the
                    // earlier ones, exactly like they did when they shared the egui pass'
                    // depth attachment.
                    store: StoreOp::Store,
                }),
                stencil_ops: Some(Operations {
                    load: stencil_load,
                    store: StoreOp::Store,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }))
    }
}

/// A paint callback's viewport, in physical pixels with a top-left origin — matching what the
/// egui pass sets before invoking a callback.
#[derive(Clone, Copy, Debug)]
pub struct ViewportPixels {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ViewportPixels {
    /// The viewport of a paint callback, clamped to the size of the canvas it renders into.
    ///
    /// `canvas_size` is passed instead of `info.screen_size_px` so the viewport/scissor can never
    /// point outside the canvas, which the wgpu validation layer rejects.
    pub fn from_info(info: &PaintCallbackInfo, canvas_size: Vector2<u32>) -> Self {
        Self::from_points(&info.viewport, info.pixels_per_point, canvas_size)
    }

    /// Mirrors `egui::epaint::ViewportInPixels::from_points`.
    pub fn from_points(rect: &egui::Rect, pixels_per_point: f32, screen: Vector2<u32>) -> Self {
        let left = (pixels_per_point * rect.min.x).round() as i32;
        let top = (pixels_per_point * rect.min.y).round() as i32;
        let right = (pixels_per_point * rect.max.x).round() as i32;
        let bottom = (pixels_per_point * rect.max.y).round() as i32;

        let (screen_width, screen_height) = (screen.x as i32, screen.y as i32);
        let left = left.clamp(0, screen_width);
        let right = right.clamp(left, screen_width);
        let top = top.clamp(0, screen_height);
        let bottom = bottom.clamp(top, screen_height);

        Self {
            x: left as u32,
            y: top as u32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn apply(&self, pass: &mut wgpu::RenderPass) {
        if self.is_empty() {
            return;
        }

        pass.set_viewport(
            self.x as f32,
            self.y as f32,
            self.width as f32,
            self.height as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(self.x, self.y, self.width, self.height);
    }
}

/// Resource holding the pipeline used to present a canvas into the egui pass.
///
/// This lives in the render world because the pipeline has to be built for the egui pass' target
/// format, which is only known there.
#[derive(Resource)]
pub struct Blitter {
    shader: Handle<Shader>,
    layout: BindGroupLayoutDescriptor,
    pipelines: Mutex<HashMap<TextureFormat, CachedRenderPipelineId>>,
}

impl Blitter {
    pub fn new(shader: Handle<Shader>) -> Self {
        Self {
            shader,
            layout: blit_layout(),
            pipelines: Mutex::new(HashMap::new()),
        }
    }

    fn pipeline(&self, format: TextureFormat) -> Option<CachedRenderPipelineId> {
        self.pipelines.lock().get(&format).copied()
    }
}

fn blit_layout() -> BindGroupLayoutDescriptor {
    BindGroupLayoutDescriptor::new(
        "mslicer blit",
        &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: true },
                    view_dimension: TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    )
}

/// Makes sure the pipeline that presents a canvas into the egui pass exists for `format`.
///
/// Retries every frame until the pipeline is actually ready, so a shader that has not been
/// extracted yet (or a failed compile) cannot wedge the blit into a permanent no-op.
pub(crate) fn ensure_blit_pipeline(world: &mut World, format: TextureFormat) {
    let cached = world.resource::<Blitter>().pipeline(format);
    if let Some(id) = cached
        && world.resource::<PipelineCache>().get_render_pipeline(id).is_some()
    {
        return;
    }

    let id = cached.unwrap_or_else(|| {
        let blitter = world.resource::<Blitter>();
        let descriptor = RenderPipelineDescriptor {
            label: Some("mslicer blit".into()),
            layout: vec![blitter.layout.clone()],
            immediate_size: 0,
            vertex: VertexState {
                shader: blitter.shader.clone(),
                shader_defs: vec![],
                entry_point: Some("vert".into()),
                buffers: vec![],
                constants: vec![],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                shader: blitter.shader.clone(),
                shader_defs: vec![],
                entry_point: Some("frag".into()),
                targets: vec![Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
                constants: vec![],
            }),
            zero_initialize_workgroup_memory: false,
        };

        world
            .resource::<PipelineCache>()
            .queue_render_pipeline(descriptor)
    });

    world
        .get_resource_mut::<PipelineCache>()
        .expect("PipelineCache is always present")
        .block_on_render_pipeline(id);

    if world.resource::<PipelineCache>().get_render_pipeline(id).is_some() {
        world.resource::<Blitter>().pipelines.lock().insert(format, id);
    }
}

/// Draws `source` (a canvas resolve target) over the current egui viewport.
///
/// This runs inside the egui pass, which has no depth attachment and no MSAA, so it can only
/// composite a finished image.
pub fn blit(
    render_pass: &mut TrackedRenderPass,
    source: &TextureView,
    sampler: &Sampler,
    key: EguiPipelineKey,
    world: &World,
) {
    let Some(pipeline_id) = world.resource::<Blitter>().pipeline(key.target_format) else {
        tracing::warn!("mslicer blit: no pipeline id for {:?}", key.target_format);
        return;
    };
    let Some(pipeline) = world
        .resource::<PipelineCache>()
        .get_render_pipeline(pipeline_id)
    else {
        tracing::warn!(
            "mslicer blit: pipeline not ready: {:?}",
            world
                .resource::<PipelineCache>()
                .get_render_pipeline_state(pipeline_id)
        );
        return;
    };

    let layout = world
        .resource::<PipelineCache>()
        .get_bind_group_layout(&world.resource::<Blitter>().layout);
    let bind_group = world
        .resource::<RenderDevice>()
        .wgpu_device()
        .create_bind_group(&BindGroupDescriptor {
            label: Some("mslicer blit"),
            layout: &layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(source),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(sampler),
                },
            ],
        });

    let pass = render_pass.wgpu_pass();
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}
