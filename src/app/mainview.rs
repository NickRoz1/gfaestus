use vulkano::command_buffer::{AutoCommandBuffer, AutoCommandBufferBuilder};
use vulkano::device::Queue;
use vulkano::framebuffer::{RenderPassAbstract, Subpass};
use vulkano::sync::GpuFuture;
use vulkano::{command_buffer::DynamicState, sampler::Sampler};

use crossbeam::atomic::AtomicCell;
use crossbeam::channel;

use std::sync::Arc;

use rgb::*;

use anyhow::Result;

use handlegraph::handle::NodeId;

use rustc_hash::FxHashSet;

use crate::geometry::*;
use crate::render::*;
use crate::view::{ScreenDims, View};
use vulkano::command_buffer::AutoCommandBufferBuilderContextError;

use crate::input::binds::*;
use crate::input::MousePos;

use super::{
    node_flags::{FlagUpdate, LayoutFlags, NodeFlag, NodeFlags},
    theme::{Theme, ThemeId},
};

#[derive(Debug, Default, Clone)]
pub struct NodeData {
    vertices: Vec<Vertex>,
    flags: LayoutFlags,
    // _updates: FxHashSet<NodeId>
}

pub struct MainView {
    node_draw_system: NodeDrawSystem,
    base_node_width: f32,
    node_data: NodeData,

    line_draw_system: LineDrawSystem,
    pub draw_grid: bool,

    view: Arc<AtomicCell<View>>,
    anim_handler_thread: AnimHandlerThread,
}

impl MainView {
    pub fn new<Rn, Rl>(
        // mouse_pos: MousePos,
        gfx_queue: Arc<Queue>,
        node_subpass: Subpass<Rn>,
        line_subpass: Subpass<Rl>,
    ) -> Result<MainView>
    where
        Rn: RenderPassAbstract + Send + Sync + Clone + 'static,
        Rl: RenderPassAbstract + Send + Sync + Clone + 'static,
    {
        let node_draw_system =
            NodeDrawSystem::new(gfx_queue.clone(), node_subpass);

        let line_draw_system =
            LineDrawSystem::new(gfx_queue.clone(), line_subpass);

        let draw_grid = false;

        let view = View::default();

        let anim_handler = AnimHandler::default();
        let view = Arc::new(AtomicCell::new(view));

        let base_node_width = 100.0;

        let anim_handler_thread =
            anim_handler_thread(anim_handler, view.clone());

        let main_view = Self {
            node_draw_system,
            base_node_width,
            node_data: Default::default(),

            line_draw_system,
            draw_grid,

            view,
            anim_handler_thread,
        };

        Ok(main_view)
    }

    pub fn prepare_themes(
        &self,
        sampler: &Arc<Sampler>,
        light: &Theme,
        dark: &Theme,
    ) -> Result<()> {
        self.node_draw_system.prepare_themes(sampler, light, dark)
    }

    pub fn view(&self) -> View {
        self.view.load()
    }

    pub fn set_initial_view(&self, center: Option<Point>, scale: Option<f32>) {
        let old_init_view = self.anim_handler_thread.initial_view.load();
        let center = center.unwrap_or(old_init_view.center);
        let scale = scale.unwrap_or(old_init_view.scale);
        self.anim_handler_thread
            .initial_view
            .store(View { center, scale });
    }

    pub fn reset_view(&self) {
        self.view
            .store(self.anim_handler_thread.initial_view.load());
    }

    pub fn set_vertices<VI>(&mut self, vertices: VI)
    where
        VI: IntoIterator<Item = Vertex>,
        VI::IntoIter: ExactSizeIterator,
    {
        self.node_data.vertices.clear();
        self.node_data.flags.clear();
        self.node_data.vertices.extend(vertices.into_iter());

        let node_count = self.node_data.vertices.len() / 2;
        self.node_draw_system
            .allocate_node_selection_buffer(node_count)
            .unwrap();
    }

    pub fn update_node_selection(
        &mut self,
        new_selection: &FxHashSet<NodeId>,
    ) -> Result<()> {
        let draw_sys = &self.node_draw_system;
        let flags = &mut self.node_data.flags;

        draw_sys.update_node_selection(|buffer| {
            let _result = flags.update_selection(new_selection, buffer)?;
            Ok(())
        })
    }

    pub fn clear_node_selection(&mut self) -> Result<()> {
        let draw_sys = &self.node_draw_system;
        let flags = &mut self.node_data.flags;

        flags.clear();

        draw_sys.update_node_selection(|buffer| {
            let _result = flags.clear_buffer(buffer)?;
            Ok(())
        })?;

        Ok(())
    }

    pub fn has_vertices(&self) -> bool {
        !self.node_data.vertices.is_empty()
    }

