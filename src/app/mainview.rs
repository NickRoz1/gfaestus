use crossbeam::channel;
use crossbeam::{
    atomic::AtomicCell,
    channel::{Receiver, Sender},
};

use std::sync::Arc;

use rgb::*;

use anyhow::Result;

use handlegraph::handle::NodeId;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::app::{node_flags::SelectionBuffer, NodeWidth};
use crate::view::{ScreenDims, View};
use crate::{geometry::*, vulkan::render_pass::Framebuffers};

use crate::input::binds::{
    BindableInput, InputPayload, KeyBind, MouseButtonBind, SystemInput, SystemInputBindings,
    WheelBind,
};
use crate::input::MousePos;

use crate::vulkan::{
    context::VkContext,
    draw_system::nodes::{NodeIdBuffer, NodePipelines, NodeThemePipeline, NodeVertices},
    GfaestusVk, SwapchainProperties,
};

use ash::vk;

pub mod view;

use view::*;

pub struct MainView {
    pub node_draw_system: crate::vulkan::draw_system::nodes::NodePipelines,
    pub node_id_buffer: NodeIdBuffer,
    pub selection_buffer: SelectionBuffer,

    node_width: Arc<NodeWidth>,

    view: Arc<AtomicCell<View>>,
    anim_handler: AnimHandler,

    view_input_state: ViewInputState,

    msg_tx: Sender<MainViewMsg>,
    msg_rx: Receiver<MainViewMsg>,
}

#[derive(Debug, Clone, Copy)]
pub enum MainViewMsg {
    GotoView(View),
}

impl MainView {
    pub fn new(
        app: &GfaestusVk,
        node_width: Arc<NodeWidth>,
        node_count: usize,
        swapchain_props: SwapchainProperties,
        msaa_samples: vk::SampleCountFlags,
        render_pass: vk::RenderPass,
    ) -> Result<Self> {
        let selection_buffer = SelectionBuffer::new(app, node_count)?;

        let node_draw_system = NodePipelines::new(
            app,
            swapchain_props,
            msaa_samples,
            render_pass,
            selection_buffer.buffer,
        )?;

        let view = View::default();

        let view = Arc::new(AtomicCell::new(view));

        let screen_dims = {
            let extent = swapchain_props.extent;
            ScreenDims {
                width: extent.width as f32,
                height: extent.height as f32,
            }
        };

        let node_id_buffer =
            NodeIdBuffer::new(&app, screen_dims.width as u32, screen_dims.height as u32)?;

        let anim_handler = AnimHandler::new(view.clone(), Point::ZERO, screen_dims);

        let (msg_tx, msg_rx) = channel::unbounded::<MainViewMsg>();

        let main_view = Self {
            node_draw_system,
            node_id_buffer,
            selection_buffer,

            node_width,

            view,
            anim_handler,

            view_input_state: Default::default(),

            msg_tx,
            msg_rx,
        };

        Ok(main_view)
    }

    pub fn main_view_msg_tx(&self) -> &Sender<MainViewMsg> {
        &self.msg_tx
    }

    pub fn main_view_msg_rx(&self) -> &Receiver<MainViewMsg> {
        &self.msg_rx
    }

    pub fn apply_msg(&self, msg: MainViewMsg) {
        match msg {
            MainViewMsg::GotoView(view) => {
                use std::time::Duration;

                let anim_def = AnimationDef {
                    kind: AnimationKind::Absolute,
                    order: AnimationOrder::Transform {
                        center: view.center,
                        scale: view.scale,
                    },
                    duration: Duration::from_millis(500),
                };
                self.anim_handler.send_anim_def(anim_def);
            }
        }
    }

    pub fn view(&self) -> View {
        self.view.load()
    }

    pub fn set_initial_view(&self, center: Option<Point>, scale: Option<f32>) {
        let old_init_view = self.anim_handler.initial_view.load();
        let center = center.unwrap_or(old_init_view.center);
        let scale = scale.unwrap_or(old_init_view.scale);
        self.anim_handler.initial_view.store(View { center, scale });
    }

    pub fn reset_view(&self) {
        self.view.store(self.anim_handler.initial_view.load());
    }

    pub fn set_view(&self, view: View) {
        self.view.store(view);
    }

    pub fn node_id_buffer(&self) -> vk::Buffer {
        self.node_id_buffer.buffer
    }

    pub fn recreate_node_id_buffer(
        &mut self,
        app: &GfaestusVk,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.node_id_buffer.recreate(app, width, height)
    }

    pub fn read_node_id_at(&self, point: Point) -> Option<u32> {
        let x = point.x as u32;
        let y = point.y as u32;

        self.node_id_buffer
            .read(self.node_draw_system.device(), x, y)
    }

