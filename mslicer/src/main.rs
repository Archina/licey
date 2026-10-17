#![windows_subsystem = "windows"]

use std::{
    fs::{self, File},
    panic,
    path::PathBuf,
    sync::Arc,
    thread,
};

use bevy::{
    app::AppExit,
    ecs::message::Messages,
    prelude::*,
    render::{
        Render, RenderApp, RenderPlugin, RenderSystems,
        pipelined_rendering::PipelinedRenderingPlugin,
        renderer::{RenderDevice, RenderQueue},
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
    },
    shader::Shader,
    window::{MonitorSelection, PrimaryWindow, WindowPlugin, WindowPosition, WindowResolution},
};
use bevy_egui::{
    EguiContext, EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass,
    PrimaryEguiContext,
};
use egui::{FontDefinitions, Id, LayerId, Ui, UiBuilder};
use nalgebra::Vector2;
use parking_lot::Mutex;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{filter, fmt::layer, layer::SubscriberExt, util::SubscriberInitExt};

const VERSION: &str = env!("CARGO_PKG_VERSION");

mod core;
mod interface;
mod render;
mod system;
mod task;
mod util;

use crate::{
    core::{App as MslicerApp, Gpu, config::Config},
    render::{Blitter, scene::ScenePlugin},
    system::{arguments::Args, icon},
};

/// Bevy owns the device/queue; this is how the mslicer app gets a hold of them.
///
/// The main world installs the handle, the render world fills it in as soon as it can, and the
/// main world picks it up on the next frame.
#[derive(Resource, Clone, Default)]
struct GpuHandle(Arc<Mutex<Option<Gpu>>>);

impl GpuHandle {
    fn get(&self) -> Option<Gpu> {
        self.0.lock().clone()
    }
}

/// The values `main` needs to hand over to the startup system.
#[derive(Resource)]
struct Startup {
    args: Args,
    config_dir: PathBuf,
    config: Config,
    collector: egui_tracing::EventCollector,
}

struct MslicerPlugin;

impl Plugin for MslicerPlugin {
    fn build(&self, app: &mut App) {
        let shader = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .add(Shader::from_wgsl(
                include_str!("../shaders/blit.wgsl"),
                "mslicer/blit.wgsl",
            ));

        let gpu = GpuHandle::default();
        app.insert_resource(gpu.clone());

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(Blitter::new(shader));
            render_app.insert_resource(gpu.clone());
            render_app.add_systems(Render, capture_gpu.in_set(RenderSystems::PrepareResources));
        }

        app.add_systems(PreStartup, spawn_camera)
            .add_systems(Startup, setup_egui)
            .add_systems(EguiPrimaryContextPass, run_app)
            .add_systems(Last, (save_on_exit, set_window_icon))
            .init_resource::<WindowIconSet>();
    }
}

/// Whether the window icon has been applied yet.
#[derive(Resource, Default)]
struct WindowIconSet(bool);

/// Applies the app's icon to the winit window.
///
/// Bevy has no window icon API, so this reaches through `bevy::winit`'s `WINIT_WINDOWS` into the
/// underlying `winit::window::Window`. Retries until the window exists.
fn set_window_icon(mut set: ResMut<WindowIconSet>, windows: Query<Entity, With<PrimaryWindow>>) {
    if set.0 {
        return;
    }
    let Ok(entity) = windows.single() else {
        return;
    };

    let data = icon();
    let set_ok = bevy::winit::WINIT_WINDOWS.with_borrow(|winit_windows| {
        let Some(window) = winit_windows.get_window(entity) else {
            return false;
        };
        window.set_window_icon(winit::window::Icon::from_rgba(data.rgba, data.width, data.height).ok());
        true
    });

    set.0 = set_ok;
}

/// Captures the renderer's device/queue so the main world can use it.
fn capture_gpu(
    handle: Res<GpuHandle>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let mut guard = handle.0.lock();
    if guard.is_none() {
        let wgpu_device = device.wgpu_device();
        info!("Acquired wgpu device: {:?}", wgpu_device.adapter_info().name);
        *guard = Some(Gpu {
            device: wgpu_device.clone(),
            queue: (**queue).clone(),
        });
    }
}

/// Spawns the camera that owns the primary Egui context.
fn spawn_camera(mut commands: Commands) {
    // Bevy only owns the window and the egui pass here; everything else is drawn by mslicer, so
    // the camera has nothing to tonemap (the default `TonyMcMapFace` also needs the
    // `tonemapping_luts` feature, which isn't enabled).
    commands.spawn((
        Camera3d::default(),
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        bevy::core_pipeline::tonemapping::DebandDither::Disabled,
        PrimaryEguiContext,
    ));
}

/// Loads the app's fonts into the primary Egui context.
fn setup_egui(mut egui_contexts: EguiContexts) -> Result {
    let ctx = egui_contexts.ctx_mut()?;

    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    egui_extras::install_image_loaders(ctx);

    Ok(())
}