    pub fn draw_nodes_primary<'a>(
        &self,
        builder: &'a mut AutoCommandBufferBuilder,
        dynamic_state: &DynamicState,
        offset: Point,
        theme: ThemeId,
    ) -> Result<&'a mut AutoCommandBufferBuilder> {
        let view = self.view.load();
        let node_width = {
            let mut width = self.base_node_width;
            if view.scale > 100.0 {
                width *= view.scale / 100.0;
            }
            width
        };

        let vertices = if self.node_draw_system.has_cached_vertices() {
            None
        } else {
            Some(self.node_data.vertices.iter().copied())
        };

        self.node_draw_system.draw_primary(
            builder,
            dynamic_state,
            vertices,
            view,
            offset,
            node_width,
            theme,
        )
    }

    pub fn draw_nodes(
        &self,
        dynamic_state: &DynamicState,
        offset: Point,
        theme: ThemeId,
    ) -> Result<AutoCommandBuffer> {
        let view = self.view.load();
        let node_width = {
            let mut width = self.base_node_width;
            if view.scale > 100.0 {
                width *= view.scale / 100.0;
            }
            width
        };

        let vertices = if self.node_draw_system.has_cached_vertices() {
            None
        } else {
            Some(self.node_data.vertices.iter().copied())
        };

        self.node_draw_system.draw(
            dynamic_state,
            vertices,
            view,
            offset,
            node_width,
            // false,
            theme,
        )
    }

    pub fn draw_nodes_dynamic<VI>(
        &self,
        dynamic_state: &DynamicState,
        vertices: VI,
        offset: Point,
        theme: ThemeId,
    ) -> Result<AutoCommandBuffer>
    where
        VI: IntoIterator<Item = Vertex>,
        VI::IntoIter: ExactSizeIterator,
    {
        let view = self.view.load();
        let node_width = {
            let mut width = self.base_node_width;
            if view.scale > 100.0 {
                width *= view.scale / 100.0;
            }
            width
        };
        self.node_draw_system.draw(
            dynamic_state,
            Some(vertices),
            view,
            offset,
            node_width,
            theme, // false,
        )
    }

    pub fn read_node_id_at<Dims: Into<ScreenDims>>(
        &self,
        screen_dims: Dims,
        point: Point,
    ) -> Option<u32> {
        self.node_draw_system.read_node_id_at(screen_dims, point)
    }

    pub fn add_lines(
        &self,
        lines: &[(Point, Point)],
        color: RGB<f32>,
    ) -> Result<(usize, Box<dyn GpuFuture>)> {
        self.line_draw_system.add_lines(lines, color)
    }

    pub fn draw_lines(
        &self,
        dynamic_state: &DynamicState,
    ) -> Result<AutoCommandBuffer> {
        let view = self.view.load();
        self.line_draw_system.draw_stored(dynamic_state, view)
    }

    pub fn set_view_center(&self, center: Point) {
        let mut view = self.view.load();
        view.center = center;
        self.view.store(view);
    }

    pub fn set_view_scale(&self, scale: f32) {
        let mut view = self.view.load();
        view.scale = scale;
        self.view.store(view);
    }

    pub fn set_mouse_pos(&self, pos: Option<Point>) {
        self.anim_handler_thread.mouse_pos.store(pos);
    }

    pub fn set_mouse_pan(&self, origin: Option<Point>) {
        self.anim_handler_thread.set_mouse_pan(origin);
    }

    pub fn pan_const(&self, dx: Option<f32>, dy: Option<f32>) {
        self.anim_handler_thread.pan_const(dx, dy);
    }

    pub fn pan_delta(&self, dxy: Point) {
        self.anim_handler_thread.pan_delta(dxy);
    }

    pub fn zoom_delta(&self, dz: f32) {
        self.anim_handler_thread.zoom_delta(dz)
    }

    pub fn apply_input<Dims: Into<ScreenDims>>(
        &self,
        screen_dims: Dims,
        app_msg_tx: &channel::Sender<crate::app::AppMsg>,
        input: SystemInput<MainViewInputs>,
    ) {
        use MainViewInputs as In;
        let payload = input.payload();

        match input {
            SystemInput::Keyboard { state, .. } => {
                let pressed = state.pressed();

                let pan_delta = |invert: bool| {
                    let delta = if pressed { 1.0 } else { 0.0 };
                    if invert {
                        -delta
                    } else {
                        delta
                    }
                };

                match payload {
                    In::KeyPanUp => {
                        self.pan_const(None, Some(pan_delta(true)));
                    }
                    In::KeyPanRight => {
                        self.pan_const(Some(pan_delta(false)), None);
                    }
                    In::KeyPanDown => {
                        self.pan_const(None, Some(pan_delta(false)));
                    }
                    In::KeyPanLeft => {
                        self.pan_const(Some(pan_delta(true)), None);
                    }
                    In::KeyResetView => {
                        if pressed {
                            self.reset_view();
                        }
                    }
                    _ => (),
                }
            }
            SystemInput::MouseButton { pos, state, .. } => {
                let pressed = state.pressed();
                match payload {
                    In::ButtonMousePan => {
                        if pressed {
                            self.set_mouse_pan(Some(pos));
                        } else {
                            self.set_mouse_pan(None);
                        }
                    }
                    In::ButtonSelect => {
                        use crate::app::AppMsg;
                        use crate::app::Select;

                        let selected_node = self
                            .read_node_id_at(screen_dims, pos)
                            .map(|nid| NodeId::from(nid as u64));

                        if let Some(node) = selected_node {
                            app_msg_tx
                                .send(AppMsg::Selection(Select::One {
                                    node,
                                    clear: false,
                                }))
                                .unwrap();
                        }
                    }
                    _ => (),
                }
            }
            SystemInput::Wheel { delta, .. } => {
                if let In::WheelZoom = payload {
                    self.zoom_delta(delta);
                }
            }
        }
    }
}

