use std::{collections::HashMap, path::PathBuf, pin::Pin, sync::Arc};

use futures::{future::RemoteHandle, Future, StreamExt};
#[allow(unused_imports)]
use handlegraph::{
    handle::{Direction, Handle, NodeId},
    handlegraph::*,
    mutablehandlegraph::*,
    packed::*,
    pathhandlegraph::*,
};

use anyhow::Result;

use log::debug;

use crossbeam::atomic::AtomicCell;

use rhai::plugin::*;
use rustc_hash::FxHashSet;

use bstr::ByteSlice;

use crate::{
    annotations::{
        AnnotationCollection, AnnotationRecord, Annotations, BedColumn,
        BedRecord, BedRecords, ColumnKey, Gff3Column, Gff3Record, Gff3Records,
    },
    overlays::OverlayKind,
    reactor::{ModalError, ModalHandler, ModalSuccess},
};
use crate::{
    app::{
        selection::NodeSelection, AppChannels, AppMsg, OverlayCreatorMsg,
        Select,
    },
    geometry::*,
    quad_tree::*,
    reactor::Reactor,
    script::{overlay_colors_tgt_ast, ScriptConfig, ScriptTarget},
    view::View,
};
use crate::{
    app::{AppSettings, SharedState},
    graph_query::GraphQuery,
};

use parking_lot::Mutex;

pub type ScriptEvalResult =
    std::result::Result<rhai::Dynamic, Box<rhai::EvalAltResult>>;

/// The main console that is available in the GUI, and directly
/// interacted with by the user.
///
/// Holds both GUI-related state, and all of the state the Rhai API requires.
pub struct Console<'a> {
    input_line: String,

    input_history_ix: Option<usize>,

    input_history: Vec<String>,
    output_offset: usize,
    output_history: Vec<String>,

    scope: Arc<Mutex<rhai::Scope<'a>>>,

    request_focus: bool,

    settings: AppSettings,
    shared_state: SharedState,
    channels: AppChannels,

    get_set: Arc<GetSetTruth>,

    remote_handles: HashMap<String, RemoteHandle<()>>,

    result_rx: crossbeam::channel::Receiver<ScriptEvalResult>,
    result_tx: crossbeam::channel::Sender<ScriptEvalResult>,

    graph: Arc<GraphQuery>,
    modules: Arc<Mutex<Vec<Arc<rhai::Module>>>>,

    key_code_map: Arc<HashMap<String, winit::event::VirtualKeyCode>>,
    overlay_list: Arc<Mutex<Vec<(usize, OverlayKind, String)>>>,

    rayon_pool: Arc<rayon::ThreadPool>,

    // TODO this shouldn't be a Vec, and it should probably use an
    // RwLock or something inside
    window_defs: Arc<Mutex<Vec<ConsoleGuiDsl>>>,

    future_tx: crossbeam::channel::Sender<
        Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>,
    >,
}

/// A "subconsole", spawned from one of the console commands (such as
/// keybinds).
///
/// It's more limited than the main console, in that it cannot create
/// new modules (and thus functions), yet.
#[allow(dead_code)]
pub struct ConsoleShared {
    settings: AppSettings,
    shared_state: SharedState,
    channels: AppChannels,
    get_set: Arc<GetSetTruth>,
    key_code_map: Arc<HashMap<String, winit::event::VirtualKeyCode>>,

    graph: Arc<GraphQuery>,

    overlay_list: Arc<Mutex<Vec<(usize, OverlayKind, String)>>>,

    // is this a bad idea? i should probably just use a global pool
    rayon_pool: Arc<rayon::ThreadPool>,

    result_tx: crossbeam::channel::Sender<ScriptEvalResult>,

    future_tx: crossbeam::channel::Sender<
        Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>,
    >,
}

