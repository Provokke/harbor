//! Rust functions callable from JS ("ops").
//!
//! Sync ops return a value or throw. Async ops take a promise id as their
//! first argument, push a future via [`spawn_op`] and return immediately;
//! the event loop settles the promise when the future completes.
//!
//! # The zero-copy boundary
//!
//! Byte buffers cross the boundary without copying in both directions:
//!
//! * Rust -> JS: a `Vec<u8>` is turned into the backing store of a new
//!   `ArrayBuffer` ([`bytes_to_uint8array`]); V8 adopts the allocation.
//! * JS -> Rust: an op receiving a `Uint8Array` takes the view's backing
//!   store (a ref-counted `SharedRef<BackingStore>`) and builds a slice over
//!   `[data + byte_offset, len)`. See [`BufView`] for the safety argument.

use std::cell::RefCell;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::resources::Resource;
use crate::runtime::{
    OpError, OpResult, OpValue, bytes_to_uint8array, spawn_op, state, throw, to_v8,
};

macro_rules! register_ops {
    ($scope:expr, $obj:expr, [$($name:ident),* $(,)?]) => {
        $(
            let key = v8::String::new($scope, stringify!($name)).unwrap();
            let func = v8::Function::new($scope, $name).unwrap();
            $obj.set($scope, key.into(), func.into());
        )*
    };
}

/// Build the object of op functions handed to the bootstrap script.
pub fn build_ops_object<'s>(scope: &mut v8::HandleScope<'s>) -> v8::Local<'s, v8::Object> {
    let obj = v8::Object::new(scope);
    register_ops!(
        scope,
        obj,
        [
            op_print,
            op_timer_start,
            op_timer_cancel,
            op_now,
            op_exit,
            op_cwd,
            op_env_get,
            op_env_set,
            op_env_delete,
            op_env_to_object,
            op_read_file,
            op_read_text_file,
            op_read_text_file_sync,
            op_write_file,
            op_write_text_file,
            op_write_text_file_sync,
            op_remove,
            op_stat,
            op_mkdir,
            op_open,
            op_read,
            op_write,
            op_seek,
            op_close,
            op_resources,
            op_listen,
            op_accept,
            op_connect,
            op_run,
            op_encode,
            op_decode,
            op_alloc,
            op_last_alloc_addr,
            op_buf_addr,
            op_buf_fill,
            op_buf_sum,
            op_noop,
            op_noop_async,
        ]
    );
    obj
}

// ------------------------------------------------------------ arg helpers

fn arg_string(scope: &mut v8::HandleScope, args: &v8::FunctionCallbackArguments, i: i32) -> String {
    args.get(i).to_rust_string_lossy(scope)
}

fn arg_u32(
    scope: &mut v8::HandleScope,
    args: &v8::FunctionCallbackArguments,
    i: i32,
) -> Result<u32, OpError> {
    args.get(i)
        .uint32_value(scope)
        .ok_or_else(|| OpError::type_error("expected an unsigned integer"))
}

fn arg_f64(scope: &mut v8::HandleScope, args: &v8::FunctionCallbackArguments, i: i32) -> f64 {
    args.get(i).number_value(scope).unwrap_or(0.0)
}

/// A JS `Uint8Array` viewed from Rust without copying.
///
/// Holding `_store` (a ref-counted handle to the backing store) guarantees
/// the memory stays allocated for as long as the `BufView` lives, even if JS
/// drops every reference to the array or the GC runs.
pub struct BufView {
    _store: v8::SharedRef<v8::BackingStore>,
    ptr: *mut u8,
    len: usize,
}

