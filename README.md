<div align="center">

# Harbor

Harbor is a small, secure JavaScript/TypeScript runtime written in Rust. It embeds **V8** (through the `v8` crate), drives I/O with **tokio**, strips TypeScript with **oxc**, and runs every script in a **deny-by-default sandbox**. Deno inspired th...

![License](https://img.shields.io/badge/LICENSE-MIT-blue?style=for-the-badge)  ![Language](https://img.shields.io/badge/RUST-dea584?style=for-the-badge&logo=rust&logoColor=white)

Refer to [LICENSE](LICENSE) for the legal bits.

</div>

## 🚀 Quick start

```
harbor run [PERMISSIONS] <file.{js,ts,mjs,json}> [args...]
harbor eval [PERMISSIONS] '<code>'
harbor repl [PERMISSIONS]            # also: plain `harbor`

PERMISSIONS (everything is denied by default)
  --allow-read[=<path>,...]    --allow-write[=<path>,...]
  --allow-net[=<host[:port]>,...]
  --allow-env[=<VAR>,...]      --allow-run[=<program>,...]
  -A, --allow-all
```

Permission flags go before the file. Anything after the file becomes `harbor.args`.

### APIs

| Area | API |
|---|---|
| Console | `console.log/info/debug/error/warn/assert/count/time/timeEnd/group/groupEnd/table`, with `%s %d %i %f %j %o` formatting and a built-in inspector (handles cycles, Map, Set, Uint8Array, and class names) |
| Timers | `setTimeout`, `setInterval`, `clearTimeout`, `clearInterval`, `queueMicrotask`, `performance.now()` |
| Encoding | `TextEncoder` (`encode`, `encodeInto`) and `TextDecoder` (UTF-8, `fatal` mode) |
| FS | `harbor.readTextFile`, `readFile` (returns a `Uint8Array`), `writeTextFile`, `writeFile` (both take `{append}`), `readTextFileSync`, `writeTextFileSync`, `stat`, `mkdir`, `remove`, `open` → `FsFile` (`read(buf)`, `write(buf)`, `seek`, `close`, `.readable`) |
| Net | `harbor.listen({hostname, port})` → `Listener` (`accept`, async iterable, `close`); `harbor.connect` → `Conn` (`read`, `write`, `.readable`, `close`); `fetch` (HTTP/1.1 over `harbor.connect`, `http:` only) with `Response`, `Headers`, and `URL` |
| Streams | `ReadableStream`-like adapter (`getReader().read()`, `for await`, `.text()`) over files and sockets, backed by Rust |
| Process | `harbor.args`, `harbor.env.get/set/delete/has/toObject`, `harbor.exit(code)`, `harbor.cwd()`, `harbor.run({cmd})` → `{code, success, stdout, stderr}` |
| Modules | ES modules: relative, absolute, and `file://` specifiers; `.js` `.mjs` `.ts` `.json`; import cycles; top-level await; dynamic `import()`; `import.meta.url` and `import.meta.main` |
| Errors | `harbor.errors.{PermissionDenied, NotFound, AlreadyExists, BadResource, ConnectionRefused, InvalidData}` |

## 🏗️ Architecture

```
            ┌──────────────────────────── harbor (one OS thread) ──────────────────────────────┐
 file.ts ──►│ loader.rs: resolve() ─► read ─► oxc strip types ─► v8::compile_module             │
            │                        (module map: path ⇄ v8::Global<Module>, cycles OK)         │
            │                                                                                   │
            │  V8 isolate + context                         Rust                                │
            │  ┌──────────────────────┐   ops.op_x(...)    ┌───────────────────────────────┐    │
            │  │ bootstrap.js         │ ─────────────────► │ ops.rs                        │    │
            │  │  console, timers,    │   sync: value/throw│  permission check (always 1st)│    │
            │  │  harbor.*, fetch,    │ ◄───────────────── │  ResourceTable rid → Rc<dyn>  │    │
            │  │  streams, opAsync()  │   async: promiseId │  BufView: zero-copy Uint8Array│    │
            │  └──────────▲───────────┘                    └──────────────┬────────────────┘    │
            │             │ __opResolve(id, ok, value)                    │ push future         │
            │             │ __timerDispatch(id)                           ▼                     │
            │  ┌──────────┴──────────────────────────────────────────────────────────────┐     │
            │  │ event loop (runtime.rs), on a tokio current_thread runtime:               │     │
            │  │  1. fire expired timers in (deadline, seq) order, microtask checkpoint    │     │
            │  │     after each one                                                         │     │
            │  │  2. drain completed op futures (FuturesUnordered), checkpoint after each   │     │
            │  │  3. unhandled rejections or a rejected main module → exit 1               │     │
            │  │  4. nothing pending → exit 0; else select!{ next op, next timer deadline } │     │
            │  └───────────────────────────────────────────────────────────────────────────┘     │
            └───────────────────────────────────────────────────────────────────────────────────┘
                   tokio::fs/blocking pool, tokio::net, tokio::process do the actual I/O
```

* **Ops.** These are plain `fn(&mut HandleScope, FunctionCallbackArguments, ReturnValue)`
  callbacks registered on an object that is passed to the bootstrap closure and never
  exposed globally. An async op takes a promise id, pushes a `Future<Output = (id, OpResult)>`,
  and returns at once. `OpValue` is converted to a V8 value on the JS thread when the
  future completes.
* **Microtasks.** The isolate runs with `MicrotasksPolicy::Explicit`. The loop runs a
  checkpoint after every macrotask (each timer callback and each op completion), which
  gives the ordering the web platform uses (`timers_order.js` pins it down).
* **Resource table.** A `BTreeMap<u32, Rc<dyn Resource>>` with typed `get::<T>()`
  (downcast through `Any`). Rids are never reused. `close(rid)` runs the resource's
  `close` hook. For sockets and listeners that hook fires a `CancelHandle`, so a pending
  `accept()` or `read()` rejects with `BadResource` and no longer keeps the event loop
  alive.
* **Permissions** (`permissions.rs`). `Grant::{Denied, All, Only(list)}` exists per
  category. Before matching, paths are normalised *lexically* against the cwd (`..` is
  resolved, and matching is by path component, so `/tmp/a` does not match `/tmp/ab`).
  Every op that touches the outside world checks first. Failures become
  `harbor.errors.PermissionDenied`. Async ops record the JS call site, so the stack
  points at user code.
* **Unhandled rejections.** A V8 `PromiseRejectCallback` tracks rejections that have no
  handler and removes them again on `PromiseHandlerAddedAfterReject`. Any left after a
  macrotask turn are reported and the process exits with code 1.

### The zero-copy boundary (`ops.rs`)

* **Rust → JS.** `bytes_to_uint8array` passes the `Vec<u8>` heap allocation to
  `ArrayBuffer::new_backing_store_from_boxed_slice`. V8 adopts the allocation as is and
  frees it through a Rust deleter. `readFile`, `TextEncoder.encode`, and subprocess
  output all use this path. `zero_copy.js` checks that the `Uint8Array`'s data pointer
  equals the address of the `Vec` Rust allocated, including for a 100 MB buffer.
* **JS → Rust.** `BufView::from_value` takes the view's `SharedRef<BackingStore>` (a
  refcount that keeps the memory alive) and the pointer at `data + byte_offset`. Slices
  are only created inside `unsafe` blocks, each with a `SAFETY` note:
  * *sync ops*: JS is blocked inside the call and the op never calls back into JS, so
    the `&mut [u8]` cannot alias.
  * *socket reads and writes*: the slice is created **inside each `poll`**. Polls run on
    the JS thread while V8 is not executing, so the slice never lives across an `.await`
    that JS could interleave with.
  * *file reads and writes*: the pointer goes to tokio's blocking pool (`SendPtr`). The
    awaiting future owns the `BufView`, so the memory outlives the blocking call. This
    path relies on the same documented contract as `Deno.read`/Node `fs.read`: **do not
    touch the buffer until the promise settles.** Breaking that contract from JS is a
    data race. Harbor cannot enforce it without copying, so it is documented rather
    than hidden.