impl Console<'static> {
    pub const ID: &'static str = "quake_console";
    pub const ID_TEXT: &'static str = "quake_console_input";

    pub fn new(
        reactor: &Reactor,
        graph: &Arc<GraphQuery>,
        channels: AppChannels,
        settings: AppSettings,
        shared_state: SharedState,
    ) -> Self {
        let (result_tx, result_rx) =
            crossbeam::channel::unbounded::<ScriptEvalResult>();

        let future_tx = reactor.future_tx.clone();

        let rayon_pool = reactor.rayon_pool.clone();

        // These macros add to the keys available with the `get` and `set` console functions
        let mut get_set = GetSetTruth::default();

        macro_rules! add_t {
            ($type:ty, $name:literal, $arc:expr) => {
                get_set.add_arc_atomic_cell_get_set(
                    $name,
                    $arc,
                    |x| rhai::Dynamic::from(x),
                    |x: rhai::Dynamic| x.try_cast::<$type>(),
                );
            };
        }

        /*
        macro_rules! add_nested_t {
            ($into:expr, $from:expr, $ubo:expr, $name:tt, $field:tt) => {
                get_set.add_arc_atomic_cell_get_set($name, $ubo, $into, $from);
            };
        }
        */

        macro_rules! add_nested_cast {
            ($ubo:expr, $field:tt, $type:ty) => {{
                let name = stringify!($field);

                get_set.add_arc_atomic_cell_get_set(
                    name,
                    $ubo,
                    move |cont| rhai::Dynamic::from(cont.$field),
                    {
                        let ubo = $ubo.clone();
                        move |val: rhai::Dynamic| {
                            let x = val.try_cast::<$type>()?;
                            let mut ubo = ubo.load();
                            ubo.$field = x;
                            Some(ubo)
                        }
                    },
                );
            }};
        }

        macro_rules! add_nested_cell {
            ($obj:expr, $get:tt, $set:tt) => {
                let nw = $obj.clone();
                let nw_ = $obj.clone();

                get_set.add_dynamic(
                    stringify!($get),
                    move || nw.$get(),
                    move |v| {
                        nw_.$set(v);
                    },
                )
            };
        }

        add_t!(f32, "label_radius", settings.label_radius().clone());
        add_t!(Point, "mouse_pos", shared_state.mouse_pos.clone());

        add_t!(
            rgb::RGB<f32>,
            "background_color_light",
            settings.background_color_light().clone()
        );
        add_t!(
            rgb::RGB<f32>,
            "background_color_dark",
            settings.background_color_dark().clone()
        );

        let edge = settings.edge_renderer().clone();

        add_nested_cast!(edge.clone(), edge_color, rgb::RGB<f32>);
        add_nested_cast!(edge.clone(), edge_width, f32);
        add_nested_cast!(edge.clone(), curve_offset, f32);

        let e1 = edge.clone();
        let e2 = edge.clone();

        get_set.add_dynamic(
            "tess_levels",
            move || {
                let tl = e1.load().tess_levels;
                let get = |ix| rhai::Dynamic::from(tl[ix]);
                vec![get(0), get(1), get(2), get(3), get(4)]
            },
            move |tess_vec: Vec<rhai::Dynamic>| {
                let get = |ix| {
                    tess_vec
                        .get(ix)
                        .cloned()
                        .and_then(|v: rhai::Dynamic| v.try_cast())
                        .unwrap_or(0.0f32)
                };
                let arr = [get(0), get(1), get(2), get(3), get(4)];
                let mut ubo = e2.load();
                ubo.tess_levels = arr;
                e2.store(ubo);
            },
        );

        add_nested_cell!(
            settings.node_width().clone(),
            min_node_width,
            set_min_node_width
        );
        add_nested_cell!(
            settings.node_width().clone(),
            max_node_width,
            set_max_node_width
        );
        add_nested_cell!(
            settings.node_width().clone(),
            min_node_scale,
            set_min_node_scale
        );
        add_nested_cell!(
            settings.node_width().clone(),
            max_node_scale,
            set_max_node_scale
        );

        let scope = Self::create_scope();
        let scope = Arc::new(Mutex::new(scope));

        let output_history =
            vec![" < close this console with Esc >".to_string()];

        let key_code_map = Arc::new(virtual_key_code_map());

        let overlay_list = Arc::new(Mutex::new(Vec::new()));

        /*
        let mut window_test =
            ConsoleGuiDsl::new("test window", egui::Id::new("window dsl test"));
        window_test.elements.push(ConsoleGuiElem::Label {
            text: "hello world".to_string(),
        });
        window_test.elements.push(ConsoleGuiElem::Button {
            text: "im a button".to_string(),
            callback_id: "button_callback".to_string(),
        });

        let callback = || {
            println!("button clicked!");
        };

        window_test
            .callbacks
            .insert("button_callback".to_string(), Box::new(callback) as _);
        let window_defs = Arc::new(Mutex::new(vec![window_test]));
        */

        let window_defs = Arc::new(Mutex::new(vec![]));

        Self {
            input_line: String::new(),

            input_history_ix: None,

            input_history: Vec::new(),
            output_offset: 0,
            output_history,

            scope,

            request_focus: false,

            channels,
            settings,
            shared_state,

            get_set: Arc::new(get_set),

            remote_handles: Default::default(),

            result_tx,
            result_rx,

            graph: graph.clone(),
            // graph: graph.graph.clone(),
            // path_positions: graph.path_positions.clone(),
            modules: Arc::new(Mutex::new(Vec::new())),

            key_code_map,

            overlay_list,
            rayon_pool,

            window_defs,

            future_tx,
        }
    }

    /// Create a subconsole that shares state with the main console
    /// where applicable
    pub fn shared(&self) -> ConsoleShared {
        ConsoleShared {
            settings: self.settings.clone(),
            shared_state: self.shared_state.clone(),
            channels: self.channels.clone(),
            get_set: self.get_set.clone(),
            key_code_map: self.key_code_map.clone(),

            graph: self.graph.clone(),
            // path_positions: self.path_positions.clone(),
            result_tx: self.result_tx.clone(),

            overlay_list: self.overlay_list.clone(),
            rayon_pool: self.rayon_pool.clone(),

            future_tx: self.future_tx.clone(),
        }
    }

    pub fn append_output(&mut self, output: &str) {
        self.output_history.extend(output.lines().map(String::from));
    }

    // NB: this shouldn't be handled this way (it shouldn't be a
    // function called from main), but works for now
    pub fn populate_overlay_list(
        &mut self,
        names: &[(usize, OverlayKind, &str)],
    ) {
        let mut overlays = self.overlay_list.lock();
        overlays.clear();
        overlays.extend(names.iter().map(|&(a, b, s)| (a, b, s.to_string())));
    }

    fn create_scope() -> rhai::Scope<'static> {
        let scope = rhai::Scope::new();
        scope
    }

    /// Creates the Rhai engine, adding all types, modules, and
    /// functions available in the console, and special features such
    /// as binding keys.
    ///
    /// See [`ConsoleShared::create_engine`] for the bulk of the
    /// features.
    pub fn create_engine(&self) -> rhai::Engine {
        let shared = self.shared();

        let modules = self.modules.clone();

        let key_code_map = self.key_code_map.clone();
        let binds_tx = self.channels.binds_tx.clone();

        let mut engine = shared.create_engine();

        // Bind a Rhai function to execute when the given key is
        // pressed. See the virtual_key_code_map() function below for
        // which keys are available.
        //
        // `fn_name` must be the name of a function that is part of
        // the console API, or is in a module that has been imported
        // using the `:import <src>` console command (for now)
        //
        // the same applies to the other functions here that take a
        // function name as parameter
        engine.register_fn(
            "bind_key",
            move |key: &str, fn_name: rhai::Dynamic| {
                log::debug!("in bind_key");

                let key_code = if let Some(map) = key_code_map.get(key) {
                    map
                } else {
                    return;
                };

                if let Some(fn_name) = fn_name.try_cast::<String>() {
                    let scope = Self::create_scope();

                    let mut engine = shared.create_engine();
                    {
                        let modules = modules.lock();
                        for module in modules.iter() {
                            engine.register_global_module(module.clone());
                        }
                    }

                    log::debug!("compiling to AST");
                    let script =
                        format!("fn a_function() {{\n{}();\n}}", fn_name);

                    let ast = engine.compile_with_scope(&scope, &script);

                    match ast {
                        Ok(ast) => {
                            let function =
                                rhai::Func::<(), ()>::create_from_ast(
                                    engine,
                                    ast,
                                    "a_function",
                                );

                            binds_tx
                                .send((
                                    *key_code,
                                    Some(Box::new(move || match function() {
                                        Ok(_) => (),
                                        Err(err) => log::warn!(
                                            "bound function error: {:?}",
                                            err
                                        ),
                                    })),
                                ))
                                .unwrap();
                        }
                        Err(err) => {
                            log::warn!("compilation error: {:?}", err);
                        }
                    }
                }
            },
        );

        let rayon_pool = self.rayon_pool.clone();
        let graph = self.graph.clone();
        let config = ScriptConfig {
            default_color: rgb::RGBA::new(0.3, 0.3, 0.3, 0.3),
            target: ScriptTarget::Nodes,
        };

        let overlay_tx = self.channels.new_overlay_tx.clone();
        let shared = self.shared();
        let modules = self.modules.clone();
        engine.register_fn(
            "create_overlay_from_fn",
            move |name: &str, fn_name: rhai::Dynamic| {
                if let Some(fn_name) = fn_name.try_cast::<String>() {
                    let mut scope = Self::create_scope();

                    scope
                        .push("graph", graph.graph.clone())
                        .push("path_pos", graph.path_positions.clone());

                    let mut engine = shared.create_engine();
                    {
                        let modules = modules.lock();
                        for module in modules.iter() {
                            engine.register_global_module(module.clone());
                        }
                    }

                    let script =
                        format!("\nfn node_color(i) {{\n{}(i);\n}}", fn_name);
                    log::debug!("script: {}", script);

                    let node_color_ast =
                        engine.compile_into_self_contained(&scope, &script);

                    match node_color_ast {
                        Ok(node_color_ast) => {
                            let result = overlay_colors_tgt_ast(
                                &rayon_pool,
                                &config,
                                &graph,
                                &engine,
                                scope,
                                node_color_ast,
                            );

                            match result {
                                Ok(data) => {
                                    let msg = OverlayCreatorMsg::NewOverlay {
                                        name: name.to_string(),
                                        data,
                                    };
                                    overlay_tx.send(msg).unwrap();
                                    log::info!("overlay data success");
                                }
                                Err(_err) => {
                                    log::warn!("overlay failure");
                                }
                            }
                        }
                        Err(_err) => {
                            log::warn!("ast failure");
                        }
                    }
                }
            },
        );

        self.add_gui_dsl_fns(&mut engine);

        {
            let modules = self.modules.lock();

            for module in modules.iter() {
                engine.register_global_module(module.clone());
            }
        }

        engine
    }

    fn add_gui_dsl_fns(&self, engine: &mut rhai::Engine) {
        // create a new window with the provided title, and return the index of the window
        //
        // NB: there's no way yet to remove or hide windows created here
        let window_defs = self.window_defs.clone();
        engine.register_fn("new_window", move |title: &str| {
            let mut win_defs = window_defs.lock();

            let ix = win_defs.len();
            let window = ConsoleGuiDsl::new(
                title,
                egui::Id::new(format!("{}-win_def_dsl-{}", title, ix,)),
            );

            win_defs.push(window);

            ix as i64
        });

        // add a label to the window with the provided index
        let window_defs = self.window_defs.clone();
        engine.register_fn("add_label", move |ix: i64, text: &str| {
            let mut win_defs = window_defs.lock();

            if let Some(window) = win_defs.get_mut(ix as usize) {
                window.elements.push(ConsoleGuiElem::Label {
                    text: text.to_string(),
                });
            }
        });

        let window_defs = self.window_defs.clone();
        engine.register_fn(
            "add_button",
            move |ix: i64, text: &str, callback_id: &str| {
                let mut win_defs = window_defs.lock();

                if let Some(window) = win_defs.get_mut(ix as usize) {
                    window.elements.push(ConsoleGuiElem::Button {
                        text: text.to_string(),
                        callback_id: callback_id.to_string(),
                    });
                }
            },
        );

        let window_defs = self.window_defs.clone();
        engine.register_fn("add_text_edit", move |ix: i64, data_id: &str| {
            let mut win_defs = window_defs.lock();

            if let Some(window) = win_defs.get_mut(ix as usize) {
                window.elements.push(ConsoleGuiElem::TextInput {
                    label: "".to_string(),
                    data_id: data_id.to_string(),
                });

                window.text_data.insert(data_id.to_string(), "".to_string());
            }
        });

        let window_defs = self.window_defs.clone();
        engine.register_result_fn(
            "text_edit_value",
            move |ix: i64, data_id: &str| {
                let mut win_defs = window_defs.lock();

                if let Some(window) = win_defs.get_mut(ix as usize) {
                    if let Some(contents) = window.get_text_data(data_id) {
                        return Ok(rhai::Dynamic::from(contents.to_string()));
                    }
                }

                Err("Text box does not exist".into())
            },
        );

        // `fn_name` here has the same limitations as seen in create_engine above
        let window_defs = self.window_defs.clone();
        let shared = self.shared();
        let modules = self.modules.clone();
        engine.register_fn(
            "add_callback",
            move |ix: i64, callback_id: &str, fn_name: &str| {
                let mut win_defs = window_defs.lock();

                if let Some(window) = win_defs.get_mut(ix as usize) {
                    let scope = Self::create_scope();
                    let mut engine = shared.create_engine();
                    {
                        let modules = modules.lock();
                        for module in modules.iter() {
                            engine.register_global_module(module.clone());
                        }
                    }

                    let script =
                        format!("fn a_function() {{\n{}();\n}}", fn_name);
                    let ast = engine.compile_with_scope(&scope, &script);

                    match ast {
                        Ok(ast) => {
                            let function =
                                rhai::Func::<(), ()>::create_from_ast(
                                    engine,
                                    ast,
                                    "a_function",
                                );

                            let callback = Box::new(move || match function() {
                                Ok(_) => (),
                                Err(err) => log::warn!(
                                    "gui dsl callback error: {:?}",
                                    err
                                ),
                            }) as _;

                            window
                                .callbacks
                                .insert(callback_id.to_string(), callback);
                        }
                        Err(err) => {
                            log::warn!("compilation error: {:?}", err);
                        }
                    }
                }
            },
        );
    }

    pub fn eval_input(
        &mut self,
        reactor: &mut Reactor,
        print: bool,
    ) -> Result<()> {
        debug!("evaluating: {}", &self.input_line);

        let input = self.input_line.to_owned();
        let executed_command = self.exec_console_command(reactor, &input)?;
        if executed_command {
            self.input_line.clear();
            return Ok(());
        }
        self.eval(reactor, print)?;

        Ok(())
    }

    pub fn eval_file(
        &mut self,
        reactor: &mut Reactor,
        print: bool,
        path: &str,
    ) -> Result<()> {
        use std::io::prelude::*;
        let mut file = std::fs::File::open(path)?;
        let mut script = String::new();
        let _count = file.read_to_string(&mut script)?;

        if print {
            self.output_history
                .push(format!(">>> Evaluating file '{}'", path));
        }

        self.eval_line(reactor, print, &script)
    }

    pub fn eval_line(
        &mut self,
        reactor: &mut Reactor,
        print: bool,
        input_line: &str,
    ) -> Result<()> {
        let mut old_input = input_line.to_string();
        std::mem::swap(&mut old_input, &mut self.input_line);

        self.eval(reactor, print)?;
        std::mem::swap(&mut old_input, &mut self.input_line);

        Ok(())
    }

    fn eval_file_interval(
        &mut self,
        reactor: &mut Reactor,
        handle_name: &str,
        path: &str,
    ) -> Result<()> {
        let handle_name = handle_name.to_string();

        let engine = self.create_engine();

        let start = std::time::Instant::now();

        let path = PathBuf::from(path);
        let ast = engine.compile_file(path)?;

        let mut scope = {
            let scope_lock = self.scope.lock();
            let scope = scope_lock.to_owned();
            scope
        };

        let handle = reactor.spawn_interval(
            move || {
                scope.set_value(
                    "time_since_start",
                    start.elapsed().as_secs_f32(),
                );

                let _result: std::result::Result<(), _> =
                    engine.eval_ast_with_scope(&mut scope, &ast);
            },
            std::time::Duration::from_millis(30),
        )?;

        self.remote_handles.insert(handle_name, handle);

        Ok(())
    }

    fn stop_interval(&mut self, handle_name: &str) {
        self.remote_handles.remove(handle_name);
    }

    // NB: edit this to add new console commands that do *not* use the Rhai engine
    fn exec_console_command(
        &mut self,
        reactor: &mut Reactor,
        input: &str,
    ) -> Result<bool> {
        if input.starts_with(":clear") {
            // Clears the output history visible in the console GUI

            self.output_history.clear();
            return Ok(true);
        } else if input.starts_with(":reset") {
            // Clears both the input and output history, and forgets all state and imported modules
            // applies to all ConsoleShareds created from this Console as well!

            self.scope = Arc::new(Mutex::new(Self::create_scope()));

            self.input_history.clear();
            self.output_history.clear();
            {
                let mut modules = self.modules.lock();
                modules.clear();
            }

            return Ok(true);
        } else if input.starts_with(":exec ") {
            // Execute the provided script, without importing any functions from it
            let file_path = &self.input_line[6..].to_string();
            let result = self.eval_file(reactor, true, &file_path);

            if let Err(err) = result {
                debug!(
                    "console :exec of file '{}' failed: {:?}",
                    file_path, err
                );
            }

            return Ok(true);
        } else if input.starts_with(":import ") {
            // Import the provided script module
            log::debug!("importing file");
            let file_path = &self.input_line[8..].to_string();
            let result = self.import_file(&file_path);

            if let Err(err) = result {
                let msg = format!(
                    " >>> error importing file {}: {:?}",
                    file_path, err
                );
                self.append_output(&msg);

                log::warn!(
                    "console :import of file '{}' failed: {:?}",
                    file_path,
                    err
                );
            }

            return Ok(true);
        } else if input.starts_with(":start_interval ") {
            // run the provided script every 30ms
            // the handle can be used with `:end_interval` to stop it
            let mut fields = self.input_line.split_ascii_whitespace();

            fields.next();
            let file_name = fields.next();
            let handle_name = fields.next();

            if let (Some(file), Some(handle)) = (file_name, handle_name) {
                let file = file.to_string();
                let handle = handle.to_string();
                self.eval_file_interval(reactor, &handle, &file)?;
            }

            return Ok(true);
        } else if input.starts_with(":end_interval ") {
            // see `:start_interval`
            let handle = &self.input_line[":end_interval ".len()..].to_string();
            self.stop_interval(&handle);

            return Ok(true);
        }

        Ok(false)
    }

    fn handle_eval_result(
        &mut self,
        print: bool,
        result: std::result::Result<rhai::Dynamic, Box<rhai::EvalAltResult>>,
    ) -> Result<()> {
        match result {
            Ok(result) => {
                if print {
                    let rtype = result.type_id();
                    let type_name = result.type_name();

                    // Handle printing the result to the console output as appropriate
                    if let Ok(_) = result.as_unit() {
                        // don't log unit
                    } else if rtype == TypeId::of::<rgb::RGB<f32>>() {
                        let color = result.cast::<rgb::RGB<f32>>();
                        self.append_output(&format!("{}", color))
                    } else if rtype == TypeId::of::<rgb::RGBA<f32>>() {
                        let color = result.cast::<rgb::RGBA<f32>>();
                        self.append_output(&format!("{}", color));
                    } else if type_name == "string" {
                        if let Ok(result) = result.as_string() {
                            self.append_output(&result);
                        }
                    } else {
                        self.append_output(&format!("{:?}", result));
                    }
                }
            }
            Err(err) => {
                debug!("Eval error: {:?}", err);
                if print {
                    self.append_output(&format!("Error: {:?}", err));
                }
            }
        }

        Ok(())
    }

    pub fn import_file(&mut self, file: &str) -> Result<()> {
        let engine = self.create_engine();

        let ast = engine.compile_file(file.into())?;
        let module =
            rhai::Module::eval_ast_as_new(rhai::Scope::new(), &ast, &engine)?;

        let (vars, funcs, iters) = module.count();

        let msg = format!(
            " >>> imported {} variables, {} functions, and {} iterators from '{}'", vars, funcs, iters, file);
        self.append_output(&msg);

        {
            let mut modules = self.modules.lock();
            modules.push(Arc::new(module));
        }

        Ok(())
    }

    pub fn eval(&mut self, reactor: &mut Reactor, _print: bool) -> Result<()> {
        let engine = self.create_engine();

        let result_tx = self.result_tx.clone();

        let input = self.input_line.to_string();

        let scope = self.scope.clone();

        let handle = reactor.spawn(async move {
            let mut scope = scope.lock();

            let result =
                engine.eval_with_scope::<rhai::Dynamic>(&mut scope, &input);
            let _ = result_tx.send(result);
        })?;

        handle.forget();

        Ok(())
    }

    pub fn ui(
        &mut self,
        ctx: &egui::CtxRef,
        is_down: bool,
        reactor: &mut Reactor,
    ) {
        {
            let mut win_defs = self.window_defs.lock();

            for win_def in win_defs.iter_mut() {
                win_def.show(ctx);
            }
        }

        while let Ok(result) = self.result_rx.try_recv() {
            self.handle_eval_result(true, result).unwrap();
        }

        if !is_down {
            return;
        }

        egui::Window::new(Self::ID)
            .resizable(false)
            .auto_sized()
            .title_bar(false)
            .collapsible(false)
            .enabled(is_down)
            .anchor(egui::Align2::CENTER_TOP, Point::new(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(ctx.input().screen_rect().width());

                let scope_locked = self.scope.is_locked();

                let mut output_lines = Vec::with_capacity(20);

                for output_line in
                    self.output_history.iter().rev().skip(self.output_offset)
                {
                    if output_lines.len() >= 20 {
                        break;
                    }

                    let split_lines = output_line.lines().rev();

                    for line in split_lines {
                        if output_lines.len() >= 20 {
                            break;
                        }

                        output_lines.push(egui::Label::new(line).monospace());
                    }
                }

                output_lines.reverse();

                let mut output_resp: Option<egui::Response> = None;

                for label in output_lines {
                    let resp = ui.add(label);
                    if let Some(union) = output_resp.as_mut() {
                        *union = union.union(resp);
                    } else {
                        output_resp = Some(resp);
                    }
                }

                if let Some(resp) = output_resp {
                    let mut rect = resp.rect;
                    rect.set_width(ui.available_width());

                    let interact = ui.interact(
                        rect,
                        egui::Id::new("console_lines"),
                        egui::Sense::hover(),
                    );
                    if interact.hovered() {
                        let scroll = ui.input().scroll_delta.y;

                        let mag = scroll.abs();
                        let delta = ((mag / 4.0) as usize).max(1);

                        let mut delta = delta as isize;
                        if scroll < 0.0 {
                            delta *= -1;
                        }

                        if mag > 1.0 {
                            self.scrollback(delta);
                        }
                    }
                }

                let old_input = self.input_line.clone();

                let input = {
                    let line_count = self.input_line.lines().count().max(1);

                    if scope_locked {
                        let mut empty = "> Executing...".to_string();
                        ui.add(
                            egui::TextEdit::multiline(&mut empty)
                                .id(egui::Id::new(Self::ID_TEXT))
                                .desired_rows(line_count)
                                .code_editor()
                                .lock_focus(true)
                                .enabled(false)
                                .desired_width(ui.available_width()),
                        )
                    } else {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.input_line)
                                .id(egui::Id::new(Self::ID_TEXT))
                                .desired_rows(line_count)
                                .code_editor()
                                .lock_focus(true)
                                .enabled(true)
                                .desired_width(ui.available_width()),
                        )
                    }
                };

                // hack to keep input
                if self.request_focus && !scope_locked {
                    if input.has_focus() {
                        self.request_focus = false;
                    }
                    input.request_focus();
                }

                if ui.input().key_pressed(egui::Key::ArrowUp) {
                    self.step_history(true);
                }

                if ui.input().key_pressed(egui::Key::ArrowDown) {
                    self.step_history(false);
                }

                if ui.input().key_pressed(egui::Key::Enter) && !scope_locked {
                    if ui.input().modifiers.shift {
                        // insert newline;
                    } else {
                        // evaluate input
                        self.input_line = old_input;
                        log::debug!("console input line: {}", self.input_line);

                        self.input_history.push(self.input_line.clone());
                        self.append_output(&format!("> {}", self.input_line));

                        self.eval_input(reactor, true).unwrap();

                        let mut line =
                            String::with_capacity(self.input_line.capacity());
                        std::mem::swap(&mut self.input_line, &mut line);

                        self.input_line.clear();

                        self.input_history_ix.take();
                    }

                    // input.request_focus() has to be called the
                    // frame *after* this piece of code is ran, hence
                    // the bool etc.
                    // input.request_focus();
                    self.request_focus = true;
                }
            });
    }

    fn step_history(&mut self, backward: bool) {
        if self.input_history.is_empty() {
            return;
        }

        if let Some(ix) = self.input_history_ix.as_mut() {
            #[rustfmt::skip]
            let ix = (backward && *ix > 0)
                      .then(|| *ix -= 1)
                .or((!backward && *ix < self.input_history.len())
                      .then(|| *ix += 1))
                .map(|_| *ix);

            let input_history = &self.input_history;
            if let Some(ix) = ix.and_then(|ix| input_history.get(ix)) {
                self.input_line.clone_from(ix);
            } else {
                self.input_line.clear();
                self.input_history_ix = None;
            }
        } else {
            let ix = backward
                .then(|| self.input_history.len().checked_sub(1))
                .flatten()
                .unwrap_or(0);

            self.input_history_ix = Some(ix);

            if let Some(line) = self.input_history.get(ix) {
                self.input_line.clone_from(line);
            }
        }
    }

    fn scrollback(&mut self, delta: isize) {
        let reverse = delta < 0;
        let delta = delta.abs() as usize;

        if reverse {
            self.output_offset =
                self.output_offset.checked_sub(delta).unwrap_or(0);
        } else {
            let max_count =
                self.output_history.len().checked_sub(20).unwrap_or(0);

            self.output_offset = (self.output_offset + delta).min(max_count);
        }
    }
}

