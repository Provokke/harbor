// Harbor bootstrap. Evaluated once per isolate as a classic script; the
// result is a function the Rust side calls with the op table and argv.
// Everything user-visible is installed on globalThis here.
(function bootstrap(ops, argv) {
  "use strict";

  // ------------------------------------------------------------ errors
  class PermissionDenied extends Error { constructor(m) { super(m); this.name = "PermissionDenied"; } }
  class NotFound extends Error { constructor(m) { super(m); this.name = "NotFound"; } }
  class AlreadyExists extends Error { constructor(m) { super(m); this.name = "AlreadyExists"; } }
  class BadResource extends Error { constructor(m) { super(m); this.name = "BadResource"; } }
  class ConnectionRefused extends Error { constructor(m) { super(m); this.name = "ConnectionRefused"; } }
  class InvalidData extends Error { constructor(m) { super(m); this.name = "InvalidData"; } }
  const errors = { PermissionDenied, NotFound, AlreadyExists, BadResource, ConnectionRefused, InvalidData };
  const builtinErrors = { TypeError, RangeError, SyntaxError, Error };

  function buildError(cls, msg) {
    const C = errors[cls] || builtinErrors[cls];
    let e;
    if (C) e = new C(msg); else { e = new Error(msg); e.name = cls; }
    Error.captureStackTrace(e, buildError); // hide this frame
    return e;
  }

  // ------------------------------------------------------------ async op plumbing
  let nextPromiseId = 1;
  const pendingPromises = new Map();

  function opAsync(name, ...args) {
    const id = nextPromiseId++;
    if (nextPromiseId > 0xfffffff0) nextPromiseId = 1;
    let resolve, reject;
    const p = new Promise((res, rej) => { resolve = res; reject = rej; });
    // Capture the JS call site now so a later rejection points at user code
    // rather than at the event loop (V8 formats the frames lazily).
    const site = {};
    Error.captureStackTrace(site, opAsync);
    pendingPromises.set(id, { resolve, reject, site });
    try {
      ops[name](id, ...args);
    } catch (e) {
      pendingPromises.delete(id);
      throw e;
    }
    return p;
  }

  function opResolve(id, ok, value) {
    const entry = pendingPromises.get(id);
    if (!entry) return;
    pendingPromises.delete(id);
    if (ok) { entry.resolve(value); return; }
    if (value instanceof Error) {
      const frames = String(entry.site.stack).split("\n").slice(1).join("\n");
      value.stack = `${value.name}: ${value.message}\n${frames}`;
    }
    entry.reject(value);
  }

  // ------------------------------------------------------------ inspect / console
  function inspect(v, depth = 0, seen = new Set()) {
    switch (typeof v) {
      case "string": return depth === 0 ? v : JSON.stringify(v);
      case "number": return Object.is(v, -0) ? "-0" : String(v);
      case "bigint": return `${v}n`;
      case "undefined": return "undefined";
      case "boolean": return String(v);
      case "symbol": return v.toString();
      case "function": return `[Function: ${v.name || "(anonymous)"}]`;
    }
    if (v === null) return "null";
    if (seen.has(v)) return "[Circular]";
    if (v instanceof Error) return v.stack || `${v.name}: ${v.message}`;
    if (depth > 4) return Array.isArray(v) ? "[Array]" : "[Object]";
    seen.add(v);
    try {
      const inner = (x) => inspect(x, depth + 1, seen);
      if (Array.isArray(v)) return v.length === 0 ? "[]" : `[ ${v.map(inner).join(", ")} ]`;
      if (v instanceof Uint8Array) {
        const shown = Array.from(v.subarray(0, 32)).join(", ");
        return `Uint8Array(${v.length}) [ ${shown}${v.length > 32 ? ", ..." : ""} ]`;
      }
      if (v instanceof Map) return `Map(${v.size}) { ${[...v].map(([k, x]) => `${inner(k)} => ${inner(x)}`).join(", ")} }`;
      if (v instanceof Set) return `Set(${v.size}) { ${[...v].map(inner).join(", ")} }`;
      if (v instanceof Promise) return "Promise {}";
      if (v instanceof Date) return v.toISOString();
      if (v instanceof RegExp) return String(v);
      const keys = Object.keys(v);
      const ctor = v.constructor && v.constructor !== Object ? `${v.constructor.name} ` : "";
      if (keys.length === 0) return `${ctor}{}`;
      const body = keys.map((k) => `${/^[A-Za-z_$][\w$]*$/.test(k) ? k : JSON.stringify(k)}: ${inner(v[k])}`);
      return `${ctor}{ ${body.join(", ")} }`;
    } finally {
      seen.delete(v);
    }
  }

  function format(args) {
    if (typeof args[0] === "string" && /%[sdifoOj%]/.test(args[0])) {
      let i = 1;
      const head = args[0].replace(/%([sdifoOj%])/g, (m, c) => {
        if (c === "%") return "%";
        if (i >= args.length) return m;
        const a = args[i++];
        switch (c) {
          case "s": return String(a);
          case "d": case "i": return String(c === "i" ? Math.trunc(Number(a)) : Number(a));
          case "f": return String(Number(a));
          case "j": return JSON.stringify(a);
          default: return inspect(a, 1);
        }
      });
      return [head, ...args.slice(i).map((a) => inspect(a))].join(" ");
    }
    return args.map((a) => inspect(a)).join(" ");
  }

  const counts = new Map();
  const timersByLabel = new Map();
  let groupIndent = "";
  const out = (s) => ops.op_print(groupIndent + s + "\n", false);
  const err = (s) => ops.op_print(groupIndent + s + "\n", true);
  const console = {
    log: (...a) => out(format(a)),
    info: (...a) => out(format(a)),
    debug: (...a) => out(format(a)),
    error: (...a) => err(format(a)),
    warn: (...a) => err(format(a)),
    assert: (cond, ...a) => { if (!cond) err("Assertion failed" + (a.length ? ": " + format(a) : "")); },
    count: (label = "default") => { const n = (counts.get(label) || 0) + 1; counts.set(label, n); out(`${label}: ${n}`); },
    time: (label = "default") => timersByLabel.set(label, ops.op_now()),
    timeEnd: (label = "default") => {
      const t = timersByLabel.get(label);
      timersByLabel.delete(label);
      if (t !== undefined) out(`${label}: ${(ops.op_now() - t).toFixed(3)}ms`);
    },
    group: (...a) => { if (a.length) out(format(a)); groupIndent += "  "; },
    groupEnd: () => { groupIndent = groupIndent.slice(2); },
    table: (d) => out(inspect(d)),
  };

  // ------------------------------------------------------------ timers
  let nextTimerId = 1;
  const timers = new Map();

  function normalizeDelay(ms) {
    ms = Number(ms);
    return Number.isFinite(ms) && ms > 0 ? ms : 0;
  }

  function setTimer(cb, ms, args, repeat) {
    if (typeof cb !== "function") throw new TypeError("timer callback must be a function");
    const id = nextTimerId++;
    const delay = normalizeDelay(ms);
    timers.set(id, { cb, args, repeat, delay });
    ops.op_timer_start(id, delay);
    return id;
  }

  function timerDispatch(id) {
    const t = timers.get(id);
    if (!t) return;
    if (t.repeat) ops.op_timer_start(id, t.delay); else timers.delete(id);
    Reflect.apply(t.cb, undefined, t.args);
  }

  function clearTimer(id) {
    id = Number(id);
    if (timers.delete(id)) ops.op_timer_cancel(id);
  }

  const setTimeout = (cb, ms, ...args) => setTimer(cb, ms, args, false);
  const setInterval = (cb, ms, ...args) => setTimer(cb, ms, args, true);
  const queueMicrotask = (cb) => {
    if (typeof cb !== "function") throw new TypeError("queueMicrotask callback must be a function");
    Promise.resolve().then(cb);
  };

  // ------------------------------------------------------------ encoding
  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") { return ops.op_encode(String(input)); }
    encodeInto(input, dest) {
      const bytes = ops.op_encode(String(input));
      const n = Math.min(bytes.length, dest.length);
      dest.set(bytes.subarray(0, n));
      return { read: input.length, written: n };
    }
  }

  class TextDecoder {
    #fatal;
    constructor(label = "utf-8", options = {}) {
      label = String(label).toLowerCase();
      if (label !== "utf-8" && label !== "utf8") throw new RangeError(`unsupported encoding '${label}'`);
      this.#fatal = !!options.fatal;
    }
    get encoding() { return "utf-8"; }
    get fatal() { return this.#fatal; }
    decode(input) {
      if (input === undefined) return "";
      let view = input;
      if (input instanceof ArrayBuffer) view = new Uint8Array(input);
      else if (ArrayBuffer.isView(input) && !(input instanceof Uint8Array)) {
        view = new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
      }
      return ops.op_decode(view, this.#fatal);
    }
  }

  // ------------------------------------------------------------ streams
  // A minimal ReadableStream-like adapter over a Rust-backed reader. Each
  // read() allocates a fresh chunk and Rust fills it in place (zero-copy).
  class HarborReadableStream {
    #pull; #cancel; #done = false; #locked = false;
    constructor(pull, cancel) { this.#pull = pull; this.#cancel = cancel; }
    get locked() { return this.#locked; }
    getReader() {
      if (this.#locked) throw new TypeError("stream is locked");
      this.#locked = true;
      const self = this;
      return {
        async read() {
          if (self.#done) return { value: undefined, done: true };
          const chunk = await self.#pull();
          if (chunk === null) { self.#done = true; return { value: undefined, done: true }; }
          return { value: chunk, done: false };
        },
        async cancel() { self.#done = true; if (self.#cancel) self.#cancel(); },
        releaseLock() { self.#locked = false; },
      };
    }
    async *[Symbol.asyncIterator]() {
      const r = this.getReader();
      try {
        while (true) {
          const { value, done } = await r.read();
          if (done) return;
          yield value;
        }
      } finally {
        r.releaseLock();
      }
    }
    async text() {
      const parts = [];
      for await (const c of this) parts.push(c);
      return new TextDecoder().decode(concat(parts));
    }
  }

  function concat(parts) {
    let len = 0;
    for (const p of parts) len += p.length;
    const outBuf = new Uint8Array(len);
    let off = 0;
    for (const p of parts) { outBuf.set(p, off); off += p.length; }
    return outBuf;
  }

  function readableFromRid(rid, chunkSize = 64 * 1024, onEnd) {
    return new HarborReadableStream(async () => {
      const buf = new Uint8Array(chunkSize);
      const n = await opAsync("op_read", rid, buf);
      if (n === 0) { if (onEnd) onEnd(); return null; }
      return n === chunkSize ? buf : buf.subarray(0, n);
    }, onEnd);
  }

  class FsFile {
    #closed = false;
    constructor(rid) { this.rid = rid; }
    read(buf) { return opAsync("op_read", this.rid, buf).then((n) => (n === 0 && buf.length > 0 ? null : n)); }
    write(buf) { return opAsync("op_write", this.rid, buf); }
    seek(offset) { return ops.op_seek(this.rid, offset); }
    get readable() { return readableFromRid(this.rid, 64 * 1024, () => this.close()); }
    close() { if (!this.#closed) { this.#closed = true; ops.op_close(this.rid); } }
  }

  class Conn {
    #closed = false;
    constructor(info) { this.rid = info.rid; this.localAddr = info.localAddr; this.remoteAddr = info.remoteAddr; }
    read(buf) { return opAsync("op_read", this.rid, buf).then((n) => (n === 0 && buf.length > 0 ? null : n)); }
    write(buf) { return opAsync("op_write", this.rid, buf); }
    get readable() { return readableFromRid(this.rid, 16 * 1024); }
    close() { if (!this.#closed) { this.#closed = true; ops.op_close(this.rid); } }
  }

  class Listener {
    constructor(info) { this.rid = info.rid; this.addr = info.addr; }
    async accept() { return new Conn(await opAsync("op_accept", this.rid)); }
    close() { ops.op_close(this.rid); }
    async *[Symbol.asyncIterator]() {
      while (true) {
        let c;
        try { c = await this.accept(); } catch (e) { if (e instanceof BadResource) return; throw e; }
        yield c;
      }
    }
  }

  // ------------------------------------------------------------ fetch (HTTP/1.1 over harbor.connect)
  class Headers {
    #m = new Map();
    constructor(init) {
      if (init instanceof Headers) init.forEach((v, k) => this.set(k, v));
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init) for (const k of Object.keys(init)) this.set(k, init[k]);
    }
    get(k) { const v = this.#m.get(k.toLowerCase()); return v === undefined ? null : v; }
    set(k, v) { this.#m.set(k.toLowerCase(), String(v)); }
    append(k, v) { k = k.toLowerCase(); this.#m.set(k, this.#m.has(k) ? `${this.#m.get(k)}, ${v}` : String(v)); }
    has(k) { return this.#m.has(k.toLowerCase()); }
    delete(k) { this.#m.delete(k.toLowerCase()); }
    forEach(f) { for (const [k, v] of this.#m) f(v, k, this); }
    *entries() { yield* this.#m.entries(); }
    [Symbol.iterator]() { return this.#m.entries(); }
  }

  class Response {
    #bytes;
    constructor(body, init = {}) {
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.headers = new Headers(init.headers);
      this.url = init.url ?? "";
      this.#bytes = typeof body === "string" ? new TextEncoder().encode(body) : (body ?? new Uint8Array(0));
      this.bodyUsed = false;
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    async arrayBuffer() { this.bodyUsed = true; const b = this.#bytes; return b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength); }
    async bytes() { this.bodyUsed = true; return this.#bytes; }
    async text() { this.bodyUsed = true; return new TextDecoder().decode(this.#bytes); }
    async json() { return JSON.parse(await this.text()); }
    get body() { let sent = false; return new HarborReadableStream(async () => { if (sent) return null; sent = true; return this.#bytes; }); }
  }

  function indexOfCRLFCRLF(b) {
    for (let i = 0; i + 3 < b.length; i++) {
      if (b[i] === 13 && b[i + 1] === 10 && b[i + 2] === 13 && b[i + 3] === 10) return i;
    }
    return -1;
  }

  function decodeChunked(body) {
    const parts = [];
    let i = 0;
    const dec = new TextDecoder();
    while (i < body.length) {
      let j = i;
      while (j + 1 < body.length && !(body[j] === 13 && body[j + 1] === 10)) j++;
      const size = parseInt(dec.decode(body.subarray(i, j)), 16);
      if (!size) break;
      parts.push(body.subarray(j + 2, j + 2 + size));
      i = j + 2 + size + 2;
    }
    return concat(parts);
  }

  async function fetch(input, init = {}) {
    const url = new URL(typeof input === "string" ? input : String(input.url ?? input));
    if (url.protocol !== "http:") throw new TypeError(`fetch: unsupported protocol '${url.protocol}' (only http: is implemented)`);
    const method = (init.method || "GET").toUpperCase();
    const headers = new Headers(init.headers);
    let body = init.body;
    if (typeof body === "string") body = new TextEncoder().encode(body);
    const port = Number(url.port || 80);
    const conn = new Conn(await opAsync("op_connect", url.hostname, port));
    try {
      if (!headers.has("host")) headers.set("host", url.host);
      headers.set("connection", "close");
      if (body) headers.set("content-length", String(body.length));
      let head = `${method} ${url.pathname}${url.search} HTTP/1.1\r\n`;
      headers.forEach((v, k) => { head += `${k}: ${v}\r\n`; });
      head += "\r\n";
      await conn.write(new TextEncoder().encode(head));
      if (body) await conn.write(body);
      const parts = [];
      for await (const c of conn.readable) parts.push(c.slice());
      const raw = concat(parts);
      const split = indexOfCRLFCRLF(raw);
      if (split < 0) throw new InvalidData("fetch: malformed HTTP response");
      const lines = new TextDecoder().decode(raw.subarray(0, split)).split("\r\n");
      const m = /^HTTP\/1\.[01] (\d{3}) ?(.*)$/.exec(lines[0]);
      if (!m) throw new InvalidData("fetch: malformed status line");
      const rh = new Headers();
      for (const l of lines.slice(1)) { const c = l.indexOf(":"); if (c > 0) rh.append(l.slice(0, c).trim(), l.slice(c + 1).trim()); }
      let rbody = raw.subarray(split + 4);
      if ((rh.get("transfer-encoding") || "").includes("chunked")) rbody = decodeChunked(rbody);
      else if (rh.has("content-length")) rbody = rbody.subarray(0, Number(rh.get("content-length")));
      return new Response(rbody, { status: Number(m[1]), statusText: m[2], headers: rh, url: url.href });
    } finally {
      conn.close();
    }
  }

  // ------------------------------------------------------------ URL (small subset)
  class URL {
    constructor(s, base) {
      s = String(s);
      if (base !== undefined && !/^[a-z][a-z0-9+.-]*:/i.test(s)) {
        const b = new URL(base);
        s = s.startsWith("/") ? `${b.protocol}//${b.host}${s}` : `${b.protocol}//${b.host}${b.pathname.replace(/[^/]*$/, "")}${s}`;
      }
      const m = /^([a-z][a-z0-9+.-]*:)(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(s);
      if (!m) throw new TypeError(`Invalid URL: '${s}'`);
      this.protocol = m[1].toLowerCase();
      this.host = m[2] || "";
      const hp = /^(.*?)(?::(\d+))?$/.exec(this.host);
      this.hostname = hp[1];
      this.port = hp[2] || "";
      this.pathname = m[3] || "/";
      this.search = m[4] && m[4] !== "?" ? m[4] : "";
      this.hash = m[5] || "";
    }
    get origin() { return `${this.protocol}//${this.host}`; }
    get href() { return `${this.protocol}${this.host || this.protocol === "file:" ? "//" : ""}${this.host}${this.pathname}${this.search}${this.hash}`; }
    toString() { return this.href; }
  }

  // ------------------------------------------------------------ harbor namespace
  const env = {
    get: (k) => ops.op_env_get(String(k)),
    set: (k, v) => ops.op_env_set(String(k), String(v)),
    delete: (k) => ops.op_env_delete(String(k)),
    has: (k) => ops.op_env_get(String(k)) !== undefined,
    toObject: () => ops.op_env_to_object(),
  };

  const harbor = {
    version: "0.1.0",
    args: Object.freeze([...argv]),
    env,
    errors,
    exit: (code = 0) => ops.op_exit(code | 0),
    cwd: () => ops.op_cwd(),
    readFile: (p) => opAsync("op_read_file", String(p)),
    readTextFile: (p) => opAsync("op_read_text_file", String(p)),
    readTextFileSync: (p) => ops.op_read_text_file_sync(String(p)),
    writeFile: (p, data, opts) => opAsync("op_write_file", String(p), data, opts),
    writeTextFile: (p, s, opts) => opAsync("op_write_text_file", String(p), String(s), opts),
    writeTextFileSync: (p, s) => ops.op_write_text_file_sync(String(p), String(s)),
    remove: (p, opts = {}) => opAsync("op_remove", String(p), !!opts.recursive),
    mkdir: (p, opts = {}) => opAsync("op_mkdir", String(p), !!opts.recursive),
    stat: (p) => opAsync("op_stat", String(p)),
    open: async (p, opts = { read: true }) => new FsFile(ops.op_open(String(p), opts)),
    close: (rid) => ops.op_close(rid),
    resources: () => ops.op_resources(),
    listen: ({ hostname = "127.0.0.1", port }) => new Listener(ops.op_listen(hostname, port)),
    connect: async ({ hostname = "127.0.0.1", port }) => new Conn(await opAsync("op_connect", hostname, port)),
    run: async ({ cmd }) => {
      const r = await opAsync("op_run", cmd);
      return { ...r, stdoutText: () => new TextDecoder().decode(r.stdout), stderrText: () => new TextDecoder().decode(r.stderr) };
    },
    inspect: (v) => inspect(v, 1),
    // Low-level access for tests and benchmarks.
    core: {
      opAsync,
      ops: {
        alloc: ops.op_alloc,
        lastAllocAddr: ops.op_last_alloc_addr,
        bufAddr: ops.op_buf_addr,
        bufFill: ops.op_buf_fill,
        bufSum: ops.op_buf_sum,
        noop: ops.op_noop,
        noopAsync: () => opAsync("op_noop_async"),
        now: ops.op_now,
      },
    },
  };
  Object.freeze(harbor.core.ops);
  Object.freeze(harbor.core);

  const define = (name, value) =>
    Object.defineProperty(globalThis, name, { value, writable: true, configurable: true, enumerable: false });
  define("console", console);
  define("setTimeout", setTimeout);
  define("setInterval", setInterval);
  define("clearTimeout", clearTimer);
  define("clearInterval", clearTimer);
  define("queueMicrotask", queueMicrotask);
  define("TextEncoder", TextEncoder);
  define("TextDecoder", TextDecoder);
  define("ReadableStream", HarborReadableStream);
  define("fetch", fetch);
  define("Headers", Headers);
  define("Response", Response);
  define("URL", URL);
  define("performance", { now: ops.op_now });
  define("harbor", Object.freeze(harbor));
  define("globalThis", globalThis);

  return { opResolve, timerDispatch, buildError, inspect: (v) => inspect(v, 1) };
})
