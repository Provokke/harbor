//! The JS runtime: one V8 isolate + context, the op bridge, the module
//! loader glue and the event loop that drives tokio futures and timers and
//! interleaves them with the V8 microtask queue.
//!
//! ```text
//!   JS  ── ops.op_x(...) ──►  Rust op fn (sync: returns value / throws)
//!                             (async: pushes a future, returns promise id)
//!   event loop:  timers due? ─► __timerDispatch(id) ─► microtask checkpoint
//!                op future done? ─► __opResolve(id, ok, value) ─► checkpoint
//!                unhandled rejection? ─► report, exit 1
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;
use std::task::Poll;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};

use crate::loader;
use crate::ops;
use crate::permissions::Permissions;
use crate::resources::ResourceTable;

/// Error type used by ops; `class` selects the JS error constructor.
#[derive(Debug, Clone)]
pub struct OpError {
    pub class: &'static str,
    pub message: String,
}

impl OpError {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        OpError {
            class,
            message: message.into(),
        }
    }
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new("TypeError", message)
    }
}

impl From<crate::permissions::PermissionDenied> for OpError {
    fn from(e: crate::permissions::PermissionDenied) -> Self {
        OpError::new("PermissionDenied", e.to_string())
    }
}

impl From<crate::resources::ResourceError> for OpError {
    fn from(e: crate::resources::ResourceError) -> Self {
        OpError::new("BadResource", e.to_string())
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        let class = match e.kind() {
            NotFound => "NotFound",
            AlreadyExists => "AlreadyExists",
            PermissionDenied => "PermissionDenied",
            ConnectionRefused => "ConnectionRefused",
            ConnectionReset => "ConnectionReset",
            AddrInUse => "AddrInUse",
            InvalidData | InvalidInput => "InvalidData",
            TimedOut => "TimedOut",
            _ => "Error",
        };
        OpError::new(class, e.to_string())
    }
}

/// Values async ops produce. Converted to V8 values on the JS thread.
#[derive(Debug)]
pub enum OpValue {
    Undefined,
    Bool(bool),
    Number(f64),
    String(String),
    /// Handed to JS as a Uint8Array *without copying*: the Vec's heap
    /// allocation becomes the ArrayBuffer's backing store.
    Bytes(Vec<u8>),
    Object(Vec<(&'static str, OpValue)>),
}

pub type OpResult = Result<OpValue, OpError>;
type PendingOp = Pin<Box<dyn Future<Output = (u32, OpResult)>>>;

/// Futures of in-flight async ops. Kept separate from [`State`] so that a
/// future may borrow `State` while it is being polled.
#[derive(Default)]
pub struct PendingOps(pub RefCell<FuturesUnordered<PendingOp>>);

#[derive(Default)]
pub struct ModuleMap {
    pub by_path: HashMap<PathBuf, v8::Global<v8::Module>>,
    /// identity hash -> (module, path). Hashes may collide, so compare.
    pub by_hash: HashMap<i32, Vec<(v8::Global<v8::Module>, PathBuf)>>,
    pub main: Option<PathBuf>,
}

impl ModuleMap {
    fn path_of(&self, scope: &mut v8::HandleScope, m: v8::Local<v8::Module>) -> Option<PathBuf> {
        let list = self.by_hash.get(&m.get_identity_hash().get())?;
        for (g, p) in list {
            if v8::Local::new(scope, g) == m {
                return Some(p.clone());
            }
        }
        None
    }
}

pub struct State {
    pub permissions: Permissions,
    pub cwd: PathBuf,
    pub resources: ResourceTable,
    /// (deadline, sequence) -> timer id. Sequence keeps FIFO order for equal deadlines.
    pub timers: BTreeMap<(Instant, u64), u32>,
    pub timer_keys: HashMap<u32, (Instant, u64)>,
    pub timer_seq: u64,
    pub unhandled: Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>,
    pub modules: ModuleMap,
    pub op_resolve: Option<v8::Global<v8::Function>>,
    pub timer_dispatch: Option<v8::Global<v8::Function>>,
    pub build_error: Option<v8::Global<v8::Function>>,
    pub inspect: Option<v8::Global<v8::Function>>,
    /// Address of the last buffer created by `op_alloc` (zero-copy proof).
    pub last_alloc_ptr: usize,
}

pub fn state(scope: &mut v8::HandleScope) -> Rc<RefCell<State>> {
    scope
        .get_slot::<Rc<RefCell<State>>>()
        .expect("runtime state missing")
        .clone()
}

pub fn pending_ops(scope: &mut v8::HandleScope) -> Rc<PendingOps> {
    scope
        .get_slot::<Rc<PendingOps>>()
        .expect("pending ops missing")
        .clone()
}

/// Construct a JS error of the given class via the bootstrap's builder so
/// that `instanceof harbor.errors.X` works.
pub fn make_error<'s>(scope: &mut v8::HandleScope<'s>, err: &OpError) -> v8::Local<'s, v8::Value> {
    let builder = state(scope).borrow().build_error.clone();
    let class = v8::String::new(scope, err.class).unwrap();
    let msg = v8::String::new(scope, &err.message).unwrap();
    if let Some(b) = builder {
        let f = v8::Local::new(scope, b);
        let undef = v8::undefined(scope).into();
        if let Some(e) = f.call(scope, undef, &[class.into(), msg.into()]) {
            return e;
        }
    }
    v8::Exception::error(scope, msg)
}