/// Holds both the closures used with the `get` and `set` commands
/// (defined in [`ConsoleShared::create_engine`]), and the generic
/// console variable map, accessible via (`get_var` and `set_var`).
#[derive(Default)]
pub struct GetSetTruth {
    getters:
        HashMap<String, Box<dyn Fn() -> rhai::Dynamic + Send + Sync + 'static>>,
    setters:
        HashMap<String, Box<dyn Fn(rhai::Dynamic) + Send + Sync + 'static>>,

    console_vars: Mutex<HashMap<String, rhai::Dynamic>>,
}

impl GetSetTruth {
    pub fn add_var(&mut self, name: &str, val: rhai::Dynamic) {
        let mut lock = self.console_vars.lock();
        lock.insert(name.to_string(), val);
    }

    pub fn add_arc_atomic_cell_get_set<T>(
        &mut self,
        name: &str,
        arc: Arc<AtomicCell<T>>,
        to_dyn: impl Fn(T) -> rhai::Dynamic + Send + Sync + 'static,
        from_dyn: impl Fn(rhai::Dynamic) -> Option<T> + Send + Sync + 'static,
    ) where
        T: Copy + Send + Sync + 'static,
    {
        let arc_ = arc.clone();
        let getter = move || {
            let t = arc_.load();
            to_dyn(t)
        };

        let setter = move |v: rhai::Dynamic| {
            if let Some(v) = from_dyn(v) {
                arc.store(v);
            }
        };

        self.getters.insert(name.to_string(), Box::new(getter) as _);
        self.setters.insert(name.to_string(), Box::new(setter) as _);
    }