pub enum DisplayLayer {
    Grid,
    Graph,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum AnimMsg {
    SetMousePan(Option<Point>),
    PanConst { dx: Option<f32>, dy: Option<f32> },
    PanDelta { dxy: Point },
    ZoomDelta { dz: f32 },
}

pub struct AnimHandlerThread {
    settings: Arc<AtomicCell<AnimSettings>>,
    initial_view: Arc<AtomicCell<View>>,
    mouse_pos: Arc<AtomicCell<Option<Point>>>,
    _join_handle: std::thread::JoinHandle<()>,
    msg_tx: channel::Sender<AnimMsg>,
}

impl AnimHandlerThread {
    #[allow(dead_code)]
    fn update_settings<F>(&self, f: F)
    where
        F: Fn(AnimSettings) -> AnimSettings,
    {
        let old = self.settings.load();
        let new = f(old);
        self.settings.store(new);
    }

    #[allow(dead_code)]
    fn anim_settings(&self) -> AnimSettings {
        self.settings.load()
    }

    fn set_mouse_pan(&self, origin: Option<Point>) {
        self.msg_tx.send(AnimMsg::SetMousePan(origin)).unwrap();
    }

    fn pan_const(&self, dx: Option<f32>, dy: Option<f32>) {
        let speed = self.settings.load().key_pan_speed;
        let dx = dx.map(|x| x * speed);
        let dy = dy.map(|x| x * speed);
        self.msg_tx.send(AnimMsg::PanConst { dx, dy }).unwrap();
    }

    fn pan_delta(&self, dxy: Point) {
        self.msg_tx.send(AnimMsg::PanDelta { dxy }).unwrap();
    }

    fn zoom_delta(&self, dz: f32) {
        self.msg_tx.send(AnimMsg::ZoomDelta { dz }).unwrap();
    }
}

fn anim_handler_thread(
    anim_handler: AnimHandler,
    view: Arc<AtomicCell<View>>,
) -> AnimHandlerThread {
    let mouse_pos = Arc::new(AtomicCell::new(None));

    let settings: Arc<AtomicCell<AnimSettings>> =
        Arc::new(AtomicCell::new(AnimSettings::default()));
    let initial_view = view.load();
    let initial_view = Arc::new(AtomicCell::new(initial_view));

    let inner_settings = settings.clone();
    let inner_view = view;
    let inner_mouse_pos = mouse_pos.clone();

    let (msg_tx, msg_rx) = channel::unbounded::<AnimMsg>();

    let _join_handle = std::thread::spawn(move || {
        let update_delay = std::time::Duration::from_millis(5);
        let sleep_delay = std::time::Duration::from_micros(2500);

        let settings = inner_settings;
        let view = inner_view;
        let mouse_pos = inner_mouse_pos;
        let mut anim = anim_handler;

        let mut last_update = std::time::Instant::now();

        loop {
            while let Ok(msg) = msg_rx.try_recv() {
                match msg {
                    AnimMsg::SetMousePan(origin) => match origin {
                        Some(origin) => anim.start_mouse_pan(origin),
                        None => anim.end_mouse_pan(),
                    },
                    AnimMsg::PanConst { dx, dy } => {
                        anim.pan_const(dx, dy);
                    }
                    AnimMsg::PanDelta { dxy } => {
                        let settings = settings.load();
                        anim.pan_delta(&settings, dxy);
                    }
                    AnimMsg::ZoomDelta { dz } => {
                        let view = view.load();
                        anim.zoom_delta(view.scale, dz)
                    }
                }
            }

            if last_update.elapsed() > update_delay {
                let dt = last_update.elapsed().as_secs_f32();
                let settings = settings.load();
                let pos = mouse_pos.load();
                anim.update_cell(&settings, &view, pos, dt);
                last_update = std::time::Instant::now();
            } else {
                std::thread::sleep(sleep_delay);
            }
        }
    });

    AnimHandlerThread {
        _join_handle,
        mouse_pos,
        msg_tx,
        settings,
        initial_view,
    }
}

#[derive(Debug, Default, Clone)]
pub struct AnimHandler {
    mouse_pan_screen_origin: Option<Point>,
    view_anim_target: Option<View>,
    view_pan_const: Point,
    view_pan_delta: Point,
    view_scale_delta: f32,
}

impl AnimHandler {
    fn update_cell(
        &mut self,
        settings: &AnimSettings,
        view: &AtomicCell<View>,
        mouse_pos: Option<Point>,
        dt: f32,
    ) {
        let before = view.load();
        let new = self.update(settings, before, mouse_pos, dt);
        view.store(new);
    }