## 🧪 Testing

```bash
cargo test --workspace          # all tests: unit + JS corpus + CLI
cargo clippy --all-targets -- -D warnings && cargo fmt --check
HARBOR_TEST_FILTER=perm_ cargo test --test integration -- --nocapture   # subset, verbose
./scripts/bench.sh              # release build + benchmarks
```

* **Rust unit tests (22)**:
  * permissions: parsing, path-prefix matching, `..` escapes, net `host[:port]` and
    IPv6, env and run lists, flag accumulation, error text, and a 20k-case property
    test of normalisation versus allow decisions
  * resource table: add/get/close, typed-get mismatch, rids never reused, an `Rc` held
    by an in-flight op outliving `close`
  * module resolution and loading: relative, absolute, `file://`, canonical keys for
    cycles, bare and remote specifiers rejected, media types, TS stripping (enums,
    generics, type-only imports), TS syntax errors, JSON modules
* **JS integration corpus** (`tests/js/*.js|ts`, 25 scripts). The harness in
  `tests/integration.rs` runs each script through the real binary and compares stdout
  exactly against `<name>.out`. It also checks the exit code and the stderr substrings
  declared in header comments (`// args:`, `// exit:`, `// stderr:`). Every script gets
  a fresh temp dir. The corpus covers:
  * every API: console formatting, timers, encoding, FS, file handles and the resource
    table, streams, TCP, `fetch` against an in-process server on a random port, env,
    args, exit, and run
  * **permission denial for every op category**: read, write (including `..` escapes
    and prefix tricks), net (host and port lists, `fetch`), env (list versus
    `toObject`), run (allow-list), and an uncaught denial exiting with 1
  * timer ordering versus microtasks and `setInterval` arguments and cancellation
  * zero-copy: mutation is visible in both directions, subarray offsets, pointer
    identity for Rust-allocated buffers, and a 100 MB buffer (with timings)
  * error stack traces (file, line, and column of the user frames), uncaught errors in
    timers, syntax errors, an unhandled rejection exiting with 1, a rejection handled in
    the same turn not being reported, and a top-level await that never resolves
  * the module graph: a cycle with TDZ behaviour, live bindings, TS and JSON modules,
    top-level await in a dependency, re-exports from nested dirs, dynamic `import()`
    sharing instances, and bad specifiers
  * concurrency: 100 concurrent file round trips, 10,000 async ops in flight, ops
    interleaving with timers, `Promise.race` and `allSettled`