impl BufView {
    pub fn from_value(v: v8::Local<v8::Value>) -> Result<Self, OpError> {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v)
            .map_err(|_| OpError::type_error("expected a Uint8Array"))?;
        let store = view
            .get_backing_store()
            .ok_or_else(|| OpError::type_error("buffer has no backing store"))?;
        let len = view.byte_length();
        let ptr = match store.data() {
            // SAFETY: byte_offset + byte_length <= store.byte_length() is an
            // invariant V8 maintains for every live ArrayBufferView.
            Some(p) => unsafe { (p.as_ptr() as *mut u8).add(view.byte_offset()) },
            None => std::ptr::NonNull::<u8>::dangling().as_ptr(), // zero-length buffer
        };
        Ok(BufView {
            _store: store,
            ptr,
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    /// # Safety
    /// The caller must ensure no JS code runs (and no other slice over the
    /// same memory is alive) while the returned slice is in use. Sync ops
    /// satisfy this trivially: JS is blocked inside the op call, and ops
    /// never call back into JS while holding a slice. Resizable /
    /// detachable buffers are not used by the runtime, so the length cannot
    /// shrink underneath us.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr/len describe live memory kept alive by `_store`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// # Safety
    /// As for [`BufView::as_mut_slice`], but shared.
    pub unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: see as_mut_slice.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// Raw pointer that may be sent to a blocking thread. Only constructed for
/// async file I/O whose JS contract is "do not touch the buffer until the
/// promise settles" (the same contract as Deno/Node `read(buf)`).
struct SendPtr(*mut u8, usize);
// SAFETY: the pointee is plain bytes kept alive by a BufView owned by the
// awaiting future; access is exclusive per the documented JS contract above.
unsafe impl Send for SendPtr {}

macro_rules! try_op {
    ($scope:expr, $e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => {
                throw($scope, OpError::from(err));
                return;
            }
        }
    };
}

fn promise_id(scope: &mut v8::HandleScope, args: &v8::FunctionCallbackArguments) -> u32 {
    args.get(0).uint32_value(scope).unwrap_or(0)
}

/// Run a sync op that produces an OpValue; set the return value or throw.
fn finish(scope: &mut v8::HandleScope, rv: &mut v8::ReturnValue, r: OpResult) {
    match r {
        Ok(v) => {
            let v = to_v8(scope, v);
            rv.set(v);
        }
        Err(e) => throw(scope, e),
    }
}

// ------------------------------------------------------------ basics

fn op_print(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let s = arg_string(scope, &args, 0);
    let is_err = args.get(1).is_true();
    if is_err {
        let mut e = std::io::stderr().lock();
        let _ = e.write_all(s.as_bytes());
    } else {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(s.as_bytes());
    }
}

fn op_timer_start(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = try_op!(scope, arg_u32(scope, &args, 0));
    let ms = arg_f64(scope, &args, 1).max(0.0);
    let st = state(scope);
    let mut s = st.borrow_mut();
    s.timer_seq += 1;
    let key = (
        Instant::now() + Duration::from_secs_f64(ms / 1000.0),
        s.timer_seq,
    );
    s.timers.insert(key, id);
    s.timer_keys.insert(id, key);
}

fn op_timer_cancel(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = try_op!(scope, arg_u32(scope, &args, 0));
    let st = state(scope);
    let mut s = st.borrow_mut();
    if let Some(k) = s.timer_keys.remove(&id) {
        s.timers.remove(&k);
    }
}

fn op_now(
    _scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    rv.set_double(start.elapsed().as_secs_f64() * 1000.0);
}

fn op_exit(scope: &mut v8::HandleScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let code = args.get(0).int32_value(scope).unwrap_or(0);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code);
}

fn op_cwd(
    scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let st = state(scope);
    let cwd = st.borrow().cwd.clone();
    try_op!(scope, st.borrow().permissions.check_read(&cwd, &cwd));
    let s = v8::String::new(scope, &cwd.to_string_lossy()).unwrap();
    rv.set(s.into());
}

// ------------------------------------------------------------ env

fn op_env_get(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let key = arg_string(scope, &args, 0);
    try_op!(scope, state(scope).borrow().permissions.check_env(&key));
    match std::env::var(&key) {
        Ok(v) => {
            let s = v8::String::new(scope, &v).unwrap();
            rv.set(s.into());
        }
        Err(_) => rv.set_undefined(),
    }
}

fn op_env_set(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let key = arg_string(scope, &args, 0);
    let val = arg_string(scope, &args, 1);
    try_op!(scope, state(scope).borrow().permissions.check_env(&key));
    if key.is_empty() || key.contains('=') || key.contains('\0') || val.contains('\0') {
        throw(scope, OpError::type_error("invalid environment variable"));
        return;
    }
    // SAFETY: the runtime is single-threaded with respect to JS; tokio's
    // blocking pool never reads the environment. (set_var is unsafe in
    // edition 2024 because of concurrent getenv in other threads.)
    unsafe { std::env::set_var(key, val) };
}

fn op_env_delete(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let key = arg_string(scope, &args, 0);
    try_op!(scope, state(scope).borrow().permissions.check_env(&key));
    // SAFETY: see op_env_set.
    unsafe { std::env::remove_var(key) };
}

fn op_env_to_object(
    scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    try_op!(scope, state(scope).borrow().permissions.check_env_all());
    let obj = v8::Object::new(scope);
    for (k, v) in std::env::vars() {
        let k = v8::String::new(scope, &k).unwrap();
        let v = v8::String::new(scope, &v).unwrap();
        obj.set(scope, k.into(), v.into());
    }
    rv.set(obj.into());
}

// ------------------------------------------------------------ fs

fn check_read(scope: &mut v8::HandleScope, p: &str) -> Result<PathBuf, OpError> {
    let st = state(scope);
    let s = st.borrow();
    Ok(s.permissions.check_read(std::path::Path::new(p), &s.cwd)?)
}

fn check_write(scope: &mut v8::HandleScope, p: &str) -> Result<PathBuf, OpError> {
    let st = state(scope);
    let s = st.borrow();
    Ok(s.permissions.check_write(std::path::Path::new(p), &s.cwd)?)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, OpError> + Send + 'static,
) -> Result<T, OpError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| OpError::new("Error", e.to_string()))?
}