    pub fn add_dynamic<T>(
        &mut self,
        name: &str,
        get: impl Fn() -> T + Send + Sync + 'static,
        set: impl Fn(T) + Send + Sync + 'static,
    ) where
        T: Clone + Send + Sync + 'static,
    {
        let getter = move || {
            let v = get();
            rhai::Dynamic::from(v)
        };

        let setter = move |val: rhai::Dynamic| {
            let val: T = val.cast();
            set(val);
        };

        self.getters.insert(name.to_string(), Box::new(getter) as _);
        self.setters.insert(name.to_string(), Box::new(setter) as _);
    }
}

impl ConsoleShared {
    /// Creates the Rhai engine, adding all types, modules, and
    /// functions available in the console.
    pub fn create_engine(&self) -> rhai::Engine {
        use rhai::plugin::*;

        let mut engine = crate::script::create_engine();

        // TODO this should be configurable in the app options
        engine.set_max_call_levels(16);
        engine.set_max_expr_depths(0, 0);

        let result_tx = self.result_tx.clone();
        engine.on_print(move |x| {
            result_tx
                .send(Ok(rhai::Dynamic::from(x.to_string())))
                .unwrap();
        });

        engine.register_type::<Point>();

        self.add_annotation_fns(&mut engine);

        // Constructor and getters & setters for the Point struct
        engine.register_fn("Point", |x: f32, y: f32| Point::new(x, y));
        engine.register_get_set(
            "x",
            |p: &mut Point| p.x,
            |p: &mut Point, x| p.x = x,
        );
        engine.register_get_set(
            "y",
            |p: &mut Point| p.y,
            |p: &mut Point, y| p.y = y,
        );

        // Not really necessary, as you can use the built-in `print`
        // function, but this one does a bit more logic with how the
        // input is printed (see handle_eval_result())
        let result_tx = self.result_tx.clone();
        engine.register_fn("log", move |v: rhai::Dynamic| {
            result_tx.send(Ok(v)).unwrap();
        });

        // the cloned Arc containing the graph is moved into the
        // closure, which is registered as a regular function in Rhai
        let graph = self.graph.clone();
        engine.register_fn("get_graph", move || graph.graph.clone());

        let graph = self.graph.clone();
        engine.register_fn("get_path_positions", move || {
            graph.path_positions.clone()
        });

        self.add_view_fns(&mut engine);

        self.add_overlay_fns(&mut engine);

        self.add_modal_fns(&mut engine);

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("get_selection", move || {
            use crossbeam::channel;

            let (tx, rx) = channel::bounded::<(Rect, FxHashSet<NodeId>)>(1);
            let msg = AppMsg::RequestSelection(tx);

            app_msg_tx.send(msg).unwrap();

            let (_rect, result) = rx
                .recv()
                .expect("Console error when retrieving the current selection");

            NodeSelection { nodes: result }
        });

        // TODO probably... don't do it like this
        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("get_selection_center", move || {
            use crossbeam::channel;

            let (tx, rx) = channel::unbounded::<(Rect, FxHashSet<NodeId>)>();
            let msg = AppMsg::RequestSelection(tx.clone());

            app_msg_tx.send(msg).unwrap();

            let (rect, _result) =
                std::thread::spawn(move || rx.recv().unwrap())
                    .join()
                    .unwrap();

            rect.center()
        });

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("set_selection", move |selection: NodeSelection| {
            let msg = AppMsg::Selection(Select::Many {
                nodes: selection.nodes,
                clear: true,
            });
            app_msg_tx.send(msg).unwrap();
        });

        // this version is used if the input is a single node
        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("set_selection", move |node: NodeId| {
            let msg = AppMsg::Selection(Select::Many {
                nodes: Some(node).into_iter().collect(),
                clear: true,
            });
            app_msg_tx.send(msg).unwrap();
        });

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("pan_to_active_selection", move || {
            let msg = AppMsg::GotoSelection;
            app_msg_tx.send(msg).unwrap();
        });

        let graph = self.graph.graph.clone();
        engine.register_fn(
            "path_selection",
            move |path: PathId| -> NodeSelection {
                let mut selection = NodeSelection::default();
                if let Some(steps) = graph.path_steps(path) {
                    for step in steps {
                        let id = step.handle().id();
                        selection.add_one(false, id);
                    }
                }
                selection
            },
        );

        // variant of the above that takes a path name instead of ID, for convenience
        let graph = self.graph.graph.clone();
        engine.register_result_fn("path_selection", move |path_name: &str| {
            if let Some(path) = graph.get_path_id(path_name.as_bytes()) {
                let mut selection = NodeSelection::default();
                if let Some(steps) = graph.path_steps(path) {
                    for step in steps {
                        let id = step.handle().id();
                        selection.add_one(false, id);
                    }
                }
                Ok(selection)
            } else {
                Err("The provided path does not exist".into())
            }
        });

        let arc = self.shared_state.hover_node.clone();
        engine.register_fn("get_hover_node", move || arc.load());

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("toggle_dark_mode", move || {
            app_msg_tx.send(crate::app::AppMsg::ToggleDarkMode).unwrap();
        });

        // Actually add the `get` and `set` functions, see Console::new as well
        let get_set = self.get_set.clone();
        engine.register_result_fn("get", move |name: &str| {
            get_set
                .getters
                .get(name)
                .map(|get| get())
                .ok_or(format!("Setting `{}` not found", name).into())
        });

        let get_set = self.get_set.clone();

        engine.register_result_fn(
            "set",
            move |name: &str, val: rhai::Dynamic| {
                get_set
                    .setters
                    .get(name)
                    .map(|set| set(val))
                    .ok_or(format!("Setting `{}` not found", name).into())
            },
        );

        let get_set = self.get_set.clone();
        engine.register_result_fn("get_var", move |name: &str| {
            let lock = get_set.console_vars.try_lock();
            let val = lock.and_then(|l| l.get(name).cloned());
            val.ok_or(format!("Global variable `{}` not found", name).into())
        });

        let get_set = self.get_set.clone();
        engine.register_fn("set_var", move |name: &str, val: rhai::Dynamic| {
            let mut lock = get_set.console_vars.lock();
            lock.insert(name.to_string(), val);
        });

        let handle = exported_module!(crate::script::plugins::handle_plugin);

        // TODO it's probably a bad idea to have this without a way to
        // cancel/abort running scripts
        engine.register_fn("sleep", |ms: i64| {
            let dur = std::time::Duration::from_millis(ms as u64);
            std::thread::sleep(dur);
        });

        engine.register_global_module(handle.into());

        engine
    }

