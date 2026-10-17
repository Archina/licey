use bevy::{
    prelude::*,
    render::{render_phase::TrackedRenderPass, sync_world::RenderEntity},
};
use bevy_egui::{egui::PaintCallbackInfo, render::EguiBevyPaintCallbackImpl, render::EguiPipelineKey};
use nalgebra::Vector2;
use wgpu::CommandEncoder;

use crate::{
    core::{App, AppCore},
    render::{Canvas, ViewportPixels, blit, ensure_blit_pipeline, workspace::model::ModelPipeline},
};

pub mod model;

pub struct WorkspaceRenderResources {
    /// The offscreen model preview the sliced panel downloads while slicing.
    ///
    /// The viewport itself is drawn by `crate::render::scene` — a bevy `Camera3d` rendering into
    /// an `Image` — so this pipe is all that is left of the old workspace rendering.
    pub model: ModelPipeline,
}

/// Renders and presents the offscreen model preview the sliced panel shows in its sidebar.
pub struct PreviewRenderCallback {
    pub app: *mut App,
    /// Size of the preview texture the model is rendered into.
    pub viewport: Vector2<u32>,
}

unsafe impl Send for PreviewRenderCallback {}
unsafe impl Sync for PreviewRenderCallback {}

impl WorkspaceRenderResources {
    /// Renders a preview image of the model (used in the sliced panel's tooltip preview).
    pub fn prepare_preview(
        &mut self,
        app: &mut AppCore,
        encoder: &mut CommandEncoder,
        size: Vector2<u32>,
    ) {
        let gcx = app.gcx();
        self.model.prepare_preview(&gcx, encoder, app, size);
    }

    pub fn paint_preview(
        &self,
        encoder: &mut CommandEncoder,
        app: &mut AppCore,
        canvas: &mut Canvas,
        viewport: ViewportPixels,
    ) -> bool {
        let Some(mut pass) = canvas.begin_pass(encoder) else {
            return false;
        };
        viewport.apply(&mut pass);

        self.model.paint(&mut pass, app);

        true
    }
}

impl EguiBevyPaintCallbackImpl for PreviewRenderCallback {
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
        render
            .workspace
            .prepare_preview(core, &mut encoder, self.viewport);

        let viewport = ViewportPixels::from_info(&info, render.canvas.size());
        render
            .workspace
            .paint_preview(&mut encoder, core, &mut render.canvas, viewport);

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