/// Runs one frame of the mslicer app, building it the first time the renderer is ready.
///
/// The app is stored boxed so that its address stays stable for the whole process: paint
/// callbacks handed to bevy_egui hold a raw pointer to it and are only dereferenced later, from
/// the render app. (This is also why pipelined rendering is disabled — the render app must run on
/// the main thread after the frame that created the callbacks.)
fn run_app(world: &mut World) -> Result {
    if world.get_non_send::<Box<MslicerApp>>().is_none() {
        let Some(gpu) = world.resource::<GpuHandle>().get() else {
            // The renderer isn't up yet; try again next frame.
            return Ok(());
        };
        let Some(startup) = world.remove_resource::<Startup>() else {
            return Ok(());
        };

        let mut app = MslicerApp::init(gpu, startup.config_dir, startup.config, startup.collector);
        startup.args.open.start(&mut app);
        world.insert_non_send(Box::new(app));
    }

    let Some(ctx) = egui_context(world) else {
        return Ok(());
    };
    let Some((screen_size, current_title)) = window_title_and_size(world) else {
        return Ok(());
    };

    let mut app = world
        .remove_non_send::<Box<MslicerApp>>()
        .expect("just checked");

    let title = app.title();
    if title != current_title {
        set_window_title(world, title);
    }

    app.begin_frame(screen_size, ctx.pixels_per_point());

    let mut ui = Ui::new(
        ctx.clone(),
        Id::new((ctx.viewport_id(), "__top_ui")),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    app.frame(&mut ui, &ctx);

    let quit = app.quit;
    world.insert_non_send(app);

    if quit {
        world
            .resource_mut::<Messages<AppExit>>()
            .write(AppExit::Success);
    }

    Ok(())
}

fn egui_context(world: &mut World) -> Option<egui::Context> {
    let mut query = world.query::<&mut EguiContext>();
    let mut context = query.iter_mut(world).next()?;
    Some(context.get_mut().clone())
}

fn window_title_and_size(world: &mut World) -> Option<(Vector2<u32>, String)> {
    let mut query = world.query_filtered::<&Window, With<PrimaryWindow>>();
    let window = query.iter(world).next()?;
    Some((
        Vector2::new(window.physical_width(), window.physical_height()),
        window.title.clone(),
    ))
}

fn set_window_title(world: &mut World, title: String) {
    let mut query = world.query_filtered::<&mut Window, With<PrimaryWindow>>();
    if let Some(mut window) = query.iter_mut(world).next() {
        window.title = title;
    }
}

/// Writes the config out when the app is exiting (window closed or the quit shortcut).
fn save_on_exit(world: &mut World) {
    let exiting = world
        .get_resource::<Messages<AppExit>>()
        .is_some_and(|x| !x.is_empty());

    if exiting && let Some(mut app) = world.remove_non_send::<Box<MslicerApp>>() {
        app.save_config();
    }
}

fn main() -> anyhow::Result<()> {
    // Don't print panics on threads that are handled by the task system.
    let old_panic = panic::take_hook();
    panic::set_hook(Box::new(move |panic| {
        (thread::current().name() != Some("task_thread")).then(|| old_panic(panic));
    }));

    let filter = filter::Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target("experimental", LevelFilter::TRACE)
        .with_target("mslicer", LevelFilter::TRACE)
        .with_target("remote_print", LevelFilter::TRACE)
        .with_target("slicer", LevelFilter::TRACE)
        .with_target("tools", LevelFilter::TRACE);
    let format = tracing_subscriber::fmt::layer();
    let collector = egui_tracing::EventCollector::new();

    let config_dir = dirs::config_dir().unwrap().join("mslicer");
    anyhow::Context::context(fs::create_dir_all(&config_dir), "Creating config dir")?;

    let log_file = anyhow::Context::context(
        File::create(config_dir.join("latest.log")),
        "Opening log file",
    )?;
    let file_layer = layer().with_writer(log_file).with_ansi(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(format)
        .with(collector.clone())
        .with(file_layer)
        .init();
    info!("Starting mslicer v{VERSION}");

    let args = Args::parse();
    #[allow(unused_mut)]
    let mut config = Config::load_or_default(&config_dir);

    #[cfg(windows)]
    crate::system::windows::check_install(&mut config, &args)?;
    #[cfg(not(windows))]
    if args.install {
        tracing::warn!("--install has no effect on non-windows platforms, ignoring.")
    }

    let mut app = bevy::app::App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            // we set up our own tracing subscriber above
            .disable::<bevy::log::LogPlugin>()
            // The mslicer paint callbacks hold a raw pointer to the app and render into a single
            // shared canvas, both of which are only sound if the render app runs on the main
            // thread, after the frame that created them.
            .disable::<PipelinedRenderingPlugin>()
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "mslicer".into(),
                    resolution: WindowResolution::new(1920, 1080),
                    position: WindowPosition::Centered(MonitorSelection::Primary),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    // `Functionality` (the default) asks the adapter for its full set of features
                    // and limits, which is a superset of the `POLYGON_MODE_LINE | IMMEDIATES |
                    // PRIMITIVE_INDEX` and `max_color_attachment_bytes_per_sample: 40` the mslicer
                    // passes need.
                    priority: WgpuSettingsPriority::Functionality,
                    features: wgpu::Features::POLYGON_MODE_LINE
                        | wgpu::Features::IMMEDIATES
                        | wgpu::Features::PRIMITIVE_INDEX,
                    ..default()
                })),
                ..default()
            }),
    )
    // Matches eframe's default clear color (`Color32::from_rgba_unmultiplied(12, 12, 12, 180)`).
    .insert_resource(ClearColor(Color::srgba(
        12.0 / 255.0,
        12.0 / 255.0,
        12.0 / 255.0,
        180.0 / 255.0,
    )))
    .insert_resource(EguiGlobalSettings {
        auto_create_primary_context: false,
        ..default()
    })
    .add_plugins(EguiPlugin::default())
    .add_plugins(ScenePlugin)
    .add_plugins(MslicerPlugin)
    .insert_resource(Startup {
        args,
        config_dir,
        config,
        collector,
    });

    app.run();

    Ok(())
}