    pub fn add_modal_fns(&self, engine: &mut rhai::Engine) {
        fn futures_helper<T: Send + Sync + 'static>(
            mut rx: futures::channel::mpsc::Receiver<Option<T>>,
        ) -> Option<T> {
            let result = std::thread::spawn(move || {
                let val =
                    futures::executor::block_on(async move { rx.next().await })
                        .flatten();
                val
            })
            .join();

            match result {
                Ok(v) => v,
                _ => None,
            }
        }

        let modal_tx = self.channels.modal_tx.clone();
        let show_modal = self.shared_state.show_modal.clone();
        engine.register_fn("get_string_modal", move || {
            let (result_tx, result_rx) =
                futures::channel::mpsc::channel::<Option<String>>(1);

            // using an atomic bool we can easily check if it's the
            // first time this specific modal is opened, and give
            // focus to the text box
            let first_run = AtomicCell::new(true);

            let callback = move |text: &mut String, ui: &mut egui::Ui| {
                ui.label("Enter string");
                let text_box = ui.text_edit_singleline(text);

                if first_run.fetch_and(false) {
                    text_box.request_focus();
                }

                if text_box.lost_focus()
                    && ui.input().key_pressed(egui::Key::Enter)
                {
                    return Ok(ModalSuccess::Success);
                }

                Err(ModalError::Continue)
            };

            let prepared = ModalHandler::prepare_callback(
                &show_modal,
                String::new(),
                callback,
                result_tx,
            );

            modal_tx.send(prepared).unwrap();

            let result = futures_helper(result_rx);
            result.unwrap_or_default()
        });

