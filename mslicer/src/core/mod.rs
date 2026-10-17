use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
};

use egui::{Context, Theme, Ui, Visuals};
use egui_tracing::EventCollector;
use nalgebra::{Vector2, Vector3};
use remote_print::manager::RemotePrintManager;
use tracing::{info, warn};
use wgpu::{Device, Queue};

use crate::{
    core::{
        config::{Config, printers::selected_printer},
        history::History,
        project::Project,
        slice::SliceOperation,
        state::{RemotePrintConnectStatus, UiState, WorkspaceHover},
    },
    interface::{
        self,
        panels::{self, Panels},
        popup::PopupManager,
    },
    render::{
        Gcx, RENDER_TARGET_FORMAT, RenderResources,
        camera::{Camera, spacenav::SpaceNav},
        workspace::model,
    },
    task::{PrinterScan, TaskManager, update_check_if_scheduled},
    util::fps_tracker::FpsTracker,
};

pub mod config;
pub mod history;
pub mod project;
pub mod selected;
pub mod slice;
pub mod state;

/// The wgpu device and queue bevy created for us.
#[derive(Clone)]
pub struct Gpu {
    pub device: Device,
    pub queue: Queue,
}

/// Everything the app owns except its GPU pipelines.
///
/// [`App`] derefs to this so the UI code can keep using `app.config`, `app.project`, `app.state`,
/// ... exactly as before, while `App` itself can hand out `&mut AppCore` and `&mut RenderResources`
/// at the same time (the render passes need both).
pub struct AppCore {
    // Core
    pub config_dir: PathBuf,
    pub config: Config,
    pub project: Project,
    pub history: History,
    pub remote_print: RemotePrintManager,
    pub slice_operation: Option<SliceOperation>,

    // Rendering
    pub gpu: Gpu,
    /// Physical size of the window's render target, refreshed once per frame.
    pub screen_size: Vector2<u32>,
    /// egui's pixels-per-point, refreshed once per frame.
    pub pixels_per_point: f32,
    pub panels: Panels,
    pub fps: FpsTracker,
    pub camera: Camera,
    pub spacenav: SpaceNav,

    // Interface
    pub state: UiState,
    pub popup: PopupManager,
    pub tasks: TaskManager,
    /// Set by the quit shortcut; the host turns it into an [`bevy::app::AppExit`].
    pub quit: bool,
}

pub struct App {
    pub core: AppCore,
    pub render: RenderResources,
}

impl Deref for App {
    type Target = AppCore;

    fn deref(&self) -> &AppCore {
        &self.core
    }
}

impl DerefMut for App {
    fn deref_mut(&mut self) -> &mut AppCore {
        &mut self.core
    }
}

impl AppCore {
    pub fn gcx(&self) -> Gcx {
        Gcx {
            device: self.gpu.device.clone(),
            queue: self.gpu.queue.clone(),
            texture: RENDER_TARGET_FORMAT,
        }
    }

    pub fn title(&self) -> String {
        if let Some(stem) = self.project.path.as_ref().and_then(|x| x.file_stem()) {
            format!("mslicer — {}", stem.to_string_lossy())
        } else {
            "mslicer".into()
        }
    }
}

impl App {
    pub fn init(
        gpu: Gpu,
        config_dir: PathBuf,
        mut config: Config,
        event_collector: EventCollector,
    ) -> Self {
        let mut spacenav = SpaceNav::unconnected();
        spacenav.try_connect();

        let slice_config = config.default_slice_config.clone();
        let mut this = Self {
            render: RenderResources::new(&gpu.device, RENDER_TARGET_FORMAT),
            core: AppCore {
                panels: Panels::new(&mut config),
                fps: FpsTracker::new(),
                config_dir,
                popup: PopupManager::default(),
                tasks: TaskManager::new(),
                remote_print: RemotePrintManager::default(),
                slice_operation: None,
                camera: Camera::default(),
                spacenav,
                gpu,
                screen_size: Vector2::zeros(),
                pixels_per_point: 1.0,
                state: UiState {
                    event_collector,
                    selected_printer: selected_printer(&config, &slice_config),
                    ..Default::default()
                },
                history: History::default(),
                config,
                project: Project {
                    slice_config,
                    ..Default::default()
                },
                quit: false,
            },
        };

        update_check_if_scheduled(&mut this);
        if !this.remote_print.is_initialized() && this.config.remote_print.init_at_startup {
            panels::remote_print::initialize(&mut this);

            // Initial scan
            this.state.remote_print_connecting = RemotePrintConnectStatus::Scanning;
            let AppCore {
                config,
                remote_print,
                tasks,
                ..
            } = &mut this.core;
            tasks.add(PrinterScan::new(
                remote_print,
                config.remote_print.broadcast_address,
            ));
        }

        this
    }

    pub fn is_slicing(&self) -> bool {
        is_slicing(&self.slice_operation)
    }

    pub fn hovered_ray(&self) -> Option<(Vector3<f32>, Vector3<f32>)> {
        let projection = self.config.render.projection;
        let workspace @ WorkspaceHover { aspect, uv, .. } = &self.state.workspace;

        workspace
            .hovered()
            .then(|| self.camera.hovered_ray(projection, *aspect, *uv))
    }

    /// Prepares the shared canvas for a new frame.
    pub fn begin_frame(&mut self, screen_size: Vector2<u32>, pixels_per_point: f32) {
        self.core.screen_size = screen_size;
        self.core.pixels_per_point = pixels_per_point;
        self.render
            .canvas
            .prepare(&self.core.gpu.device, screen_size);
    }

    /// Runs one frame of the egui UI, which also drives the mslicer render passes.
    pub fn frame(&mut self, ui: &mut Ui, ctx: &Context) {
        ctx.request_repaint();

        self.project.update();
        self.panels.update(ctx.viewport_rect().width());
        self.fps.update();
        let AppCore { config, history, .. } = &mut self.core;
        history.set_max_mesh_size(config.ui.history_max_mesh_size);
        self.popup().render(ctx);
        self.tasks().poll();

        // todo: only update the visuals if the theme has changed?
        ctx.set_visuals(Visuals {
            collapsing_header_frame: true,
            ..match self.config.ui.theme {
                Theme::Dark => Visuals::dark(),
                Theme::Light => Visuals::light(),
            }
        });

        model::process_previews(&mut self.core, &mut self.render.workspace.model);
        interface::ui(self, ui, ctx);
    }

    /// Called when the app is asked to quit.
    pub fn save_config(&mut self) {
        // todo: save all surfaces (except slice operation?)
        let AppCore { config, panels, .. } = &mut self.core;
        config.ui.panels = Some(panels.main_surface().clone());
        if let Err(err) = self.config.save(&self.config_dir) {
            warn!("Failed to save config: {}", err);
        } else {
            info!("Successfully saved config");
        }
    }
}

pub fn is_slicing(slice_operation: &Option<SliceOperation>) -> bool {
    slice_operation
        .as_ref()
        .map(|x| !x.progress.complete())
        .unwrap_or_default()
}