pub fn throw(scope: &mut v8::HandleScope, err: OpError) {
    let e = make_error(scope, &err);
    scope.throw_exception(e);
}

pub fn to_v8<'s>(scope: &mut v8::HandleScope<'s>, v: OpValue) -> v8::Local<'s, v8::Value> {
    match v {
        OpValue::Undefined => v8::undefined(scope).into(),
        OpValue::Bool(b) => v8::Boolean::new(scope, b).into(),
        OpValue::Number(n) => v8::Number::new(scope, n).into(),
        OpValue::String(s) => v8::String::new(scope, &s).unwrap().into(),
        OpValue::Bytes(bytes) => bytes_to_uint8array(scope, bytes).into(),
        OpValue::Object(fields) => {
            let obj = v8::Object::new(scope);
            for (k, val) in fields {
                let key = v8::String::new(scope, k).unwrap();
                let val = to_v8(scope, val);
                obj.set(scope, key.into(), val);
            }
            obj.into()
        }
    }
}

/// Move a Rust `Vec<u8>` into a JS `Uint8Array` without copying the bytes.
pub fn bytes_to_uint8array<'s>(
    scope: &mut v8::HandleScope<'s>,
    bytes: Vec<u8>,
) -> v8::Local<'s, v8::Uint8Array> {
    let len = bytes.len();
    // `new_backing_store_from_boxed_slice` takes ownership of the allocation;
    // V8 frees it through a Rust deleter when the ArrayBuffer is collected.
    let store =
        v8::ArrayBuffer::new_backing_store_from_boxed_slice(bytes.into_boxed_slice()).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
    v8::Uint8Array::new(scope, ab, 0, len).unwrap()
}

/// Queue an async op. When `fut` completes, the event loop settles the JS
/// promise registered under `promise_id`.
pub fn spawn_op(
    scope: &mut v8::HandleScope,
    promise_id: u32,
    fut: impl Future<Output = OpResult> + 'static,
) {
    let pending = pending_ops(scope);
    pending
        .0
        .borrow_mut()
        .push(Box::pin(async move { (promise_id, fut.await) }));
}

static V8_INIT: Once = Once::new();

