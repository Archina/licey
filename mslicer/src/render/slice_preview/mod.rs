use std::sync::Arc;

use bevy::{
    prelude::*,
    render::{render_phase::TrackedRenderPass, sync_world::RenderEntity},
};
use bevy_egui::{
    egui::PaintCallbackInfo, render::EguiBevyPaintCallbackImpl, render::EguiPipelineKey,
};
use common::container::Run;
use nalgebra::Vector2;
use wgpu::{CommandEncoder, TextureFormat};

use crate::{
    core::{App, AppCore},
    render::{Canvas, Gcx, ViewportPixels, blit, ensure_blit_pipeline},
};

mod decompress;
mod pipeline;
pub use pipeline::SlicePreviewPipeline;

pub struct SlicePreviewRenderResources {
    pub slice_preview_pipeline: SlicePreviewPipeline,
}

/// Renders and presents a slice layer (the 2D layer viewer) into the sliced panel.
pub struct SlicePreviewRenderCallback {
    pub app: *mut App,

    pub dimensions: Vector2<u32>,
    pub offset: Vector2<f32>,
    pub scale: Vector2<f32>,
    pub rotation: f32,
    pub aspect: f32,
    pub pixel_aspect: f32,
    pub multisample: u32,

    pub new_preview: Option<(Arc<Vec<Run>>, Vec<Run>)>,
}

unsafe impl Send for SlicePreviewRenderCallback {}
unsafe impl Sync for SlicePreviewRenderCallback {}

impl SlicePreviewRenderResources {
    pub fn render(
        &mut self,
        app: &AppCore,
        encoder: &mut CommandEncoder,
        canvas: &mut Canvas,
        viewport: ViewportPixels,
        callback: &SlicePreviewRenderCallback,
    ) -> bool {
        let gcx = Gcx {
            device: app.gpu.device.clone(),
            queue: app.gpu.queue.clone(),
            texture: TextureFormat::R8Unorm, // random format bc its not actually used in this renderer
        };

        self.slice_preview_pipeline.prepare(&gcx, encoder, callback);

        let Some(mut pass) = canvas.begin_pass(encoder) else {
            return false;
        };
        viewport.apply(&mut pass);

        self.slice_preview_pipeline.paint(&mut pass);

        true
    }
}

impl EguiBevyPaintCallbackImpl for SlicePreviewRenderCallback {
    fn update(
        &self,
        info: PaintCallbackInfo,
        _render_entity: RenderEntity,
        key: EguiPipelineKey,
        world: &mut World,
    ) {
        ensure_blit_pipeline(world, key.target_format);

        let app = unsafe { &mut *self.app };
        let App { core, render } = app;

        let mut encoder = core.gpu.device.create_command_encoder(&Default::default());

        let viewport = ViewportPixels::from_info(&info, render.canvas.size());
        render.slice_preview.render(
            core,
            &mut encoder,
            &mut render.canvas,
            viewport,
            self,
        );

        core.gpu.queue.submit(Some(encoder.finish()));
    }

    fn render<'pass>(
        &self,
        _info: PaintCallbackInfo,
        render_pass: &mut TrackedRenderPass<'pass>,
        _render_entity: RenderEntity,
        key: EguiPipelineKey,
        world: &'pass World,
    ) {
        let app = unsafe { &*self.app };
        let Some(source) = app.render.canvas.resolve_view() else {
            return;
        };

        blit(render_pass, &source, &app.render.blit_sampler, key, world);
    }
}