fn op_read_file(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let path = check_read(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        let bytes = blocking(move || Ok(std::fs::read(path)?)).await?;
        Ok(OpValue::Bytes(bytes))
    });
}

fn op_read_text_file(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let path = check_read(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        let s = blocking(move || Ok(std::fs::read_to_string(path)?)).await?;
        Ok(OpValue::String(s))
    });
}

fn op_read_text_file_sync(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let p = arg_string(scope, &args, 0);
    let r =
        check_read(scope, &p).and_then(|path| Ok(OpValue::String(std::fs::read_to_string(path)?)));
    finish(scope, &mut rv, r);
}

fn write_opts(scope: &mut v8::HandleScope, v: v8::Local<v8::Value>) -> bool {
    // returns `append`
    if !v.is_object() {
        return false;
    }
    if let Some(o) = v.to_object(scope) {
        let k = v8::String::new(scope, "append").unwrap();
        if let Some(a) = o.get(scope, k.into()) {
            return a.is_true();
        }
    }
    false
}

fn write_all(path: PathBuf, data: &[u8], append: bool) -> Result<(), OpError> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

fn op_write_file(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let append = write_opts(scope, args.get(3));
    let path = check_write(scope, &p);
    let buf = BufView::from_value(args.get(2));
    spawn_op(scope, id, async move {
        let path = path?;
        let buf = buf?;
        let raw = SendPtr(buf.ptr, buf.len);
        blocking(move || {
            let raw = raw;
            // SAFETY: memory kept alive by `buf` (awaited below, so it
            // outlives this closure); JS contract: buffer untouched until
            // the promise settles. We only read.
            let data = unsafe { std::slice::from_raw_parts(raw.0, raw.1) };
            write_all(path, data, append)
        })
        .await?;
        drop(buf);
        Ok(OpValue::Undefined)
    });
}

fn op_write_text_file(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let data = arg_string(scope, &args, 2);
    let append = write_opts(scope, args.get(3));
    let path = check_write(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        blocking(move || write_all(path, data.as_bytes(), append)).await?;
        Ok(OpValue::Undefined)
    });
}

fn op_write_text_file_sync(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let p = arg_string(scope, &args, 0);
    let data = arg_string(scope, &args, 1);
    let r = check_write(scope, &p)
        .and_then(|path| write_all(path, data.as_bytes(), false).map(|_| OpValue::Undefined));
    finish(scope, &mut rv, r);
}