pub fn init_v8() {
    V8_INIT.call_once(|| {
        v8::V8::set_flags_from_string("--stack-size=2048");
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Outcome of running a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// Uncaught error, already formatted for stderr.
    Error(String),
}

pub struct JsRuntime {
    // NOTE: field order matters: the context must be dropped before the isolate.
    context: v8::Global<v8::Context>,
    state: Rc<RefCell<State>>,
    pending: Rc<PendingOps>,
    isolate: v8::OwnedIsolate,
}

const BOOTSTRAP: &str = include_str!("js/bootstrap.js");

impl JsRuntime {
    pub fn new(permissions: Permissions, args: Vec<String>) -> Self {
        init_v8();
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(promise_reject_callback);
        isolate.set_host_initialize_import_meta_object_callback(import_meta_callback);
        isolate.set_host_import_module_dynamically_callback(dynamic_import_callback);
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 20);

        let state = Rc::new(RefCell::new(State {
            permissions,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            resources: ResourceTable::new(),
            timers: BTreeMap::new(),
            timer_keys: HashMap::new(),
            timer_seq: 0,
            unhandled: Vec::new(),
            modules: ModuleMap::default(),
            op_resolve: None,
            timer_dispatch: None,
            build_error: None,
            inspect: None,
            last_alloc_ptr: 0,
        }));
        let pending = Rc::new(PendingOps::default());
        isolate.set_slot(state.clone());
        isolate.set_slot(pending.clone());

        let context = {
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            let ops_obj = ops::build_ops_object(scope);
            let args_arr = v8::Array::new(scope, args.len() as i32);
            for (i, a) in args.iter().enumerate() {
                let s = v8::String::new(scope, a).unwrap();
                args_arr.set_index(scope, i as u32, s.into());
            }

            let tc = &mut v8::TryCatch::new(scope);
            let code = v8::String::new(tc, BOOTSTRAP).unwrap();
            let name = v8::String::new(tc, "harbor:bootstrap").unwrap();
            let origin = script_origin(tc, name.into(), false);
            let script = v8::Script::compile(tc, code, Some(&origin)).expect("bootstrap compiles");
            let f = script.run(tc).expect("bootstrap runs");
            let f = v8::Local::<v8::Function>::try_from(f).expect("bootstrap is a function");
            let undef = v8::undefined(tc).into();
            let ret = match f.call(tc, undef, &[ops_obj.into(), args_arr.into()]) {
                Some(r) => r,
                None => panic!("bootstrap failed: {}", format_exception(tc)),
            };
            let ret = v8::Local::<v8::Object>::try_from(ret).unwrap();
            let get_fn = |tc: &mut v8::HandleScope, name: &str| {
                let k = v8::String::new(tc, name).unwrap();
                let v = ret.get(tc, k.into()).unwrap();
                let f = v8::Local::<v8::Function>::try_from(v).unwrap();
                v8::Global::new(tc, f)
            };
            let op_resolve = get_fn(tc, "opResolve");
            let timer_dispatch = get_fn(tc, "timerDispatch");
            let build_error = get_fn(tc, "buildError");
            let inspect = get_fn(tc, "inspect");
            {
                let mut s = state.borrow_mut();
                s.op_resolve = Some(op_resolve);
                s.timer_dispatch = Some(timer_dispatch);
                s.build_error = Some(build_error);
                s.inspect = Some(inspect);
            }
            v8::Global::new(tc, context)
        };

        JsRuntime {
            context,
            state,
            pending,
            isolate,
        }
    }

    pub fn state(&self) -> Rc<RefCell<State>> {
        self.state.clone()
    }

    /// Evaluate a classic script; returns `inspect(result)` on success.
    pub fn execute_script(&mut self, name: &str, code: &str) -> Result<String, String> {
        let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
        let tc = &mut v8::TryCatch::new(scope);
        let code = v8::String::new(tc, code).ok_or("source too large")?;
        let name = v8::String::new(tc, name).unwrap();
        let origin = script_origin(tc, name.into(), false);
        let Some(script) = v8::Script::compile(tc, code, Some(&origin)) else {
            return Err(format_exception(tc));
        };
        let Some(v) = script.run(tc) else {
            return Err(format_exception(tc));
        };
        tc.perform_microtask_checkpoint();
        Ok(inspect_value(tc, v))
    }

    /// Load the module graph rooted at `path`, instantiate and evaluate it,
    /// then run the event loop to completion.
    pub async fn run_main_module(&mut self, path: &Path) -> Outcome {
        let main_promise = {
            let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
            self.state.borrow_mut().modules.main = Some(path.to_path_buf());
            match evaluate_module(scope, path) {
                Ok(p) => v8::Global::new(scope, p),
                Err(e) => return Outcome::Error(e),
            }
        };
        let r = self.run_event_loop(Some(&main_promise)).await;
        if let Outcome::Error(_) = r {
            return r;
        }
        let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
        let p = v8::Local::new(scope, &main_promise);
        match p.state() {
            v8::PromiseState::Fulfilled => Outcome::Ok,
            v8::PromiseState::Rejected => {
                let e = p.result(scope);
                Outcome::Error(format!("error: Uncaught {}", describe_error(scope, e)))
            }
            v8::PromiseState::Pending => Outcome::Error(
                "error: Top-level await promise never resolved (event loop is empty)".into(),
            ),
        }
    }

    /// Run timers and async ops until there is nothing left to do.
    pub async fn run_event_loop(&mut self, main: Option<&v8::Global<v8::Promise>>) -> Outcome {
        loop {
            // 1. Expired timers, in (deadline, insertion) order.
            if let Err(e) = self.fire_timers() {
                return Outcome::Error(e);
            }
            // 2. Every op future that is already complete.
            loop {
                let ready = {
                    let mut p = self.pending.0.borrow_mut();
                    futures::FutureExt::now_or_never(p.next())
                };
                match ready {
                    Some(Some((id, res))) => {
                        if let Err(e) = self.complete_op(id, res) {
                            return Outcome::Error(e);
                        }
                    }
                    _ => break,
                }
            }
            // 3. Rejections nobody handled during this turn.
            if let Some(e) = self.check_main_and_unhandled(main) {
                return Outcome::Error(e);
            }

            let has_ops = !self.pending.0.borrow().is_empty();
            let next_timer = self.state.borrow().timers.keys().next().map(|k| k.0);
            if !has_ops && next_timer.is_none() {
                return Outcome::Ok;
            }

            // 4. Sleep until an op completes or the next timer is due.
            let pending = self.pending.clone();
            let ops_fut = futures::future::poll_fn(|cx| {
                let mut p = pending.0.borrow_mut();
                if p.is_empty() {
                    return Poll::Pending;
                }
                p.poll_next_unpin(cx)
            });
            let deadline =
                next_timer.unwrap_or_else(|| Instant::now() + Duration::from_secs(86400 * 365));
            let done = tokio::select! {
                biased;
                r = ops_fut => r,
                _ = tokio::time::sleep_until(deadline.into()) => None,
            };
            if let Some((id, res)) = done
                && let Err(e) = self.complete_op(id, res)
            {
                return Outcome::Error(e);
            }
        }
    }

    fn fire_timers(&mut self) -> Result<(), String> {
        loop {
            let now = Instant::now();
            let due = {
                let mut s = self.state.borrow_mut();
                match s.timers.first_key_value() {
                    Some((k, _)) if k.0 <= now => {
                        let (k, id) = s.timers.pop_first().unwrap();
                        let _ = k;
                        s.timer_keys.remove(&id);
                        Some(id)
                    }
                    _ => None,
                }
            };
            let Some(id) = due else { return Ok(()) };
            let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
            let f = self.state.borrow().timer_dispatch.clone().unwrap();
            let f = v8::Local::new(scope, f);
            let tc = &mut v8::TryCatch::new(scope);
            let idv = v8::Integer::new_from_unsigned(tc, id).into();
            let undef = v8::undefined(tc).into();
            if f.call(tc, undef, &[idv]).is_none() {
                return Err(format!("error: Uncaught {}", format_exception_inner(tc)));
            }
            tc.perform_microtask_checkpoint();
            if tc.has_caught() {
                return Err(format!("error: Uncaught {}", format_exception_inner(tc)));
            }
        }
    }

    fn complete_op(&mut self, id: u32, res: OpResult) -> Result<(), String> {
        let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
        let f = self.state.borrow().op_resolve.clone().unwrap();
        let f = v8::Local::new(scope, f);
        let tc = &mut v8::TryCatch::new(scope);
        let idv: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(tc, id).into();
        let (ok, val) = match res {
            Ok(v) => (true, to_v8(tc, v)),
            Err(e) => (false, make_error(tc, &e)),
        };
        let okv = v8::Boolean::new(tc, ok).into();
        let undef = v8::undefined(tc).into();
        if f.call(tc, undef, &[idv, okv, val]).is_none() {
            return Err(format!("error: Uncaught {}", format_exception_inner(tc)));
        }
        tc.perform_microtask_checkpoint();
        Ok(())
    }

    fn check_main_and_unhandled(
        &mut self,
        main: Option<&v8::Global<v8::Promise>>,
    ) -> Option<String> {
        let scope = &mut v8::HandleScope::with_context(&mut self.isolate, &self.context);
        if let Some(m) = main {
            let p = v8::Local::new(scope, m);
            if p.state() == v8::PromiseState::Rejected {
                let e = p.result(scope);
                return Some(format!("error: Uncaught {}", describe_error(scope, e)));
            }
        }
        let list = std::mem::take(&mut self.state.borrow_mut().unhandled);
        for (promise, value) in list {
            if let Some(m) = main
                && promise == *m
            {
                continue;
            }
            let v = v8::Local::new(scope, value);
            return Some(format!(
                "error: Uncaught (in promise) {}",
                describe_error(scope, v)
            ));
        }
        None
    }

    /// For the REPL: evaluate, then drive the loop so promises settle.
    pub async fn eval_and_settle(&mut self, code: &str) -> Result<String, String> {
        let r = self.execute_script("repl", code)?;
        match self.run_event_loop(None).await {
            Outcome::Ok => Ok(r),
            Outcome::Error(e) => Err(e),
        }
    }
}

pub fn script_origin<'s>(
    scope: &mut v8::HandleScope<'s>,
    name: v8::Local<'s, v8::Value>,
    is_module: bool,
) -> v8::ScriptOrigin<'s> {
    v8::ScriptOrigin::new(
        scope, name, 0, 0, false, 0, None, false, false, is_module, None,
    )
}