        let modal_tx = self.channels.modal_tx.clone();
        let show_modal = self.shared_state.show_modal.clone();
        let graph = self.graph.clone();
        engine.register_result_fn("get_node_modal", move || {
            let (result_tx, result_rx) =
                futures::channel::mpsc::channel::<Option<String>>(1);

            let first_run = AtomicCell::new(true);

            let callback = move |text: &mut String, ui: &mut egui::Ui| {
                ui.label("Enter node ID");
                let text_box = ui.text_edit_singleline(text);

                if first_run.fetch_and(false) {
                    text_box.request_focus();
                }

                if text_box.lost_focus()
                    && ui.input().key_pressed(egui::Key::Enter)
                {
                    return Ok(ModalSuccess::Success);
                }

                Err(ModalError::Continue)
            };

            let prepared = ModalHandler::prepare_callback(
                &show_modal,
                String::new(),
                callback,
                result_tx,
            );

            modal_tx.send(prepared).unwrap();

            let result_str = futures_helper(result_rx).unwrap_or_default();

            match result_str.parse::<u64>() {
                Ok(raw) => {
                    let node_id = NodeId::from(raw);
                    if graph.graph.has_node(node_id) {
                        Ok(node_id)
                    } else {
                        Err("Node not found".into())
                    }
                }
                _ => Err("Could not parse node ID".into()),
            }
        });
    }

    fn add_overlay_fns(&self, engine: &mut rhai::Engine) {
        engine.register_type_with_name::<(usize, OverlayKind)>("OverlayHandle");

        // returns `false` if there is no active overlay
        let overlay_state = self.shared_state.overlay_state.clone();
        engine.register_fn("get_active_overlay", move || -> rhai::Dynamic {
            if let Some(cur_overlay) = overlay_state.current_overlay() {
                rhai::Dynamic::from(cur_overlay)
            } else {
                false.into()
            }
        });

        let overlay_state = self.shared_state.overlay_state.clone();
        engine.register_fn("set_active_overlay", move |v: rhai::Dynamic| {
            if let Ok(_) = v.as_unit() {
                overlay_state.set_current_overlay(None);
            } else if let Some(overlay) = v.try_cast::<usize>() {
                overlay_state.set_current_overlay(Some(overlay));
            }
        });

        let overlay_list: Arc<_> = self.overlay_list.clone();
        // let overlay_map: Arc<HashMap<String, (usize, OverlayKind)>> =
        engine.register_fn("get_overlays", move || {
            // TODO: should probably use try_lock -- but the overlays
            // shouldn't be organized like this anyway
            let overlays = overlay_list.lock();
            overlays
                .iter()
                .map(|v| rhai::Dynamic::from(v.to_owned()))
                .collect::<Vec<_>>()
        });

        engine.register_fn(
            "overlay_name",
            move |overlay: (usize, OverlayKind, String)| overlay.2,
        );

        engine.register_fn(
            "overlay_id",
            move |overlay: (usize, OverlayKind, String)| (overlay.0, overlay.1),
        );
    }

    fn add_view_fns(&self, engine: &mut Engine) {
        engine.register_type::<View>();

        engine.register_get_set(
            "scale",
            |v: &mut View| v.scale,
            |v, s| v.scale = s,
        );
        engine.register_get_set(
            "center",
            |v: &mut View| v.center,
            |v, c| v.center = c,
        );

        // NB: these are not regular console get/sets because the view
        // system will likely be reworked soon, to make it a queue --
        // and remove direct mutable access by other systems
        // (but for now, load/store is enough)
        let view = self.shared_state.view.clone();
        engine.register_fn("get_view", move || view.load());

        let view = self.shared_state.view.clone();
        engine.register_fn("set_view", move |v: View| view.store(v));

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("goto_node", move |node: NodeId| {
            app_msg_tx.send(AppMsg::GotoNode(node)).unwrap();

            let msg = AppMsg::Selection(Select::One { node, clear: true });
            app_msg_tx.send(msg).unwrap();
        });

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("goto_node", move |node: i64| {
            let node = NodeId::from(node as u64);
            app_msg_tx.send(AppMsg::GotoNode(node)).unwrap();

            let msg = AppMsg::Selection(Select::One { node, clear: true });
            app_msg_tx.send(msg).unwrap();
        });

        let view = self.shared_state.view.clone();
        engine.register_fn("set_view_origin", move |p: Point| {
            let mut v = view.load();
            v.center = p;
            view.store(v);
        });

        let view = self.shared_state.view.clone();
        engine.register_fn("set_scale", move |s: f32| {
            let mut v = view.load();
            v.scale = s;
            view.store(v);
        });

        let mouse = self.shared_state.mouse_pos.clone();
        let view = self.shared_state.view.clone();
        let screen_dims = self.shared_state.screen_dims.clone();

        engine.register_fn("get_cursor_world", move || {
            let screen = mouse.load();
            let view = view.load();
            let dims = screen_dims.load();
            view.screen_point_to_world(dims, screen)
        });
    }

    fn error_helper<T: Clone + 'static>(
        result: &std::result::Result<
            Result<rhai::Dynamic>,
            Box<dyn std::any::Any + Send>,
        >,
    ) -> std::result::Result<T, Box<EvalAltResult>> {
        let out = match result {
            Ok(Ok(success)) => {
                if success.type_id() == TypeId::of::<T>() {
                    let success = success.clone();
                    let result = success.cast::<T>();
                    Ok(result)
                } else {
                    Err("Received incorrect type from App; this shouldn't happen!!!".into())
                }
            }
            Ok(Err(_req_err)) => {
                Err("Error when retrieving results from app request thread"
                    .into())
            }
            Err(_spawn_err) => {
                Err("Error when spawning app request thread".into())
            }
        };

        out
    }

    fn add_annotation_fns(&self, engine: &mut rhai::Engine) {
        engine.register_result_fn(
            "get_record",
            move |coll: &mut Arc<Gff3Records>, ix: i64| {
                if let Some(record) = coll.records().get(ix as usize).cloned() {
                    Ok(record)
                } else {
                    Err(Box::new(EvalAltResult::ErrorArrayBounds(
                        coll.records().len(),
                        ix as i64,
                        rhai::Position::NONE,
                    )))
                }
            },
        );

        engine.register_result_fn(
            "get_record",
            move |coll: &mut Arc<BedRecords>, ix: i64| {
                if let Some(record) = coll.records().get(ix as usize).cloned() {
                    Ok(record)
                } else {
                    Err(Box::new(EvalAltResult::ErrorArrayBounds(
                        coll.records().len(),
                        ix as i64,
                        rhai::Position::NONE,
                    )))
                }
            },
        );

        engine.register_fn("len", move |coll: &mut Arc<Gff3Records>| {
            coll.len() as i64
        });

        engine.register_fn("len", move |coll: &mut Arc<BedRecords>| {
            coll.len() as i64
        });

        engine.register_fn("gff3_column", |key: &str| match key {
            "SeqId" => Gff3Column::SeqId,
            "Source" => Gff3Column::Source,
            "Type" => Gff3Column::Type,
            "Start" => Gff3Column::Start,
            "End" => Gff3Column::End,
            "Score" => Gff3Column::Score,
            "Strand" => Gff3Column::Strand,
            "Frame" => Gff3Column::Frame,
            attr => Gff3Column::Attribute(attr.as_bytes().to_owned()),
        });

        engine
            .register_fn("bed_column", |ix: i64| BedColumn::Index(ix as usize));
        engine.register_result_fn(
            "bed_column",
            |coll: &mut Arc<BedRecords>, header: &str| {
                if let Some(col) = coll.header_to_column(header.as_bytes()) {
                    Ok(col)
                } else {
                    Err("Header not found in provided BED file".into())
                }
            },
        );
        engine.register_result_fn("bed_column", |key: &str| match key {
            "Chr" => Ok(BedColumn::Chr),
            "Start" => Ok(BedColumn::Start),
            "End" => Ok(BedColumn::End),
            "Name" => Ok(BedColumn::Name),
            _ => Err("Only headers \"name\", \"start\", \"end\", and \"name\" can be referred to without a BED record context".into()),
        });

        fn get_impl<R, K>(record: &mut R, column: K) -> rhai::Dynamic
        where
            R: AnnotationRecord<ColumnKey = K>,
            K: ColumnKey,
        {
            if column == K::seq_id() {
                let seq_id = record.seq_id();
                rhai::Dynamic::from(seq_id.to_str().unwrap().to_string())
            } else if column == K::start() {
                rhai::Dynamic::from(record.start())
            } else if column == K::end() {
                rhai::Dynamic::from(record.end())
            } else {
                let fields = record.get_all(&column);
                let dyn_fields = fields
                    .into_iter()
                    .map(|val| {
                        rhai::Dynamic::from(format!("{}", val.as_bstr()))
                    })
                    .collect::<Vec<_>>();

                rhai::Dynamic::from(dyn_fields)
            }
        }

        engine.register_fn(
            "get",
            move |record: &mut Gff3Record, column: Gff3Column| {
                get_impl(record, column)
            },
        );

        engine.register_fn(
            "get",
            move |record: &mut BedRecord, column: BedColumn| {
                get_impl(record, column)
            },
        );

        let app_msg_tx = self.channels.app_tx.clone();
        let graph = self.graph.graph.clone();
        engine.register_fn(
            "set_collection_ref_path",
            move |name: &str, path_name: &str| {
                let key = "annotation_ref_path".to_string();
                let index = name.to_string();

                let path_id =
                    if let Some(id) = graph.get_path_id(path_name.as_bytes()) {
                        id
                    } else {
                        return ();
                    };

                let msg: AppMsg = AppMsg::SetData {
                    key,
                    index,
                    value: rhai::Dynamic::from(path_id),
                };

                app_msg_tx.send(msg).unwrap();
            },
        );

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_result_fn(
            "get_collection_ref_path",
            move |name: &str| {
                let key = "annotation_ref_path".to_string();
                let index = name.to_string();

                let (tx, rx) =
                    crossbeam::channel::bounded::<Result<rhai::Dynamic>>(1);

                let msg: AppMsg = AppMsg::RequestData {
                    key,
                    index,
                    sender: tx,
                };

                app_msg_tx.send(msg).unwrap();

                let result =
                    std::thread::spawn(move || rx.recv().unwrap()).join();

                if let Ok(_) = Self::error_helper::<()>(&result) {
                    return Ok(rhai::Dynamic::from(false));
                }
                let result = Self::error_helper::<PathId>(&result)?;

                Ok(rhai::Dynamic::from(result))
            },
        );

        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_fn("list_collections", move || {
            let key = "annotation_names".to_string();
            let index = "".to_string();

            let (tx, rx) =
                crossbeam::channel::bounded::<Result<rhai::Dynamic>>(1);

            let msg: AppMsg = AppMsg::RequestData {
                key,
                index,
                sender: tx,
            };

            app_msg_tx.send(msg).unwrap();

            let result = std::thread::spawn(move || rx.recv().unwrap()).join();
            let result = Self::error_helper::<Vec<String>>(&result).unwrap();

            let result = result
                .into_iter()
                .map(rhai::Dynamic::from)
                .collect::<Vec<_>>();

            result
        });

        let app_msg_tx = self.channels.app_tx.clone();
        let result_tx = self.result_tx.clone();
        engine.register_result_fn("load_collection", move |path: &str| {
            let file = PathBuf::from(path);

            let ext = file.extension().and_then(|ext| ext.to_str()).map_or(
                Err("Missing file extension".into())
                    as std::result::Result<_, Box<EvalAltResult>>,
                |ext| Ok(ext),
            )?;

            if ext == "gff3" {
                let records = Gff3Records::parse_gff3_file(&file);
                match records {
                    Ok(records) => {
                        app_msg_tx
                            .send(AppMsg::AddGff3Records(records))
                            .unwrap();

                        result_tx
                            .send(Ok(rhai::Dynamic::from("Loaded GFF3 file")))
                            .unwrap();

                        return Ok(());
                    }
                    Err(_err) => {
                        return Err("Error parsing GFF3 file".into());
                    }
                }
            } else if ext == "bed" {
                let records = BedRecords::parse_bed_file(&file);
                match records {
                    Ok(records) => {
                        app_msg_tx
                            .send(AppMsg::AddBedRecords(records))
                            .unwrap();

                        result_tx
                            .send(Ok(rhai::Dynamic::from("Loaded BED file")))
                            .unwrap();

                        return Ok(());
                    }
                    Err(_err) => {
                        return Err("Error parsing BED file".into());
                    }
                }
            } else {
                return Err("Invalid file extension".into());
            }
        });

        // this one's messy, there should be a better system in place
        // for requesting data like this
        let app_msg_tx = self.channels.app_tx.clone();
        engine.register_result_fn("get_collection", move |c_name: &str| {
            use crossbeam::channel;
            let key = "annotation_file".to_string();
            let index = c_name.to_string();

            let (tx, rx) = channel::bounded::<Result<rhai::Dynamic>>(1);

            let msg: AppMsg = AppMsg::RequestData {
                key,
                index,
                sender: tx,
            };

            app_msg_tx.send(msg).unwrap();

            let result = std::thread::spawn(move || rx.recv().unwrap()).join();

            if let Err(_) = &result {
                return Err("Error spawning console request thread".into());
            }

            let result = result.unwrap().unwrap();

            if result.type_id() == TypeId::of::<Arc<Gff3Records>>()
                || result.type_id() == TypeId::of::<Arc<BedRecords>>()
            {
                return Ok(result);
            }

            Err("Error retrieving data".into())
        });

        fn create_label_set_impl<C, K>(
            app_msg_tx: &crossbeam::channel::Sender<AppMsg>,
            graph: &Arc<GraphQuery>,

            annots: &mut Arc<C>,
            record_indices: Vec<rhai::Dynamic>,
            path_id: PathId,
            column: K,
            label_set_name: &str,
        ) where
            C: AnnotationCollection<ColumnKey = K> + Send + Sync + 'static,
            K: ColumnKey,
        {
            log::warn!("in create_label_set");
            let record_indices = record_indices
                .into_iter()
                .filter_map(|i| {
                    let i = i.as_int().ok()?;

                    Some(i as usize)
                })
                .collect::<Vec<_>>();

            let path_name = graph.graph.get_path_name_vec(path_id).unwrap();
            let path_name = path_name.to_str().unwrap();

            log::warn!("calling calculate_annotation_set");
            let label_set =
                crate::gui::windows::annotations::calculate_annotation_set(
                    graph,
                    annots.as_ref(),
                    &record_indices,
                    path_id,
                    path_name,
                    &column,
                    label_set_name,
                );

            if let Some(label_set) = label_set {
                log::warn!("label set calculated");
                let name = label_set_name.to_string();

                app_msg_tx
                    .send(AppMsg::NewNodeLabels { name, label_set })
                    .unwrap();
            } else {
                log::warn!("error calculating the label set");
            }
        }

        let app_msg_tx = self.channels.app_tx.clone();
        let graph = self.graph.clone();
        engine.register_fn(
            "create_label_set",
            move |annots: &mut Arc<Gff3Records>,
                  record_indices: Vec<rhai::Dynamic>,
                  path_id: PathId,
                  column: Gff3Column,
                  label_set_name: &str| {
                create_label_set_impl(
                    &app_msg_tx,
                    &graph,
                    annots,
                    record_indices,
                    path_id,
                    column,
                    label_set_name,
                )
            },
        );

        let app_msg_tx = self.channels.app_tx.clone();
        let graph = self.graph.clone();
        engine.register_fn(
            "create_label_set",
            move |annots: &mut Arc<BedRecords>,
                  record_indices: Vec<rhai::Dynamic>,
                  path_id: PathId,
                  column: BedColumn,
                  label_set_name: &str| {
                create_label_set_impl(
                    &app_msg_tx,
                    &graph,
                    annots,
                    record_indices,
                    path_id,
                    column,
                    label_set_name,
                )
            },
        );
    }
}

