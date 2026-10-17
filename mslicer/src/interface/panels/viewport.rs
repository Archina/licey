use bevy_egui::render::EguiBevyPaintCallback;
use egui::{
    Align2, Area, Button, Color32, Context, Frame, Id, Key, Order, Painter, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Theme, Ui, Vec2, Widget, pos2, vec2,
};
use nalgebra::{Rotation3, Vector2, Vector3};

use crate::{
    core::{
        App, AppCore,
        config::ui::HoverOverlay,
        state::{GeometryHit, GizmoTransform, Tool, ViewportRect, WorkspaceHover},
    },
    interface::{components::grid, panels::supports::manual_support_placement},
    render::interface::basis::BasisRenderCallback,
};

#[rustfmt::skip]
const NUMBER_KEYS: [Key; 10] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];

pub fn ui(app: &mut App, ui: &mut Ui, _ctx: &Context) {
    let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

    // While a gizmo handle is hovered or being dragged the gizmo owns the
    // pointer, so the camera must not orbit on the same drag.
    let gizmo_hot = app.state.gizmo_hot;
    if !gizmo_hot {
        app.camera.handle_movement(&response, ui);
    }

    let focused = ui.input(|i| i.focused);
    let mut is_moving = app.spacenav().handle_movement(focused);
    if is_moving {
        app.state.move_timeout = ((0.025 / app.fps.frame_time()).round() as u32).min(60);
    } else if app.state.move_timeout > 0 {
        app.state.move_timeout -= 1;
        is_moving = true;
    }
    is_moving |= response.dragged();

    let aspect = rect.width() / rect.height();
    let px = response.hover_pos().unwrap_or_default();
    let uv = (px - rect.min) / rect.size();
    app.state.workspace = WorkspaceHover::new(is_moving, aspect, uv);

    (app.state.tool == Tool::Support).then(|| manual_support_placement(app, false));
    if response.clicked() && !is_moving && !gizmo_hot {
        match app.state.tool {
            Tool::Select => {
                let AppCore { project, state, .. } = &mut app.core;
                if let Some(hover) = state.hovered_geometry {
                    let shift = ui.input(|x| x.modifiers.shift);
                    if hover.support {
                        if let Some(model) = project.model(hover.model)
                            && let Some(support) = model.supports.support_for_face(hover.face)
                        {
                            (state.selected_supports).support_clicked(model.id, support);
                        }
                    } else {
                        state.selected.model_clicked(hover.model, shift);
                    }
                }
            }
            Tool::Support => manual_support_placement(app, true),
            Tool::Orient => {
                let platform = app.project.slice_config.platform_size;
                if let Some(hover) = app.state.hovered_geometry
                    && let Some(model) = app.project.model(hover.model)
                {
                    let normal = model.mesh.normal(hover.face as usize);
                    let down = Vector3::new(0.0, 0.0, -1.0);
                    let matrix = Rotation3::rotation_between(&normal, &down);

                    let rotation = if let Some(matrix) = matrix {
                        let (roll, pitch, yaw) = matrix.euler_angles();
                        Vector3::new(roll, pitch, yaw)
                    } else {
                        Vector3::zeros()
                    };

                    let center = model.get_bounds_center();
                    model.set_rotation(&platform, rotation);
                    model.set_bounds_center(&platform, center);
                    model.align_to_bed();
                }
            }
        }
    }

    let painter = ui.painter();
    let color = match app.config.ui.theme {
        Theme::Dark => Color32::from_rgb(9, 9, 9),
        Theme::Light => Color32::from_rgb(255, 255, 255),
    };
    painter.rect_filled(rect, 0.0, color);

    // Tell the scene how big the viewport is, in physical pixels, then draw the image the bevy
    // camera renders into it. The size only takes effect next frame (the camera renders before the
    // dock layout is known), which is invisible for a docked tab.
    let ppp = ui.ctx().pixels_per_point();
    app.state.viewport.target_size = Vector2::new(
        (rect.width() * ppp).round() as u32,
        (rect.height() * ppp).round() as u32,
    );
    // Where this viewport sits on the window. The transform gizmo hit-tests through a camera whose
    // `viewport` is this rect, so it must be in window pixels (`render::scene::gizmo`).
    app.state.viewport.rect = ViewportRect {
        min: Vector2::new(
            (rect.min.x * ppp).round() as u32,
            (rect.min.y * ppp).round() as u32,
        ),
        size: Vector2::new(
            (rect.width() * ppp).round() as u32,
            (rect.height() * ppp).round() as u32,
        ),
        logical: rect,
    };
    app.state.viewport.painted = true;
    if let Some(texture) = app.state.viewport.texture_id {
        painter.image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    let toolbar = Area::new(Id::new("toolbar"))
        .fixed_pos(rect.min + Vec2::splat(10.0))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .inner_margin(Vec2::splat(8.0))
                .corner_radius(ui.visuals().menu_corner_radius)
                .fill(Color32::BLACK.lerp_to_gamma(Color32::TRANSPARENT, 0.25))
                .show(ui, |ui| {
                    for (i, tool) in Tool::ALL.into_iter().enumerate() {
                        let button = Button::new(RichText::new(tool.icon()).size(20.0))
                            .selected(tool == app.state.tool)
                            .min_size(Vec2::splat(30.0))
                            .ui(ui)
                            .on_hover_text(format!("{}\nShortcut: {}", tool.name(), i + 1));

                        let shortcut = ui.input(|x| x.key_pressed(NUMBER_KEYS[i + 1]));
                        (button.clicked() || shortcut).then(|| app.state.tool = tool);
                    }
                })
        });

    paint_gizmo_toolbar(app, ui, rect, toolbar.response.rect);

    paint_basis_vectors(painter, app, &rect);

    if app.config.ui.hover_overlay != HoverOverlay::Off
        && let Some(hover) = app.state.hovered_geometry
        && response.contains_pointer()
    {
        paint_hover_overlay(ui, app, hover, px);
    }
}