fn inspect_value(scope: &mut v8::HandleScope, v: v8::Local<v8::Value>) -> String {
    let f = state(scope).borrow().inspect.clone();
    if let Some(f) = f {
        let f = v8::Local::new(scope, f);
        let undef = v8::undefined(scope).into();
        if let Some(s) = f.call(scope, undef, &[v]) {
            return s.to_rust_string_lossy(scope);
        }
    }
    v.to_rust_string_lossy(scope)
}

/// `stack` for Error objects (includes "Name: message" + frames), else inspect().
pub fn describe_error(scope: &mut v8::HandleScope, e: v8::Local<v8::Value>) -> String {
    if e.is_native_error() || e.is_object() {
        let obj = e.to_object(scope).unwrap();
        let k = v8::String::new(scope, "stack").unwrap();
        if let Some(stack) = obj.get(scope, k.into())
            && stack.is_string()
        {
            return stack.to_rust_string_lossy(scope);
        }
    }
    inspect_value(scope, e)
}

fn format_exception_inner(tc: &mut v8::TryCatch<v8::HandleScope>) -> String {
    match tc.exception() {
        Some(e) => {
            let mut s = describe_error(tc, e);
            // Syntax errors have no useful stack; add the location.
            if let Some(msg) = tc.message()
                && !s.contains("\n    at ")
            {
                let file = msg
                    .get_script_resource_name(tc)
                    .map(|n| n.to_rust_string_lossy(tc))
                    .unwrap_or_default();
                let line = msg.get_line_number(tc).unwrap_or(0);
                s = format!("{s}\n    at {file}:{line}");
            }
            s
        }
        None => "execution terminated".into(),
    }
}

