use common::color::SRgb;
use nalgebra::Vector2;
use wgpu::{
    BindGroup, Buffer, BufferUsages, CommandEncoder, Device, Origin3d, RenderPass, Sampler,
    TexelCopyTextureInfo, TextureAspect, TextureFormat, TextureView,
    util::{BufferInitDescriptor, DeviceExt},
};

use crate::{
    core::AppCore,
    render::{
        Gcx,
        consts::{FILTERING_SAMPLER, NONFILTERING_SAMPLER},
        workspace::model::{
            bindings::occlusion_size,
            pass::{
                base::{BasePass, BaseResources},
                blur::BlurPass,
                composite::CompositePass,
                fxaa::FxaaPass,
                lighting::LightingPass,
                ssao::SsaoPass,
            },
        },
    },
};

mod bindings;
mod pass;
mod preview;
mod selected;
pub use preview::process_previews;

/// Renders the offscreen model preview the sliced panel downloads.
///
/// This used to also render the viewport; that is now a real bevy scene
/// (`crate::render::scene`), so only the preview target is left.
pub struct ModelPipeline {
    preview: Option<MultiStage>,

    base: BasePass,
    ssao: SsaoPass,
    blur: BlurPass,
    lighting: LightingPass,
    fxaa: FxaaPass,
    composite: CompositePass,

    post_index_buffer: Buffer,
    sampler: Sampler,
    filtering_sampler: Sampler,
}

struct MultiStage {
    target_a: TextureView,
    target_b: TextureView,

    occlusion_target_a: TextureView,
    occlusion_target_b: TextureView,

    // g buffer
    depth_target: TextureView,
    normal_target: TextureView,
    world_target: TextureView,

    //bind groups!
    base: BaseResources,
    ssao: BindGroup,
    blur: BindGroup,
    lighting: BindGroup,
    fxaa: BindGroup,
    composite: BindGroup,
}

impl ModelPipeline {
    pub fn new(device: &Device, texture: TextureFormat) -> Self {
        let sampler = device.create_sampler(&NONFILTERING_SAMPLER);
        let filtering_sampler = device.create_sampler(&FILTERING_SAMPLER);

        let post_index_buffer = device.create_buffer_init(&BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&[0, 1, 2]),
            usage: BufferUsages::INDEX | BufferUsages::COPY_DST,
        });

        Self {
            preview: None,

            base: BasePass::create(device, texture),
            ssao: SsaoPass::create(device),
            blur: BlurPass::create(device),
            lighting: LightingPass::create(device, texture),
            fxaa: FxaaPass::create(device, texture),
            composite: CompositePass::create(device, texture),

            post_index_buffer,
            sampler,
            filtering_sampler,
        }
    }

    fn render(
        &mut self,
        encoder: &mut CommandEncoder,
        app: &mut AppCore,
        background: Option<SRgb<f32>>,
    ) {
        let multi = self.preview.as_ref().unwrap();
        let index = &self.post_index_buffer;

        self.base.paint_preview(encoder, app, multi);
        if app.config.render.ambient_occlusion.enabled {
            self.ssao.paint(encoder, multi, index, &multi.ssao);
            self.blur.paint(encoder, multi, index, &multi.blur);
        }

        self.lighting
            .paint(encoder, multi, index, background, &multi.lighting);
        if app.config.render.anti_aliasing.enabled {
            self.fxaa.paint(encoder, multi, index, &multi.fxaa);
        } else {
            encoder.copy_texture_to_texture(
                TexelCopyTextureInfo {
                    texture: multi.target_b.texture(),
                    mip_level: 0,
                    origin: Origin3d::ZERO,
                    aspect: TextureAspect::All,
                },
                TexelCopyTextureInfo {
                    texture: multi.target_a.texture(),
                    mip_level: 0,
                    origin: Origin3d::ZERO,
                    aspect: TextureAspect::All,
                },
                multi.target_a.texture().size(),
            );
        }
    }

    pub fn prepare_preview(
        &mut self,
        gcx: &Gcx,
        encoder: &mut CommandEncoder,
        app: &mut AppCore,
        size: Vector2<u32>,
    ) {
        let Some(Some(interactive)) =
            (app.slice_operation.as_ref()).map(|x| x.interactive_previews.as_ref())
        else {
            return;
        };

        let aspect = size.x as f32 / size.y as f32;
        let camera = interactive.camera.lock().clone();

        let preview = &app.config.render.preview;
        let background = preview.background;
        let view = camera.view_projection_matrix(preview.projection, aspect);

        self.size_textures(gcx, app, size);
        let multi = self.preview.as_mut().unwrap();
        multi.base.prepare_preview(gcx, app, &self.base, view);

        self.ssao.prepare(gcx, app, view);
        self.blur.prepare(gcx, app, occlusion_size(app, size));
        self.lighting.prepare(gcx, app, Some(&camera));
        self.fxaa.prepare(gcx, app, size);
        self.render(encoder, app, Some(background.to_srgb()));
    }

    // Runs the post-processing pipeline, copying the data from the intermediary
    // buffers to the output surface.
    pub fn paint(&self, render_pass: &mut RenderPass, _app: &mut AppCore) {
        let bindings = &self.preview.as_ref().unwrap().composite;
        self.composite
            .paint(render_pass, &self.post_index_buffer, bindings);
    }
}