fn op_remove(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let recursive = args.get(2).is_true();
    let path = check_write(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        blocking(move || {
            let md = std::fs::symlink_metadata(&path)?;
            if md.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(&path)?
                } else {
                    std::fs::remove_dir(&path)?
                }
            } else {
                std::fs::remove_file(&path)?
            }
            Ok(())
        })
        .await?;
        Ok(OpValue::Undefined)
    });
}

fn op_mkdir(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let recursive = args.get(2).is_true();
    let path = check_write(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        blocking(move || {
            if recursive {
                std::fs::create_dir_all(path)?
            } else {
                std::fs::create_dir(path)?
            }
            Ok(())
        })
        .await?;
        Ok(OpValue::Undefined)
    });
}

fn op_stat(scope: &mut v8::HandleScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = promise_id(scope, &args);
    let p = arg_string(scope, &args, 1);
    let path = check_read(scope, &p);
    spawn_op(scope, id, async move {
        let path = path?;
        let md = blocking(move || Ok(std::fs::metadata(path)?)).await?;
        Ok(OpValue::Object(vec![
            ("size", OpValue::Number(md.len() as f64)),
            ("isFile", OpValue::Bool(md.is_file())),
            ("isDirectory", OpValue::Bool(md.is_dir())),
            ("isSymlink", OpValue::Bool(md.file_type().is_symlink())),
        ]))
    });
}

// ------------------------------------------------------------ resources

pub struct FileResource {
    file: Arc<Mutex<std::fs::File>>,
}

impl Resource for FileResource {
    fn name(&self) -> &'static str {
        "file"
    }
}

/// Lets `close(rid)` abort ops that are blocked on a resource (e.g. a
/// pending `accept`), so the event loop does not wait on them forever.
#[derive(Default)]
pub struct CancelHandle {
    closed: std::cell::Cell<bool>,
    notify: tokio::sync::Notify,
}

impl CancelHandle {
    fn cancel(&self) {
        self.closed.set(true);
        self.notify.notify_waiters();
    }

    /// Run `fut` unless/until the resource is closed.
    async fn run<T>(
        &self,
        fut: impl std::future::Future<Output = Result<T, OpError>>,
    ) -> Result<T, OpError> {
        let closed = || OpError::new("BadResource", "resource closed while operation was pending");
        if self.closed.get() {
            return Err(closed());
        }
        tokio::select! {
            r = fut => r,
            _ = self.notify.notified() => Err(closed()),
        }
    }
}

pub struct TcpStreamResource {
    rd: tokio::sync::Mutex<tokio::net::tcp::OwnedReadHalf>,
    wr: tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>,
    cancel: CancelHandle,
}

impl Resource for TcpStreamResource {
    fn name(&self) -> &'static str {
        "tcpStream"
    }
    fn close(self: Rc<Self>) {
        self.cancel.cancel();
    }
}

pub struct TcpListenerResource {
    listener: tokio::net::TcpListener,
    cancel: CancelHandle,
}

impl Resource for TcpListenerResource {
    fn name(&self) -> &'static str {
        "tcpListener"
    }
    fn close(self: Rc<Self>) {
        self.cancel.cancel();
    }
}

fn op_open(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let p = arg_string(scope, &args, 0);
    let opts = args.get(1);
    let opts = if opts.is_object() {
        opts
    } else {
        v8::Object::new(scope).into()
    };
    let flag = |scope: &mut v8::HandleScope, name: &str| -> bool {
        opts.to_object(scope)
            .and_then(|o| {
                let k = v8::String::new(scope, name).unwrap();
                o.get(scope, k.into())
            })
            .is_some_and(|v| v.is_true())
    };
    let write = flag(scope, "write");
    let create = flag(scope, "create");
    let truncate = flag(scope, "truncate");
    let append = flag(scope, "append");
    let read = flag(scope, "read") || !(write || append);
    let mut path = None;
    if read {
        path = Some(try_op!(scope, check_read(scope, &p)));
    }
    if write || append || create || truncate {
        path = Some(try_op!(scope, check_write(scope, &p)));
    }
    let file = try_op!(
        scope,
        std::fs::OpenOptions::new()
            .read(read)
            .write(write)
            .append(append)
            .create(create)
            .truncate(truncate)
            .open(path.unwrap())
    );
    let rid = state(scope).borrow_mut().resources.add(FileResource {
        file: Arc::new(Mutex::new(file)),
    });
    rv.set_uint32(rid);
}