fn virtual_key_code_map() -> HashMap<String, winit::event::VirtualKeyCode> {
    use winit::event::VirtualKeyCode as Key;

    let keys = [
        ("Key1", Key::Key1),
        ("Key2", Key::Key2),
        ("Key3", Key::Key3),
        ("Key4", Key::Key4),
        ("Key5", Key::Key5),
        ("Key6", Key::Key6),
        ("Key7", Key::Key7),
        ("Key8", Key::Key8),
        ("Key9", Key::Key9),
        ("Key0", Key::Key0),
        ("A", Key::A),
        ("B", Key::B),
        ("C", Key::C),
        ("D", Key::D),
        ("E", Key::E),
        ("F", Key::F),
        ("G", Key::G),
        ("H", Key::H),
        ("I", Key::I),
        ("J", Key::J),
        ("K", Key::K),
        ("L", Key::L),
        ("M", Key::M),
        ("N", Key::N),
        ("O", Key::O),
        ("P", Key::P),
        ("Q", Key::Q),
        ("R", Key::R),
        ("S", Key::S),
        ("T", Key::T),
        ("U", Key::U),
        ("V", Key::V),
        ("W", Key::W),
        ("X", Key::X),
        ("Y", Key::Y),
        ("Z", Key::Z),
        ("Escape", Key::Escape),
        ("F1", Key::F1),
        ("F2", Key::F2),
        ("F3", Key::F3),
        ("F4", Key::F4),
        ("F5", Key::F5),
        ("F6", Key::F6),
        ("F7", Key::F7),
        ("F8", Key::F8),
        ("F9", Key::F9),
        ("F10", Key::F10),
        ("F11", Key::F11),
        ("F12", Key::F12),
        ("F13", Key::F13),
        ("F14", Key::F14),
        ("F15", Key::F15),
        ("F16", Key::F16),
        ("F17", Key::F17),
        ("F18", Key::F18),
        ("F19", Key::F19),
        ("F20", Key::F20),
        ("F21", Key::F21),
        ("F22", Key::F22),
        ("F23", Key::F23),
        ("F24", Key::F24),
        ("Snapshot", Key::Snapshot),
        ("Scroll", Key::Scroll),
        ("Pause", Key::Pause),
        ("Insert", Key::Insert),
        ("Home", Key::Home),
        ("Delete", Key::Delete),
        ("End", Key::End),
        ("PageDown", Key::PageDown),
        ("PageUp", Key::PageUp),
        ("Left", Key::Left),
        ("Up", Key::Up),
        ("Right", Key::Right),
        ("Down", Key::Down),
        ("Back", Key::Back),
        ("Return", Key::Return),
        ("Space", Key::Space),
        ("Compose", Key::Compose),
        ("Caret", Key::Caret),
        ("Numlock", Key::Numlock),
        ("Numpad0", Key::Numpad0),
        ("Numpad1", Key::Numpad1),
        ("Numpad2", Key::Numpad2),
        ("Numpad3", Key::Numpad3),
        ("Numpad4", Key::Numpad4),
        ("Numpad5", Key::Numpad5),
        ("Numpad6", Key::Numpad6),
        ("Numpad7", Key::Numpad7),
        ("Numpad8", Key::Numpad8),
        ("Numpad9", Key::Numpad9),
        ("NumpadAdd", Key::NumpadAdd),
        ("NumpadDivide", Key::NumpadDivide),
        ("NumpadDecimal", Key::NumpadDecimal),
        ("NumpadComma", Key::NumpadComma),
        ("NumpadEnter", Key::NumpadEnter),
        ("NumpadEquals", Key::NumpadEquals),
        ("NumpadMultiply", Key::NumpadMultiply),
        ("NumpadSubtract", Key::NumpadSubtract),
        ("AbntC1", Key::AbntC1),
        ("AbntC2", Key::AbntC2),
        ("Apostrophe", Key::Apostrophe),
        ("Apps", Key::Apps),
        ("Asterisk", Key::Asterisk),
        ("At", Key::At),
        ("Ax", Key::Ax),
        ("Backslash", Key::Backslash),
        ("Calculator", Key::Calculator),
        ("Capital", Key::Capital),
        ("Colon", Key::Colon),
        ("Comma", Key::Comma),
        ("Convert", Key::Convert),
        ("Equals", Key::Equals),
        ("Grave", Key::Grave),
        ("Kana", Key::Kana),
        ("Kanji", Key::Kanji),
        ("LAlt", Key::LAlt),
        ("LBracket", Key::LBracket),
        ("LControl", Key::LControl),
        ("LShift", Key::LShift),
        ("LWin", Key::LWin),
        ("Mail", Key::Mail),
        ("MediaSelect", Key::MediaSelect),
        ("MediaStop", Key::MediaStop),
        ("Minus", Key::Minus),
        ("Mute", Key::Mute),
        ("MyComputer", Key::MyComputer),
        ("NavigateForward", Key::NavigateForward),
        ("NavigateBackward", Key::NavigateBackward),
        ("NextTrack", Key::NextTrack),
        ("NoConvert", Key::NoConvert),
        ("OEM102", Key::OEM102),
        ("Period", Key::Period),
        ("PlayPause", Key::PlayPause),
        ("Plus", Key::Plus),
        ("Power", Key::Power),
        ("PrevTrack", Key::PrevTrack),
        ("RAlt", Key::RAlt),
        ("RBracket", Key::RBracket),
        ("RControl", Key::RControl),
        ("RShift", Key::RShift),
        ("RWin", Key::RWin),
        ("Semicolon", Key::Semicolon),
        ("Slash", Key::Slash),
        ("Sleep", Key::Sleep),
        ("Stop", Key::Stop),
        ("Sysrq", Key::Sysrq),
        ("Tab", Key::Tab),
        ("Underline", Key::Underline),
        ("Unlabeled", Key::Unlabeled),
        ("VolumeDown", Key::VolumeDown),
        ("VolumeUp", Key::VolumeUp),
        ("Wake", Key::Wake),
        ("WebBack", Key::WebBack),
        ("WebFavorites", Key::WebFavorites),
        ("WebForward", Key::WebForward),
        ("WebHome", Key::WebHome),
        ("WebRefresh", Key::WebRefresh),
        ("WebSearch", Key::WebSearch),
        ("WebStop", Key::WebStop),
        ("Yen", Key::Yen),
        ("Copy", Key::Copy),
        ("Paste", Key::Paste),
        ("Cut", Key::Cut),
    ]
    .iter()
    .map(|(n, c)| (n.to_string(), *c))
    .collect();

    keys
}