    pub fn draw_nodes(
        &self,
        cmd_buf: vk::CommandBuffer,
        render_pass: vk::RenderPass,
        framebuffers: &Framebuffers,
        screen_dims: [f32; 2],
        offset: Point,
        use_overlay: bool,
    ) -> Result<()> {
        let view = self.view.load();

        let node_width = {
            let mut width = self.node_width.base_node_width();
            let upscale_limit = self.node_width.upscale_limit();
            let upscale_factor = self.node_width.upscale_factor();

            if view.scale > upscale_limit {
                width *= view.scale / upscale_factor;
            }
            width
        };

        let has_overlay = self.node_draw_system.has_overlay();

        if use_overlay && has_overlay {
            self.node_draw_system.draw_overlay(
                cmd_buf,
                render_pass,
                framebuffers,
                screen_dims,
                node_width,
                view,
                offset,
            )
        } else {
            self.node_draw_system.draw_themed(
                cmd_buf,
                render_pass,
                framebuffers,
                screen_dims,
                node_width,
                view,
                offset,
            )
        }
    }

    pub fn update_node_selection(&mut self, new_selection: &FxHashSet<NodeId>) -> Result<()> {
        let device = self.node_draw_system.device();
        let selection = &mut self.selection_buffer;

        selection.update_selection(device, new_selection)
    }

    pub fn clear_node_selection(&mut self) -> Result<()> {
        let device = self.node_draw_system.device();
        let selection = &mut self.selection_buffer;

        selection.clear();
        selection.clear_buffer(device)
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

    pub fn update_view_animation<D: Into<ScreenDims>>(&self, screen_dims: D, mouse_pos: Point) {
        let screen_dims = screen_dims.into();
        let view = self.view.load();

        let mouse_screen = mouse_pos;
        let mouse_world = view.screen_point_to_world(screen_dims, mouse_screen);

        if let Some(anim_def) =
            self.view_input_state
                .animation_def(view, screen_dims, mouse_screen, mouse_world)
        {
            self.anim_handler.send_anim_def(anim_def);
        }
    }

    pub fn apply_input<Dims: Into<ScreenDims>>(
        &self,
        screen_dims: Dims,
        mouse_pos: Point,
        app_msg_tx: &channel::Sender<crate::app::AppMsg>,
        input: SystemInput<MainViewInput>,
    ) {
        use MainViewInput as In;
        let payload = input.payload();

        match input {
            SystemInput::Keyboard { state, .. } => {
                let pressed = state.pressed();

                match payload {
                    In::KeyPanUp => {
                        self.view_input_state.key_pan.set_up(pressed);
                    }
                    In::KeyPanRight => {
                        self.view_input_state.key_pan.set_right(pressed);
                    }
                    In::KeyPanDown => {
                        self.view_input_state.key_pan.set_down(pressed);
                    }
                    In::KeyPanLeft => {
                        self.view_input_state.key_pan.set_left(pressed);
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
                            let view = self.view.load();
                            let mouse_world = view.screen_point_to_world(screen_dims, mouse_pos);

                            self.view_input_state
                                .start_click_and_drag_pan(view, mouse_world);
                        } else {
                            self.view_input_state.mouse_released();
                        }
                    }
                    In::ButtonSelect => {
                        use crate::app::AppMsg;
                        use crate::app::Select;

                        let selected_node = self
                            .read_node_id_at(pos)
                            .map(|nid| NodeId::from(nid as u64));

                        if let Some(node) = selected_node {
                            app_msg_tx
                                .send(AppMsg::Selection(Select::One { node, clear: false }))
                                .unwrap();
                        }
                    }
                    _ => (),
                }
            }
            SystemInput::Wheel { delta, .. } => {
                if let In::WheelZoom = payload {
                    self.view_input_state
                        .scroll_zoom(self.view.load(), mouse_pos, delta);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MainViewInput {
    ButtonMousePan,
    ButtonSelect,
    KeyPanUp,
    KeyPanRight,
    KeyPanDown,
    KeyPanLeft,
    KeyResetView,
    WheelZoom,
}

impl BindableInput for MainViewInput {
    fn default_binds() -> SystemInputBindings<Self> {
        use winit::event;
        use winit::event::VirtualKeyCode as Key;
        use MainViewInput as Input;

        let key_binds: FxHashMap<Key, Vec<KeyBind<Input>>> = [
            (Key::Up, Input::KeyPanUp),
            (Key::Down, Input::KeyPanDown),
            (Key::Left, Input::KeyPanLeft),
            (Key::Right, Input::KeyPanRight),
            (Key::Space, Input::KeyResetView),
        ]
        .iter()
        .copied()
        .map(|(k, i)| (k, vec![KeyBind::new(i)]))
        .collect::<FxHashMap<_, _>>();

        let mouse_binds: FxHashMap<event::MouseButton, Vec<MouseButtonBind<Input>>> = [(
            event::MouseButton::Left,
            vec![
                MouseButtonBind::new(Input::ButtonMousePan),
                MouseButtonBind::new(Input::ButtonSelect),
            ],
        )]
        .iter()
        .cloned()
        .collect();

        let wheel_bind = Some(WheelBind::new(true, 0.45, Input::WheelZoom));

        SystemInputBindings::new(key_binds, mouse_binds, wheel_bind)
    }
}
