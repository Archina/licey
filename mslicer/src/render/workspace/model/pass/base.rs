use std::num::NonZero;

use encase::{DynamicUniformBuffer, ShaderSize, ShaderType};
use nalgebra::{Matrix4, Vector3};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, BlendState, BufferBinding,
    BufferBindingType, BufferUsages, Color, ColorTargetState, ColorWrites, CommandEncoder,
    CompareFunction, DepthBiasState, DepthStencilState, Device, FragmentState, IndexFormat, LoadOp,
    Operations, PipelineLayoutDescriptor, RenderPass, RenderPassColorAttachment,
    RenderPassDepthStencilAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, ShaderStages, StencilFaceState, StencilState, StoreOp, TextureFormat,
    VertexState,
};

use crate::{
    core::{AppCore, config::render::RenderStyle},
    include_shader,
    render::{
        Gcx, VERTEX_BUFFER_LAYOUT,
        util::ResizingBuffer,
        workspace::model::{MultiStage, selected::Selected},
    },
};

pub struct BasePass {
    pipeline: RenderPipeline,
    group_layout: BindGroupLayout,
}

pub struct BaseResources {
    uniform: ResizingBuffer,
    uniform_offsets: Vec<u32>,
    selected: ResizingBuffer,
    bind_group: Option<BindGroup>,
}

#[derive(ShaderType, Clone)]
struct Uniforms {
    transform: Matrix4<f32>,
    model_transform: Matrix4<f32>,
    build_volume: Vector3<f32>,
    model_color: Vector3<f32>,
    render_style: u32,
    overhang_angle: f32,
    id: u32,
    selected_offset: u32,
}

impl BasePass {
    pub fn create(device: &Device, texture: TextureFormat) -> Self {
        let shader = device
            .create_shader_module(include_shader!("workspace/model/base.wgsl", "common.wgsl"));

        let group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: Some(Uniforms::SHADER_SIZE),
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZero::new(4),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Model"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: None,
                buffers: &[Some(VERTEX_BUFFER_LAYOUT)],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: None,
                targets: &[
                    // target
                    Some(ColorTargetState {
                        format: texture,
                        blend: Some(BlendState::ALPHA_BLENDING),
                        write_mask: ColorWrites::all(),
                    }),
                    // model
                    // normals
                    Some(ColorTargetState {
                        format: TextureFormat::Rgba16Float,
                        blend: None,
                        write_mask: ColorWrites::all(),
                    }),
                    // world space
                    Some(ColorTargetState {
                        format: TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: ColorWrites::all(),
                    }),
                ],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: Some(DepthStencilState {
                format: TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::Less),
                stencil: StencilState {
                    front: StencilFaceState::IGNORE,
                    back: StencilFaceState::IGNORE,
                    read_mask: 0,
                    write_mask: 0,
                },
                bias: DepthBiasState {
                    constant: 0,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            group_layout,
        }
    }

    fn render_pass<'a>(
        &self,
        encoder: &'a mut CommandEncoder,
        multi: &MultiStage,
    ) -> RenderPass<'a> {
        encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("Model"),
            color_attachments: &[
                Some(RenderPassColorAttachment {
                    view: &multi.target_a,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                    depth_slice: None,
                }),
                Some(RenderPassColorAttachment {
                    view: &multi.normal_target,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                    depth_slice: None,
                }),
                Some(RenderPassColorAttachment {
                    view: &multi.world_target,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                    depth_slice: None,
                }),
            ],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &multi.depth_target,
                depth_ops: Some(Operations {
                    load: LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    pub fn paint_preview(&self, encoder: &mut CommandEncoder, app: &mut AppCore, multi: &MultiStage) {
        let (Some(bind_group), Some(Some(interactive))) = (
            &multi.base.bind_group,
            (app.slice_operation.as_ref()).map(|x| x.interactive_previews.as_ref()),
        ) else {
            return;
        };

        let mut render_pass = self.render_pass(encoder, multi);
        render_pass.set_pipeline(&self.pipeline);

        for (i, model) in interactive.models.iter().enumerate() {
            render_pass.set_bind_group(0, bind_group, &[multi.base.uniform_offsets[i]]);

            let buffers = &model.mesh;
            render_pass.set_vertex_buffer(0, buffers.vertex_buffer.slice(..));
            render_pass.set_index_buffer(buffers.index_buffer.slice(..), IndexFormat::Uint32);
            render_pass.draw_indexed(0..(model.faces as u32 * 3), 0, 0..1);
        }
    }
}

impl BaseResources {
    pub fn new(device: &Device) -> Self {
        let uniform = ResizingBuffer::new(device, BufferUsages::UNIFORM | BufferUsages::COPY_DST);
        let selected = ResizingBuffer::new(device, BufferUsages::STORAGE | BufferUsages::COPY_DST);

        Self {
            uniform,
            selected,
            uniform_offsets: Vec::new(),
            bind_group: None,
        }
    }

    fn recreate_bind_group(&mut self, gcx: &Gcx, layout: &BindGroupLayout) {
        let bind_group = gcx.device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::Buffer(BufferBinding {
                        buffer: &self.uniform,
                        offset: 0,
                        size: Some(Uniforms::SHADER_SIZE),
                    }),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Buffer(BufferBinding {
                        buffer: &self.selected,
                        offset: 0,
                        size: None,
                    }),
                },
            ],
        });

        self.bind_group.replace(bind_group);
    }

    pub fn prepare_preview(
        &mut self,
        gcx: &Gcx,
        app: &mut AppCore,
        pass: &BasePass,
        view: Matrix4<f32>,
    ) {
        let Some(Some(interactive)) =
            (app.slice_operation.as_ref()).map(|x| x.interactive_previews.as_ref())
        else {
            return;
        };

        self.uniform_offsets.clear();

        let mut uniform_buffer = DynamicUniformBuffer::new(Vec::new());
        let mut selected_words = Vec::new();

        for model in interactive.models.iter() {
            let uniform = Uniforms {
                transform: view * model.transform,
                model_transform: model.transform,
                build_volume: Vector3::repeat(f32::MAX),
                model_color: model.color.into(),
                render_style: RenderStyle::Rendered as u32,
                overhang_angle: 0.0,
                id: 0,
                selected_offset: selected_words.len() as u32,
            };

            let selected = Selected::new(model.faces).into_inner();
            selected_words.extend_from_slice(&selected);

            let offset = uniform_buffer.write(&uniform);
            self.uniform_offsets.push(offset.unwrap() as u32);
        }

        selected_words.resize(selected_words.len().max(1), 0);
        let uniform = self.uniform.write(gcx, &uniform_buffer.into_inner());
        let selected = self.selected.write_slice(gcx, &selected_words);

        if !self.uniform_offsets.is_empty() && (uniform || selected) {
            self.recreate_bind_group(gcx, &pass.group_layout);
        }
    }
}