pub enum ConsoleGuiElem {
    Label { text: String },
    Button { text: String, callback_id: String },
    TextInput { label: String, data_id: String },
    Row { fields: Vec<String> },
}

pub struct ConsoleGuiDsl {
    window_title: String,
    id: egui::Id,
    elements: Vec<ConsoleGuiElem>,
    callbacks: HashMap<String, Box<dyn Fn() + Send + Sync + 'static>>,

    text_data: HashMap<String, String>,
}

impl ConsoleGuiDsl {
    pub fn new(window_title: &str, id: egui::Id) -> Self {
        Self {
            window_title: window_title.to_string(),
            id,
            elements: Vec::new(),
            callbacks: HashMap::default(),

            text_data: HashMap::default(),
        }
    }

    pub fn get_text_data(&self, data_id: &str) -> Option<&str> {
        self.text_data.get(data_id).map(|s| s.as_str())
    }

    pub fn show(&mut self, ctx: &egui::CtxRef) {
        egui::Window::new(&self.window_title)
            .id(self.id)
            .show(ctx, |ui| {
                for elem in self.elements.iter_mut() {
                    match elem {
                        ConsoleGuiElem::Label { text } => {
                            let text: &str = text;
                            ui.label(text);
                        }
                        ConsoleGuiElem::Button { text, callback_id } => {
                            if ui.button(text).clicked() {
                                if let Some(callback) =
                                    self.callbacks.get(callback_id)
                                {
                                    callback();
                                }
                            }
                        }
                        ConsoleGuiElem::TextInput { label, data_id } => {
                            let data_id: &str = data_id;

                            if let Some(contents) =
                                self.text_data.get_mut(data_id)
                            {
                                let text_edit =
                                    egui::TextEdit::singleline(contents);
                                ui.add(text_edit);
                            }

                            //
                        }
                        ConsoleGuiElem::Row { fields } => {
                            // TODO
                        }
                    }
                }
            });
    }
}
