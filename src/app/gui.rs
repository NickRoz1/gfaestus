use std::sync::Arc;

#[allow(unused_imports)]
use handlegraph::{
    handle::{Direction, Handle, NodeId},
    handlegraph::*,
    mutablehandlegraph::*,
    packed::*,
    pathhandlegraph::*,
};

use anyhow::Result;

use vulkano::{
    command_buffer::{AutoCommandBuffer, DynamicState},
    device::Queue,
    framebuffer::{RenderPassAbstract, Subpass},
    sync::GpuFuture,
};

use crossbeam::channel;

use crate::geometry::*;
use crate::render::GuiDrawSystem;
use crate::view::View;

use crate::input::binds::*;

pub struct GfaestusGui {
    ctx: egui::CtxRef,
    frame_input: FrameInput,
    enabled_ui_elements: EnabledUiElements,

    gui_draw_system: GuiDrawSystem,

    hover_node_id: Option<NodeId>,
    selected_node_id: Option<NodeId>,
    selected_node_info: Option<NodeInfo>,

    graph_stats: GraphStatsUi,
    view_info: ViewInfoUi,
    frame_rate_box: FrameRateBox,
}

#[derive(Debug, Default, Clone)]
struct FrameInput {
    events: Vec<egui::Event>,
    scroll_delta: f32,
}

impl FrameInput {
    fn into_raw_input(&mut self) -> egui::RawInput {
        let mut raw_input = egui::RawInput::default();
        raw_input.events = std::mem::take(&mut self.events);
        raw_input.scroll_delta = egui::Vec2 {
            x: 0.0,
            y: self.scroll_delta,
        };
        self.scroll_delta = 0.0;

        raw_input
    }
}

#[derive(Debug, Clone, Copy)]
struct EnabledUiElements {
    egui_inspection_ui: bool,
    egui_settings_ui: bool,
    egui_memory_ui: bool,
    frame_rate: bool,
    graph_stats: bool,
    view_info: bool,
    selected_node: bool,
}