pub fn format_exception(tc: &mut v8::TryCatch<v8::HandleScope>) -> String {
    format!("error: Uncaught {}", format_exception_inner(tc))
}

// ---------------------------------------------------------------- modules

fn module_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Load (read + transpile + compile) a module and, recursively, its static
/// dependencies. Modules are inserted into the map *before* their
/// dependencies are loaded, which is what makes import cycles terminate.
fn load_module_graph<'s>(
    scope: &mut v8::HandleScope<'s>,
    path: &Path,
) -> Result<v8::Local<'s, v8::Module>, String> {
    if let Some(g) = state(scope).borrow().modules.by_path.get(path) {
        return Ok(v8::Local::new(scope, g));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("error: Module not found \"{}\": {e}", module_url(path)))?;
    let js = loader::to_javascript(path, raw).map_err(|e| format!("error: {e}"))?;

    let tc = &mut v8::TryCatch::new(scope);
    let code = v8::String::new(tc, &js).ok_or("module too large")?;
    let name = v8::String::new(tc, &module_url(path)).unwrap();
    let origin = script_origin(tc, name.into(), true);
    let mut source = v8::script_compiler::Source::new(code, Some(&origin));
    let Some(module) = v8::script_compiler::compile_module(tc, &mut source) else {
        return Err(format_exception(tc));
    };
    {
        let st = state(tc);
        let mut st = st.borrow_mut();
        let g = v8::Global::new(tc, module);
        st.modules.by_path.insert(path.to_path_buf(), g.clone());
        st.modules
            .by_hash
            .entry(module.get_identity_hash().get())
            .or_default()
            .push((g, path.to_path_buf()));
    }
    let requests = module.get_module_requests();
    for i in 0..requests.length() {
        let req = requests.get(tc, i).unwrap();
        let req = v8::Local::<v8::ModuleRequest>::try_from(req).unwrap();
        let spec = req.get_specifier().to_rust_string_lossy(tc);
        let dep = loader::resolve(&spec, path)
            .map_err(|e| format!("error: {e}\n    at {}", module_url(path)))?;
        load_module_graph(tc, &dep)?;
    }
    Ok(module)
}