enum Stream {
    File(Rc<FileResource>),
    Tcp(Rc<TcpStreamResource>),
}

fn get_stream(scope: &mut v8::HandleScope, rid: u32) -> Result<Stream, OpError> {
    let st = state(scope);
    let s = st.borrow();
    if let Ok(f) = s.resources.get::<FileResource>(rid) {
        return Ok(Stream::File(f));
    }
    Ok(Stream::Tcp(s.resources.get::<TcpStreamResource>(rid)?))
}

/// `read(rid, buf) -> Promise<number>`; 0 means EOF. Bytes are written
/// straight into the JS buffer's memory (zero-copy).
fn op_read(scope: &mut v8::HandleScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = promise_id(scope, &args);
    let rid = arg_u32(scope, &args, 1);
    let res = rid.and_then(|rid| get_stream(scope, rid));
    let buf = BufView::from_value(args.get(2));
    spawn_op(scope, id, async move {
        let mut buf = buf?;
        let n = match res? {
            Stream::File(f) => {
                let raw = SendPtr(buf.ptr, buf.len);
                let file = f.file.clone();
                let n = blocking(move || {
                    let raw = raw;
                    // SAFETY: see SendPtr; `buf` outlives this closure because
                    // the enclosing future awaits it before dropping `buf`.
                    let slice = unsafe { std::slice::from_raw_parts_mut(raw.0, raw.1) };
                    Ok(file.lock().unwrap().read(slice)?)
                })
                .await?;
                drop(buf);
                n
            }
            Stream::Tcp(t) => {
                let mut rd = t.rd.lock().await;
                let read = futures::future::poll_fn(|cx| {
                    // SAFETY: the slice only lives for this poll call, during
                    // which no JS runs (we are on the JS thread, inside the
                    // event loop, outside any V8 call).
                    let slice = unsafe { buf.as_mut_slice() };
                    let mut rb = ReadBuf::new(slice);
                    std::pin::Pin::new(&mut *rd)
                        .poll_read(cx, &mut rb)
                        .map_ok(|_| rb.filled().len())
                });
                t.cancel.run(async { Ok(read.await?) }).await?
            }
        };
        Ok(OpValue::Number(n as f64))
    });
}

/// `write(rid, buf) -> Promise<number>`; writes the whole buffer.
fn op_write(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let rid = arg_u32(scope, &args, 1);
    let res = rid.and_then(|rid| get_stream(scope, rid));
    let buf = BufView::from_value(args.get(2));
    spawn_op(scope, id, async move {
        let buf = buf?;
        let len = buf.len();
        match res? {
            Stream::File(f) => {
                let raw = SendPtr(buf.ptr, buf.len);
                let file = f.file.clone();
                blocking(move || {
                    let raw = raw;
                    // SAFETY: as in op_read; read-only access.
                    let slice = unsafe { std::slice::from_raw_parts(raw.0, raw.1) };
                    Ok(file.lock().unwrap().write_all(slice)?)
                })
                .await?;
                drop(buf);
            }
            Stream::Tcp(t) => {
                let mut wr = t.wr.lock().await;
                let mut off = 0;
                while off < len {
                    let n = futures::future::poll_fn(|cx| {
                        // SAFETY: slice lives only within this poll; see op_read.
                        let slice = unsafe { &buf.as_slice()[off..] };
                        std::pin::Pin::new(&mut *wr).poll_write(cx, slice)
                    })
                    .await?;
                    if n == 0 {
                        return Err(OpError::new("Error", "write returned 0"));
                    }
                    off += n;
                }
            }
        }
        Ok(OpValue::Number(len as f64))
    });
}