    fn update(
        &mut self,
        settings: &AnimSettings,
        mut view: View,
        mouse_pos: Option<Point>,
        dt: f32,
    ) -> View {
        view.scale += view.scale * dt * self.view_scale_delta;

        if let Some(min_scale) = settings.min_view_scale {
            view.scale = view.scale.max(min_scale);
        }

        if let Some(max_scale) = settings.max_view_scale {
            view.scale = view.scale.min(max_scale);
        }

        let dxy = match (self.mouse_pan_screen_origin, mouse_pos) {
            (Some(origin), Some(mouse_pos)) => {
                (mouse_pos - origin) * settings.mouse_pan_mult * dt
            }
            _ => (self.view_pan_const + self.view_pan_delta) * dt,
        };

        view.center += dxy * view.scale;

        // let zoom_friction = 1.0 - (10.0_f32.powf(dt - 1.0));
        // let pan_friction = 1.0 - (10.0_f32.powf(dt - 1.0));

        let zoom_friction = if dt >= 1.0 {
            0.0
        } else {
            1.0 - (10.0_f32.powf(dt - 1.0))
        };
        let pan_friction = if dt >= 1.0 {
            0.0
        } else {
            1.0 - (10.0_f32.powf(dt - 1.0))
        };

        self.view_pan_delta *= pan_friction;
        self.view_scale_delta *= zoom_friction;

        if self.view_scale_delta.abs() < 0.00001 {
            self.view_scale_delta = 0.0;
        }

        view
    }

    fn start_mouse_pan(&mut self, origin: Point) {
        self.mouse_pan_screen_origin = Some(origin);
    }

    fn end_mouse_pan(&mut self) {
        self.mouse_pan_screen_origin = None;
    }

    /// If a direction is `None`, don't update the corresponding view delta const
    fn pan_const(&mut self, dx: Option<f32>, dy: Option<f32>) {
        // if a direction is set to zero, set the regular pan delta to
        // the old constant speed, and let the pan_friction in
        // update() smoothly bring it down
        if Some(0.0) == dx {
            self.view_pan_delta.x = self.view_pan_const.x;
        }

        if Some(0.0) == dy {
            self.view_pan_delta.y = self.view_pan_const.y;
        }

        let dxy = Point {
            x: dx.unwrap_or(self.view_pan_const.x),
            y: dy.unwrap_or(self.view_pan_const.y),
        };
        self.view_pan_const = dxy;
    }

    fn pan_delta(&mut self, settings: &AnimSettings, dxy: Point) {
        self.view_pan_delta += dxy;

        if let Some(max_speed) = settings.max_speed {
            let d = &mut self.view_pan_delta;
            d.x = d.x.clamp(-max_speed, max_speed);
            d.y = d.y.clamp(-max_speed, max_speed);
        }
    }

    fn zoom_delta(&mut self, current_scale: f32, dz: f32) {
        let delta_mult = current_scale.log2();
        let delta_mult = delta_mult.max(1.0);
        self.view_scale_delta += dz * delta_mult;
    }
}

#[derive(Debug, Clone, Copy)]
struct AnimSettings {
    min_view_scale: Option<f32>,
    max_view_scale: Option<f32>,
    max_speed: Option<f32>,
    key_pan_speed: f32,
    mouse_pan_mult: f32,
    wheel_zoom_base_speed: f32,
}

impl std::default::Default for AnimSettings {
    fn default() -> Self {
        Self {
            min_view_scale: Some(0.5),
            max_view_scale: None,
            max_speed: Some(600.0),
            key_pan_speed: 400.0,
            mouse_pan_mult: 1.0,
            wheel_zoom_base_speed: 0.45,
        }
    }
}