fn resolve_module_callback<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _attrs: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: V8 invokes this callback synchronously on the isolate's thread
    // while `context` is entered, which is the precondition of CallbackScope.
    let scope = &mut unsafe { v8::CallbackScope::new(context) };
    let st = state(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let referrer_path = st.borrow().modules.path_of(scope, referrer)?;
    let path = match loader::resolve(&spec, &referrer_path) {
        Ok(p) => p,
        Err(e) => {
            let msg = v8::String::new(scope, &e.to_string()).unwrap();
            let ex = v8::Exception::type_error(scope, msg);
            scope.throw_exception(ex);
            return None;
        }
    };
    let g = st.borrow().modules.by_path.get(&path).cloned()?;
    Some(v8::Local::new(scope, g))
}

/// Load + instantiate + evaluate. Returns the evaluation promise.
fn evaluate_module<'s>(
    scope: &mut v8::HandleScope<'s>,
    path: &Path,
) -> Result<v8::Local<'s, v8::Promise>, String> {
    let module = load_module_graph(scope, path)?;
    let tc = &mut v8::TryCatch::new(scope);
    if module.get_status() == v8::ModuleStatus::Uninstantiated
        && module
            .instantiate_module(tc, resolve_module_callback)
            .is_none()
    {
        return Err(format_exception(tc));
    }
    let Some(result) = module.evaluate(tc) else {
        return Err(format_exception(tc));
    };
    tc.perform_microtask_checkpoint();
    let promise = v8::Local::<v8::Promise>::try_from(result)
        .map_err(|_| "module evaluation did not return a promise".to_string())?;
    Ok(promise)
}

extern "C" fn import_meta_callback(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    // SAFETY: called synchronously by V8 on the isolate thread with `context` entered.
    let scope = &mut unsafe { v8::CallbackScope::new(context) };
    let st = state(scope);
    let (path, main) = {
        let s = st.borrow();
        let p = s.modules.path_of(scope, module);
        let main = p.is_some() && p == s.modules.main;
        (p, main)
    };
    if let Some(path) = path {
        let k = v8::String::new(scope, "url").unwrap();
        let v = v8::String::new(scope, &module_url(&path)).unwrap();
        meta.set(scope, k.into(), v.into());
        let k = v8::String::new(scope, "main").unwrap();
        let v = v8::Boolean::new(scope, main);
        meta.set(scope, k.into(), v.into());
    }
}

fn dynamic_import_callback<'s>(
    scope: &mut v8::HandleScope<'s>,
    _host_defined_options: v8::Local<'s, v8::Data>,
    resource_name: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    _attrs: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let referrer = resource_name.to_rust_string_lossy(scope);
    let referrer = match referrer.strip_prefix("file://") {
        Some(p) => PathBuf::from(p),
        None => state(scope).borrow().cwd.join("__repl__"),
    };
    let reject = |scope: &mut v8::HandleScope<'s>, msg: String| {
        let m = v8::String::new(scope, &msg).unwrap();
        let e = v8::Exception::type_error(scope, m);
        resolver.reject(scope, e);
    };
    let path = match loader::resolve(&spec, &referrer) {
        Ok(p) => p,
        Err(e) => {
            reject(scope, e.to_string());
            return Some(promise);
        }
    };
    match evaluate_module(scope, &path) {
        Err(e) => reject(scope, e),
        Ok(eval_promise) => {
            let st = state(scope);
            let g = st.borrow().modules.by_path.get(&path).cloned().unwrap();
            let module = v8::Local::new(scope, g);
            let ns = module.get_module_namespace();
            let ns = v8::Local::new(scope, ns);
            // Resolve with the namespace once evaluation (incl. TLA) finishes.
            let then = v8::Function::builder(
                |_: &mut v8::HandleScope,
                 args: v8::FunctionCallbackArguments,
                 mut rv: v8::ReturnValue| {
                    rv.set(args.data());
                },
            )
            .data(ns)
            .build(scope)?;
            let chained = eval_promise.then(scope, then)?;
            resolver.resolve(scope, chained.into());
        }
    }
    Some(promise)
}

extern "C" fn promise_reject_callback(msg: v8::PromiseRejectMessage) {
    // SAFETY: V8 calls this synchronously on the isolate thread.
    let scope = &mut unsafe { v8::CallbackScope::new(&msg) };
    let st = state(scope);
    let promise = msg.get_promise();
    match msg.get_event() {
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            let value = msg
                .get_value()
                .unwrap_or_else(|| v8::undefined(scope).into());
            let p = v8::Global::new(scope, promise);
            let v = v8::Global::new(scope, value);
            st.borrow_mut().unhandled.push((p, v));
        }
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            let p = v8::Global::new(scope, promise);
            st.borrow_mut().unhandled.retain(|(q, _)| *q != p);
        }
        _ => {}
    }
}