fn paint_basis_vectors(painter: &Painter, app: &mut App, rect: &Rect) {
    let size = app.config.render.basis_size;
    if size == 0.0 {
        return;
    }

    let pad = 4.0;
    let color = |r, g, b, a| Color32::from_rgba_unmultiplied(r, g, b, a);
    let (color_bg, color_edge) = match app.config.ui.theme {
        Theme::Dark => (color(255, 255, 255, 40), color(255, 255, 255, 180)),
        Theme::Light => (color(0, 0, 0, 40), color(0, 0, 0, 180)),
    };
    let stroke = Stroke::new(1.5_f32, color_edge);

    let rect = Rect::from_min_size(
        pos2(rect.max.x - size - pad, rect.min.y + pad),
        vec2(size, size),
    );
    painter.rect_filled(rect, size / 2.0, color_bg);
    painter.rect_stroke(rect, size / 2.0, stroke, StrokeKind::Outside);

    painter.add(EguiBevyPaintCallback::new_paint_callback(
        rect.expand(-pad),
        app.get_basis_render_callback(),
    ));
}

fn paint_hover_overlay(ui: &mut Ui, app: &mut App, hover: GeometryHit, px: Pos2) {
    let p = Vec2::splat(8.0);
    let detail = app.config.ui.hover_overlay == HoverOverlay::Detailed;

    Area::new(Id::new("hover_overlay"))
        .order(Order::Tooltip)
        .fixed_pos(px + vec2(p.x, -p.y))
        .pivot(Align2::LEFT_BOTTOM)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .inner_margin(p)
                .corner_radius(ui.visuals().menu_corner_radius)
                .fill(Color32::BLACK.lerp_to_gamma(Color32::TRANSPARENT, 0.25))
                .show(ui, |ui| {
                    ui.style_mut().visuals.override_text_color = Some(Color32::WHITE);
                    ui.set_max_width(300.0);

                    let Some(model) = app.project.model(hover.model) else {
                        return;
                    };

                    grid("overlay").spacing([8.0, 4.0]).show(ui, |ui| {
                        ui.label("Model");
                        ui.label(&model.name);
                        ui.end_row();

                        if hover.support
                            && let Some(support) = model.supports.support_for_face(hover.face)
                        {
                            ui.label("Support");
                            ui.label(format!("#{}", support.raw()));
                            ui.end_row();
                        }

                        if detail {
                            ui.label("Face");
                            let [a, b, c] = model.mesh.face(hover.face as usize);
                            ui.label(format!("{} ({a}, {b}, {c})", hover.face));
                            ui.end_row();

                            ui.label("Normal");
                            let normal = model.mesh.normal(hover.face as usize);
                            let transformed_normal = model.mesh.transform_normal(&normal);
                            ui.vertical(|ui| {
                                ui.label(format_normal(&normal));

                                if transformed_normal != normal {
                                    ui.horizontal(|ui| {
                                        ui.label(format_normal(&transformed_normal));
                                        ui.label("(Transformed)");
                                    });
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
        });
}

fn format_normal(normal: &Vector3<f32>) -> String {
    const SIGN: [&str; 2] = ["", "+"];
    format!(
        "‹{}{:.2}, {}{:.2}, {}{:.2}›",
        SIGN[(normal.x > 0.0) as usize],
        normal.x,
        SIGN[(normal.y > 0.0) as usize],
        normal.y,
        SIGN[(normal.z > 0.0) as usize],
        normal.z,
    )
}

impl App {
    pub fn get_basis_render_callback(&mut self) -> BasisRenderCallback {
        BasisRenderCallback {
            app: self as *mut App,
            camera: self.camera.clone(),
        }
    }
}

/// Draw the transform gizmo mode/orientation controls when a model is selected.
fn paint_gizmo_toolbar(app: &mut App, ui: &Ui, rect: Rect, toolbar: Rect) {
    if app.state.tool != Tool::Select || app.state.selected.single_model().is_none() {
        return;
    }

    // Sit beside the tool column, not over it: one button width plus the toolbar's padding on
    // both sides (`toolbar` already spans those) and one more padding of blank space.
    const GAP: f32 = 8.0;

    Area::new(Id::new("gizmo_toolbar"))
        .fixed_pos(pos2(toolbar.max.x + GAP, toolbar.min.y))
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            Frame::new()
                .inner_margin(Vec2::splat(6.0))
                .corner_radius(ui.visuals().menu_corner_radius)
                .fill(Color32::BLACK.lerp_to_gamma(Color32::TRANSPARENT, 0.25))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for mode in GizmoTransform::ALL {
                            let button = Button::new(RichText::new(mode.name()).size(14.0))
                                .selected(mode == app.state.gizmo_mode)
                                .ui(ui);
                            button.clicked().then(|| app.state.gizmo_mode = mode);
                        }

                        ui.separator();
                        ui.toggle_value(&mut app.state.gizmo_local, "Local");
                        ui.toggle_value(&mut app.state.gizmo_snap, "Snap");
                    });
                });
        });
}