fn op_seek(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let rid = try_op!(scope, arg_u32(scope, &args, 0));
    let off = arg_f64(scope, &args, 1) as u64;
    let f = try_op!(
        scope,
        state(scope).borrow().resources.get::<FileResource>(rid)
    );
    let pos = try_op!(
        scope,
        f.file.lock().unwrap().seek(std::io::SeekFrom::Start(off))
    );
    rv.set_double(pos as f64);
}

fn op_close(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let rid = try_op!(scope, arg_u32(scope, &args, 0));
    let st = state(scope);
    let r = st.borrow_mut().resources.close(rid);
    try_op!(scope, r);
}

fn op_resources(
    scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let entries = state(scope).borrow().resources.entries();
    let obj = v8::Object::new(scope);
    for (rid, name) in entries {
        let k = v8::Integer::new_from_unsigned(scope, rid);
        let v = v8::String::new(scope, name).unwrap();
        obj.set(scope, k.into(), v.into());
    }
    rv.set(obj.into());
}

// ------------------------------------------------------------ net

fn addr_obj(a: std::net::SocketAddr) -> OpValue {
    OpValue::Object(vec![
        ("hostname", OpValue::String(a.ip().to_string())),
        ("port", OpValue::Number(a.port() as f64)),
    ])
}

fn op_listen(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let host = arg_string(scope, &args, 0);
    let port = try_op!(scope, arg_u32(scope, &args, 1)) as u16;
    try_op!(
        scope,
        state(scope)
            .borrow()
            .permissions
            .check_net(&host, Some(port))
    );
    let std_listener = try_op!(scope, std::net::TcpListener::bind((host.as_str(), port)));
    try_op!(scope, std_listener.set_nonblocking(true));
    let addr = try_op!(scope, std_listener.local_addr());
    let listener = try_op!(scope, tokio::net::TcpListener::from_std(std_listener));
    let rid = state(scope)
        .borrow_mut()
        .resources
        .add(TcpListenerResource {
            listener,
            cancel: CancelHandle::default(),
        });
    let r = Ok(OpValue::Object(vec![
        ("rid", OpValue::Number(rid as f64)),
        ("addr", addr_obj(addr)),
    ]));
    finish(scope, &mut rv, r);
}

fn add_stream(
    st: &Rc<RefCell<crate::runtime::State>>,
    s: tokio::net::TcpStream,
) -> Result<OpValue, OpError> {
    let local = s.local_addr()?;
    let remote = s.peer_addr()?;
    let (rd, wr) = s.into_split();
    let rid = st.borrow_mut().resources.add(TcpStreamResource {
        rd: tokio::sync::Mutex::new(rd),
        wr: tokio::sync::Mutex::new(wr),
        cancel: CancelHandle::default(),
    });
    Ok(OpValue::Object(vec![
        ("rid", OpValue::Number(rid as f64)),
        ("localAddr", addr_obj(local)),
        ("remoteAddr", addr_obj(remote)),
    ]))
}

fn op_accept(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let st = state(scope);
    let l = arg_u32(scope, &args, 1)
        .and_then(|rid| Ok(st.borrow().resources.get::<TcpListenerResource>(rid)?));
    spawn_op(scope, id, async move {
        let l = l?;
        let (s, _) = l
            .cancel
            .run(async { Ok(l.listener.accept().await?) })
            .await?;
        add_stream(&st, s)
    });
}

fn op_connect(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    let host = arg_string(scope, &args, 1);
    let port = arg_u32(scope, &args, 2).map(|p| p as u16);
    let st = state(scope);
    let check = port
        .clone()
        .and_then(|port| Ok(st.borrow().permissions.check_net(&host, Some(port))?));
    spawn_op(scope, id, async move {
        check?;
        let s = tokio::net::TcpStream::connect((host.as_str(), port?)).await?;
        add_stream(&st, s)
    });
}

// ------------------------------------------------------------ subprocess