* **CLI tests (5)**: `eval` (event loop, exit codes, permissions), a REPL session over
  stdin (persistent state, multi-line input, promises settling, errors not killing the
  REPL), and usage errors and exit code 2.

Latest run: **22 unit + 25 JS corpus scripts + 5 CLI = all passing**. clippy
(`-D warnings`) and `fmt --check` are clean.

## ⏱️ Benchmarks

Measured with `./scripts/bench.sh` (release build) on a shared 4-core Linux VM where
about 15 other jobs were running. Treat the numbers as indicative.

| Benchmark | Result |
|---|---|
| Startup, `harbor run hello.js` (hyperfine, 100 runs) | **7.3 ms ± 3.0** |
| Startup, `harbor run hello.ts` (includes oxc parse, transform, and codegen) | **7.9 ms ± 4.9** |
| Startup, `harbor eval 1` (bash loop, 50 runs) | 6.3 ms |
| Sync op call (`noop`) | **120 M ops/s** (8 ns per call) |
| Sync op call with a 1 MB `Uint8Array` argument (backing-store lookup, no copy) | 15.4 M ops/s (65 ns per call) |
| Async op, awaited one at a time | **332 k ops/s** (3.0 µs per op) |
| Async op, 1000 in flight (`Promise.all`) | 263 k ops/s (3.8 µs per op) |
| `harbor.readFile` of 256 MB (the read plus the zero-copy handoff to JS) | **1.90 GB/s** |
| `FsFile.read()` of 1 MB chunks into one reused JS buffer | **3.88 GB/s** |
| `for await` over `file.readable` (64 KB chunks, a new buffer per chunk) | 1.22 GB/s |

From `tests/js/zero_copy.js` (debug build, printed to stderr): passing a 100 MB buffer
to an op takes well under 1 µs per call. The test asserts < 1 ms. That cost does not
depend on the buffer size, because only the pointer and length cross the boundary.
The async op cost includes allocating the promise, capturing the call-site stack
trace, one `FuturesUnordered` poll, and a microtask checkpoint per completion.
The 8 ns sync figure is plausible because V8 can call an API function from optimised
JIT code without leaving it. It is a lower bound, because a real op also does the work
of the op itself.

## ⚠️ Honest limits

* **No source maps.** For `.ts` files, stack-trace line numbers refer to oxc's
  re-emitted JS. For `.js` files they are exact.
* **TypeScript is stripped, not type-checked.** oxc parses and transforms it, and
  `enum`/`namespace` are lowered. JSX is not enabled.
* **`fetch` supports `http:` only.** It is a small HTTP/1.1 client written in JS on top
  of `harbor.connect`, with `Connection: close`, no redirects, and no TLS. HTTPS would
  need rustls or reqwest. It was cut to keep build time down.
* The `ReadableStream` is a subset (`getReader`, `read`, `cancel`, async iteration,
  `text()`). It is not WHATWG-complete: no `pipeTo`, no tee, no BYOB.
* Module loading reads files without `--allow-read`, as Deno does for the code you
  chose to run. Only relative, absolute, and `file://` specifiers work: no bare or
  `npm:` specifiers and no remote URLs.
* Permission checks are lexical. A symlink inside an allowed dir that points outside it
  is followed. Canonicalising would close that gap at the cost of extra syscalls.
* `harbor.exit()` calls `process::exit` after flushing stdout and stderr. It does not
  unwind JS `finally` blocks.
* For async *file* `read(buf)`/`writeFile(buf)`, the zero-copy contract above means
  touching the buffer before the promise settles is a data race.
* Sanitisers and Miri were not run. Miri cannot execute the V8 FFI, and ASan would need
  a V8 built with ASan. The unsafe surface is small and documented (`BufView`,
  `SendPtr`, and three `CallbackScope::new` calls).
* The REPL is line-based, with no line editing or history. A bracket counter detects
  multi-line input.

## Why V8 (and not QuickJS)

The plan was to try `v8` first and fall back to `rquickjs` if the prebuilt static
library could not be downloaded. The download from GitHub releases worked: the first
`cargo build` that included `v8 = "=130.0.7"` finished in about 53 s. So Harbor uses
V8. The version is pinned to 130 for a stable `HandleScope` API.