impl std::default::Default for EnabledUiElements {
    fn default() -> Self {
        Self {
            egui_inspection_ui: false,
            egui_settings_ui: false,
            egui_memory_ui: false,
            frame_rate: true,
            graph_stats: true,
            view_info: true,
            selected_node: true,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct NodeInfo {
    node_id: NodeId,
    len: usize,
    degree: (usize, usize),
    coverage: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct FrameRateBox {
    fps: f32,
    frame_time: f32,
    frame: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct ViewInfoUi {
    position: Point,
    view: View,
    mouse_screen: Point,
    mouse_world: Point,
}

#[derive(Debug, Default, Clone, Copy)]
struct GraphStatsUi {
    position: Point,
    stats: GraphStats,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub path_count: usize,
    pub total_len: usize,
}

impl GfaestusGui {
    pub fn new<R>(
        gfx_queue: Arc<Queue>,
        render_pass: &Arc<R>,
    ) -> Result<GfaestusGui>
    where
        R: RenderPassAbstract + Send + Sync + 'static,
    {
        let gui_draw_system = GuiDrawSystem::new(
            gfx_queue,
            Subpass::from(render_pass.clone(), 0).unwrap(),
        );

        let ctx = egui::CtxRef::default();

        let mut style: egui::Style = (*ctx.style()).clone();
        style.visuals.window_corner_radius = 0.0;
        ctx.set_style(style);

        let font_defs = {
            use egui::FontFamily as Family;
            use egui::TextStyle as Style;

            let mut font_defs = egui::FontDefinitions::default();
            let fam_size = &mut font_defs.family_and_size;

            fam_size.insert(Style::Small, (Family::Proportional, 12.0));
            fam_size.insert(Style::Body, (Family::Proportional, 16.0));
            fam_size.insert(Style::Button, (Family::Proportional, 18.0));
            fam_size.insert(Style::Heading, (Family::Proportional, 22.0));
            font_defs
        };
        ctx.set_fonts(font_defs);

        let hover_node_id = None;
        let selected_node_id = None;

        let graph_stats = GraphStatsUi {
            position: Point { x: 12.0, y: 20.0 },
            ..GraphStatsUi::default()
        };

        let view_info = ViewInfoUi {
            position: Point { x: 12.0, y: 140.0 },
            ..ViewInfoUi::default()
        };

        let frame_rate_box = FrameRateBox {
            fps: 0.0,
            frame_time: 0.0,
            frame: 0,
        };

        Ok(Self {
            ctx,
            frame_input: FrameInput::default(),
            enabled_ui_elements: EnabledUiElements::default(),
            hover_node_id,
            selected_node_id,
            selected_node_info: None,
            gui_draw_system,
            graph_stats,
            view_info,
            frame_rate_box,
        })
    }

    pub fn set_frame_rate(&mut self, frame: usize, fps: f32, frame_time: f32) {
        self.frame_rate_box.frame = frame;
        self.frame_rate_box.fps = fps;
        self.frame_rate_box.frame_time = frame_time;
    }

    pub fn set_graph_stats(&mut self, stats: GraphStats) {
        self.graph_stats.stats = stats;
    }

    pub fn set_view_info_view(&mut self, view: View) {
        self.view_info.view = view;
    }

    pub fn set_view_info_mouse(
        &mut self,
        mouse_screen: Point,
        mouse_world: Point,
    ) {
        self.view_info.mouse_screen = mouse_screen;
        self.view_info.mouse_world = mouse_world;
    }

    pub fn set_hover_node(&mut self, node: Option<NodeId>) {
        self.hover_node_id = node;
    }

    pub fn set_selected_node(&mut self, node: Option<NodeId>) {
        self.selected_node_id = node;
        if node.is_none() {
            self.selected_node_info = None;
        }
    }

    pub fn selected_node(&self) -> Option<NodeId> {
        self.selected_node_id
    }

    pub fn selected_node_info_id(&self) -> Option<NodeId> {
        self.selected_node_info.map(|i| i.node_id)
    }

    pub fn set_selected_node_info(
        &mut self,
        node_id: NodeId,
        len: usize,
        degree: (usize, usize),
        coverage: usize,
    ) {
        self.selected_node_info = Some(NodeInfo {
            node_id,
            len,
            degree,
            coverage,
        });
    }

    fn graph_stats(&self, pos: Point) {
        let stats = self.graph_stats.stats;

        egui::Area::new("graph_summary_stats").fixed_pos(pos).show(
            &self.ctx,
            |ui| {
                ui.label(format!("nodes: {}", stats.node_count));
                ui.label(format!("edges: {}", stats.edge_count));
                ui.label(format!("paths: {}", stats.path_count));
                ui.label(format!("total length: {}", stats.total_len));
            },
        );
    }

    fn view_info(&self, pos: Point) {
        let info = self.view_info;

        egui::Area::new("view_mouse_info").fixed_pos(pos).show(
            &self.ctx,
            |ui| {
                ui.label(format!(
                    "view origin: x: {:6}\ty: {:6}",
                    info.view.center.x, info.view.center.y
                ));
                ui.label(format!("view scale: {}", info.view.scale));
                ui.label(format!(
                    "mouse world:  {:6}\t{:6}",
                    info.mouse_world.x, info.mouse_world.y
                ));
                ui.label(format!(
                    "mouse screen: {:6}\t{:6}",
                    info.mouse_screen.x, info.mouse_screen.y
                ));
            },
        );
    }

    pub fn begin_frame(&mut self, screen_rect: Option<Point>) {
        let mut raw_input = self.frame_input.into_raw_input();
        let screen_rect = screen_rect.map(|p| egui::Rect {
            min: Point::ZERO.into(),
            max: p.into(),
        });
        raw_input.screen_rect = screen_rect;

        self.ctx.begin_frame(raw_input);

        let scr = self.ctx.input().screen_rect();

        if let Some(node_id) = self.hover_node_id {
            egui::containers::popup::show_tooltip_text(
                &self.ctx,
                node_id.0.to_string(),
            )
        }

        if let Some(node_id) = self.selected_node_id {
            let top_left = Point {
                x: 0.0,
                y: 0.80 * scr.max.y,
            };
            let bottom_right = Point {
                x: 200.0,
                y: scr.max.y,
            };

            let rect = egui::Rect {
                min: top_left.into(),
                max: bottom_right.into(),
            };

            egui::Window::new("node_select_info")
                .fixed_rect(rect)
                .title_bar(false)
                .show(&self.ctx, |ui| {
                    ui.expand_to_include_rect(rect);
                    let label = format!("Selected node: {}", node_id.0);
                    ui.label(label);
                    if let Some(node_info) = self.selected_node_info {
                        let lb_len = format!("Length: {}", node_info.len);
                        let lb_deg = format!(
                            "Degree: ({}, {})",
                            node_info.degree.0, node_info.degree.1
                        );
                        let lb_cov =
                            format!("Coverage: {}", node_info.coverage);

                        ui.label(lb_len);
                        ui.label(lb_deg);
                        ui.label(lb_cov);
                    }
                });
        }

        if self.enabled_ui_elements.graph_stats {
            self.graph_stats(self.graph_stats.position);
        }

        if self.enabled_ui_elements.view_info {
            self.view_info(self.view_info.position);
        }

        if self.enabled_ui_elements.frame_rate {
            let p0 = Point {
                x: 0.8 * scr.max.x,
                y: 0.0,
            };

            let p1 = Point {
                x: scr.max.x,
                y: 80.0,
            };

            egui::Window::new("mouse_over_egui")
                .fixed_rect(egui::Rect {
                    min: p0.into(),
                    max: p1.into(),
                })
                .title_bar(false)
                .show(&self.ctx, |ui| {
                    ui.label(format!("FPS:   {:.2}", self.frame_rate_box.fps));
                    ui.label(format!(
                        "update time: {:.2}",
                        self.frame_rate_box.frame_time
                    ));
                });
        }

        if self.enabled_ui_elements.egui_inspection_ui {
            egui::Window::new("egui_inspection_ui_window")
                .show(&self.ctx, |ui| self.ctx.inspection_ui(ui));
        }

        if self.enabled_ui_elements.egui_settings_ui {
            egui::Window::new("egui_settings_ui_window")
                .show(&self.ctx, |ui| self.ctx.settings_ui(ui));
        }

        if self.enabled_ui_elements.egui_memory_ui {
            egui::Window::new("egui_memory_ui_window")
                .show(&self.ctx, |ui| self.ctx.memory_ui(ui));
        }
    }

    pub fn toggle_egui_inspection_ui(&mut self) {
        self.enabled_ui_elements.egui_inspection_ui =
            !self.enabled_ui_elements.egui_inspection_ui;
    }

    pub fn toggle_egui_settings_ui(&mut self) {
        self.enabled_ui_elements.egui_settings_ui =
            !self.enabled_ui_elements.egui_settings_ui;
    }

    pub fn toggle_egui_memory_ui(&mut self) {
        self.enabled_ui_elements.egui_memory_ui =
            !self.enabled_ui_elements.egui_memory_ui;
    }

    pub fn pointer_over_gui(&self) -> bool {
        self.ctx.is_pointer_over_area()
    }

    fn draw_tessellated(
        &mut self,
        dynamic_state: &DynamicState,
        clipped_meshes: &[egui::ClippedMesh],
    ) -> Result<(AutoCommandBuffer, Option<Box<dyn GpuFuture>>)> {
        let egui_tex = self.ctx.texture();
        let tex_future =
            self.gui_draw_system.upload_texture(&egui_tex).transpose()?;
        let cmd_buf = self
            .gui_draw_system
            .draw_egui_ctx(dynamic_state, clipped_meshes)?;

        Ok((cmd_buf, tex_future))
    }

    pub fn push_event(&mut self, event: egui::Event) {
        self.frame_input.events.push(event);
    }

    pub fn end_frame_and_draw(
        &mut self,
        dynamic_state: &DynamicState,
    ) -> Option<Result<(AutoCommandBuffer, Option<Box<dyn GpuFuture>>)>> {
        let (_output, shapes) = self.ctx.end_frame();
        let clipped_meshes = self.ctx.tessellate(shapes);

        if clipped_meshes.is_empty() {
            return None;
        }

        Some(self.draw_tessellated(dynamic_state, &clipped_meshes))
    }

    pub fn apply_app_msg(&mut self, app_msg: crate::app::AppMsg) {
        use crate::app::AppMsg;
        match app_msg {
            AppMsg::SelectNode(id) => {
                self.set_selected_node(id);
            }
            AppMsg::HoverNode(id) => {
                self.set_hover_node(id);
            }
        }
    }

    pub fn apply_input(
        &mut self,
        app_msg_tx: &channel::Sender<crate::app::AppMsg>,
        input: SystemInput<GuiInput>,
    ) {
        use GuiInput as In;
        let payload = input.payload();

        match input {
            SystemInput::Keyboard { state, .. } => {
                if state.pressed() {
                    match payload {
                        GuiInput::KeyClearSelection => {
                            app_msg_tx
                                .send(crate::app::AppMsg::SelectNode(None))
                                .unwrap();
                            // self.set_selected_node(None);
                        }
                        GuiInput::KeyEguiInspectionUi => {
                            self.toggle_egui_inspection_ui();
                        }
                        GuiInput::KeyEguiSettingsUi => {
                            self.toggle_egui_settings_ui();
                        }
                        GuiInput::KeyEguiMemoryUi => {
                            self.toggle_egui_memory_ui();
                        }
                        _ => (),
                    }
                }
            }
            SystemInput::MouseButton { pos, state, .. } => {
                let pressed = state.pressed();

                let button = match payload {
                    GuiInput::ButtonLeft => Some(egui::PointerButton::Primary),
                    GuiInput::ButtonRight => {
                        Some(egui::PointerButton::Secondary)
                    }

                    _ => None,
                };

                if let Some(button) = button {
                    let egui_event = egui::Event::PointerButton {
                        pos: pos.into(),
                        button,
                        pressed,
                        modifiers: Default::default(),
                    };

                    self.push_event(egui_event);
                }
            }
            SystemInput::Wheel { delta, .. } => {
                if let In::WheelScroll = payload {
                    self.frame_input.scroll_delta = delta;
                }
            }
        }
    }
}