fn op_run(scope: &mut v8::HandleScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let id = promise_id(scope, &args);
    let mut cmd: Vec<String> = Vec::new();
    if let Ok(arr) = v8::Local::<v8::Array>::try_from(args.get(1)) {
        for i in 0..arr.length() {
            let v = arr.get_index(scope, i).unwrap();
            cmd.push(v.to_rust_string_lossy(scope));
        }
    }
    let check = match cmd.first() {
        None => Err(OpError::type_error("run: cmd must be a non-empty array")),
        Some(c) => state(scope)
            .borrow()
            .permissions
            .check_run(c)
            .map_err(OpError::from),
    };
    spawn_op(scope, id, async move {
        check?;
        let out = tokio::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        Ok(OpValue::Object(vec![
            (
                "code",
                OpValue::Number(out.status.code().unwrap_or(-1) as f64),
            ),
            ("success", OpValue::Bool(out.status.success())),
            ("stdout", OpValue::Bytes(out.stdout)),
            ("stderr", OpValue::Bytes(out.stderr)),
        ]))
    });
}

// ------------------------------------------------------------ encoding

fn op_encode(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let s = args.get(0).to_rust_string_lossy(scope);
    let u8 = bytes_to_uint8array(scope, s.into_bytes());
    rv.set(u8.into());
}

fn op_decode(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let buf = try_op!(scope, BufView::from_value(args.get(0)));
    let fatal = args.get(1).is_true();
    // SAFETY: sync op; no JS runs while `bytes` is alive.
    let bytes = unsafe { buf.as_slice() };
    let s = if fatal {
        match std::str::from_utf8(bytes) {
            Ok(s) => std::borrow::Cow::Borrowed(s),
            Err(_) => {
                throw(
                    scope,
                    OpError::type_error("The encoded data was not valid utf-8"),
                );
                return;
            }
        }
    } else {
        String::from_utf8_lossy(bytes)
    };
    match v8::String::new(scope, &s) {
        Some(s) => rv.set(s.into()),
        None => throw(scope, OpError::new("RangeError", "string too long")),
    }
}

// ------------------------------------------------------------ zero-copy probes & bench ops

/// Allocate `len` bytes in Rust and hand them to JS without a copy. The
/// address of the Rust allocation is remembered so JS can verify that the
/// Uint8Array it received points at the same memory.
fn op_alloc(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let len = arg_f64(scope, &args, 0) as usize;
    let fill = args.get(1).uint32_value(scope).unwrap_or(0) as u8;
    let v = vec![fill; len];
    state(scope).borrow_mut().last_alloc_ptr = v.as_ptr() as usize;
    let u8 = bytes_to_uint8array(scope, v);
    rv.set(u8.into());
}

fn op_last_alloc_addr(
    scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    rv.set_double(state(scope).borrow().last_alloc_ptr as f64);
}

fn op_buf_addr(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let b = try_op!(scope, BufView::from_value(args.get(0)));
    rv.set_double(b.addr() as f64);
}

/// Fill a JS buffer in place from Rust: `buf[i] = (start + i * step) & 0xff`.
fn op_buf_fill(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let mut b = try_op!(scope, BufView::from_value(args.get(0)));
    let start = arg_f64(scope, &args, 1) as u64;
    let step = arg_f64(scope, &args, 2) as u64;
    // SAFETY: sync op, JS blocked; single slice over the buffer.
    let s = unsafe { b.as_mut_slice() };
    for (i, x) in s.iter_mut().enumerate() {
        *x = (start.wrapping_add(i as u64 * step) & 0xff) as u8;
    }
}

/// Sum of bytes, computed in Rust over JS memory (proves Rust sees JS writes).
fn op_buf_sum(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let b = try_op!(scope, BufView::from_value(args.get(0)));
    // SAFETY: sync op, JS blocked.
    let s = unsafe { b.as_slice() };
    let sum: u64 = s.iter().map(|&x| x as u64).sum();
    rv.set_double(sum as f64);
}

fn op_noop(
    _scope: &mut v8::HandleScope,
    _args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
}

fn op_noop_async(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let id = promise_id(scope, &args);
    spawn_op(scope, id, async { Ok(OpValue::Undefined) });
}
