// WHATWG streams (subset), Headers, Request, Response, FormData, fetch.

const Buffer = internal.Buffer;

// ---------------------------------------------------------------------------
// ReadableStream / WritableStream / TransformStream (minimal, spec-shaped)
// ---------------------------------------------------------------------------

class ReadableStreamDefaultController {
  #stream;
  constructor(stream) {
    this.#stream = stream;
  }
  get desiredSize() {
    const s = this.#stream._state;
    return s.highWaterMark - s.queue.length;
  }
  enqueue(chunk) {
    const s = this.#stream._state;
    if (s.closeRequested || s.state !== "readable") throw new TypeError("Invalid state: Controller is already closed");
    const reader = s.reads.shift();
    if (reader) reader.resolve({ value: chunk, done: false });
    else s.queue.push(chunk);
  }
  close() {
    const s = this.#stream._state;
    if (s.closeRequested) throw new TypeError("Invalid state: Controller is already closed");
    s.closeRequested = true;
    if (s.queue.length === 0) this.#stream._finishClose();
  }
  error(e) {
    this.#stream._error(e);
  }
}

class ReadableStreamDefaultReader {
  #stream;
  #closed;
  constructor(stream) {
    if (stream.locked) throw new TypeError("ReadableStream is locked");
    this.#stream = stream;
    stream._state.reader = this;
    this.#closed = stream._state.closedPromise;
  }
  get closed() {
    return this.#closed;
  }
  read() {
    const stream = this.#stream;
    if (!stream) return Promise.reject(new TypeError("Reader was released"));
    const s = stream._state;
    if (s.queue.length) {
      const value = s.queue.shift();
      if (s.closeRequested && s.queue.length === 0) stream._finishClose();
      else stream._maybePull();
      return Promise.resolve({ value, done: false });
    }
    if (s.state === "closed") return Promise.resolve({ value: undefined, done: true });
    if (s.state === "errored") return Promise.reject(s.storedError);
    return new Promise((resolve, reject) => {
      s.reads.push({ resolve, reject });
      stream._maybePull();
    });
  }
  releaseLock() {
    if (this.#stream) {
      this.#stream._state.reader = null;
      this.#stream = null;
    }
  }
  cancel(reason) {
    return this.#stream ? this.#stream._cancel(reason) : Promise.resolve();
  }
}

class ReadableStream {
  constructor(source = {}, strategy = {}) {
    let resolveClosed, rejectClosed;
    const closedPromise = new Promise((res, rej) => {
      resolveClosed = res;
      rejectClosed = rej;
    });
    closedPromise.catch(() => {});
    this._state = {
      state: "readable",
      queue: [],
      reads: [],
      reader: null,
      closeRequested: false,
      storedError: undefined,
      pulling: false,
      pullAgain: false,
      started: false,
      highWaterMark: strategy.highWaterMark ?? 1,
      source,
      closedPromise,
      resolveClosed,
      rejectClosed,
    };
    this._controller = new ReadableStreamDefaultController(this);
    const start = () => (typeof source.start === "function" ? source.start(this._controller) : undefined);
    Promise.resolve()
      .then(start)
      .then(
        () => {
          this._state.started = true;
          this._maybePull();
        },
        (e) => this._error(e),
      );
  }
  get locked() {
    return this._state.reader !== null;
  }
  _maybePull() {
    const s = this._state;
    if (!s.started || s.state !== "readable" || s.closeRequested) return;
    if (typeof s.source.pull !== "function") return;
    if (s.reads.length === 0 && s.queue.length >= s.highWaterMark) return;
    if (s.pulling) {
      s.pullAgain = true;
      return;
    }
    s.pulling = true;
    Promise.resolve()
      .then(() => s.source.pull(this._controller))
      .then(
        () => {
          s.pulling = false;
          if (s.pullAgain || s.reads.length) {
            s.pullAgain = false;
            this._maybePull();
          }
        },
        (e) => this._error(e),
      );
  }
  _finishClose() {
    const s = this._state;
    if (s.state !== "readable") return;
    s.state = "closed";
    for (const r of s.reads.splice(0)) r.resolve({ value: undefined, done: true });
    s.resolveClosed();
  }
  _error(e) {
    const s = this._state;
    if (s.state !== "readable") return;
    s.state = "errored";
    s.storedError = e;
    s.queue = [];
    for (const r of s.reads.splice(0)) r.reject(e);
    s.rejectClosed(e);
  }
  _cancel(reason) {
    const s = this._state;
    if (s.state === "closed") return Promise.resolve();
    if (s.state === "errored") return Promise.reject(s.storedError);
    s.queue = [];
    this._finishClose();
    return Promise.resolve(typeof s.source.cancel === "function" ? s.source.cancel(reason) : undefined).then(() => undefined);
  }
  cancel(reason) {
    if (this.locked) return Promise.reject(new TypeError("ReadableStream is locked"));
    return this._cancel(reason);
  }
  getReader(options) {
    if (options?.mode === "byob") throw new TypeError("BYOB readers are not supported");
    return new ReadableStreamDefaultReader(this);
  }
  async *values(options = {}) {
    const reader = this.getReader();
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) return;
        yield value;
      }
    } finally {
      if (!options.preventCancel) await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
  }
  [Symbol.asyncIterator](options) {
    return this.values(options);
  }
  async pipeTo(dest, options = {}) {
    const reader = this.getReader();
    const writer = dest.getWriter();
    try {
      for (;;) {
        if (options.signal?.aborted) throw options.signal.reason;
        const { value, done } = await reader.read();
        if (done) break;
        await writer.write(value);
      }
      if (!options.preventClose) await writer.close();
    } catch (e) {
      if (!options.preventAbort) await writer.abort(e).catch(() => {});
      throw e;
    } finally {
      reader.releaseLock();
      writer.releaseLock();
    }
  }
  pipeThrough(transform, options) {
    this.pipeTo(transform.writable, options).catch(() => {});
    return transform.readable;
  }
  tee() {
    const reader = this.getReader();
    let c1, c2;
    const pull = async () => {
      const { value, done } = await reader.read();
      if (done) {
        c1.close();
        c2.close();
      } else {
        c1.enqueue(value);
        c2.enqueue(value);
      }
    };
    const a = new ReadableStream({ start: (c) => void (c1 = c), pull });
    const b = new ReadableStream({ start: (c) => void (c2 = c), pull });
    return [a, b];
  }
  static from(iterable) {
    const it = iterable[Symbol.asyncIterator] ? iterable[Symbol.asyncIterator]() : iterable[Symbol.iterator]();
    return new ReadableStream({
      async pull(c) {
        const { value, done } = await it.next();
        if (done) c.close();
        else c.enqueue(value);
      },
      async cancel(reason) {
        await it.return?.(reason);
      },
    });
  }
  get [Symbol.toStringTag]() {
    return "ReadableStream";
  }
}

class WritableStream {
  #sink;
  #locked = false;
  #state = "writable";
  #chain = Promise.resolve();
  #closed;
  #resolveClosed;
  constructor(sink = {}) {
    this.#sink = sink;
    this.#closed = new Promise((r) => (this.#resolveClosed = r));
    const controller = { error: (e) => (this.#state = "errored"), signal: new AbortController().signal };
    this._controller = controller;
    if (typeof sink.start === "function") this.#chain = Promise.resolve(sink.start(controller));
  }
  get locked() {
    return this.#locked;
  }
  getWriter() {
    if (this.#locked) throw new TypeError("WritableStream is locked");
    this.#locked = true;
    const stream = this;
    return {
      get closed() {
        return stream.#closed;
      },
      get ready() {
        return Promise.resolve();
      },
      get desiredSize() {
        return 1;
      },
      write: (chunk) => stream._write(chunk),
      close: () => stream._close(),
      abort: (reason) => stream._abort(reason),
      releaseLock: () => {
        stream.#locked = false;
      },
    };
  }
  _write(chunk) {
    this.#chain = this.#chain.then(() => (typeof this.#sink.write === "function" ? this.#sink.write(chunk, this._controller) : undefined));
    return this.#chain;
  }
  _close() {
    this.#chain = this.#chain.then(async () => {
      if (typeof this.#sink.close === "function") await this.#sink.close();
      this.#state = "closed";
      this.#resolveClosed();
    });
    return this.#chain;
  }
  _abort(reason) {
    this.#state = "errored";
    return Promise.resolve(typeof this.#sink.abort === "function" ? this.#sink.abort(reason) : undefined);
  }
  close() {
    return this._close();
  }
  abort(reason) {
    return this._abort(reason);
  }
}

class TransformStream {
  constructor(transformer = {}) {
    let rc;
    this.readable = new ReadableStream({ start: (c) => void (rc = c) });
    const controller = {
      enqueue: (chunk) => rc.enqueue(chunk),
      error: (e) => rc.error(e),
      terminate: () => rc.close(),
      get desiredSize() {
        return rc.desiredSize;
      },
    };
    const started = Promise.resolve().then(() => transformer.start?.(controller));
    this.writable = new WritableStream({
      write: async (chunk) => {
        await started;
        if (typeof transformer.transform === "function") await transformer.transform(chunk, controller);
        else controller.enqueue(chunk);
      },
      close: async () => {
        await started;
        if (typeof transformer.flush === "function") await transformer.flush(controller);
        rc.close();
      },
      abort: (reason) => rc.error(reason),
    });
  }
}

class TextDecoderStream extends TransformStream {
  constructor(label = "utf-8", options = {}) {
    const decoder = new internal.TextDecoder(label, options);
    super({
      transform: (chunk, c) => {
        const s = decoder.decode(chunk, { stream: true });
        if (s) c.enqueue(s);
      },
      flush: (c) => {
        const s = decoder.decode();
        if (s) c.enqueue(s);
      },
    });
  }
}

class TextEncoderStream extends TransformStream {
  constructor() {
    const encoder = new internal.TextEncoder();
    super({ transform: (chunk, c) => c.enqueue(encoder.encode(String(chunk))) });
  }
}

class CountQueuingStrategy {
  constructor({ highWaterMark }) {
    this.highWaterMark = highWaterMark;
  }
  size() {
    return 1;
  }
}
class ByteLengthQueuingStrategy {
  constructor({ highWaterMark }) {
    this.highWaterMark = highWaterMark;
  }
  size(chunk) {
    return chunk.byteLength;
  }
}

internal.ReadableStream = ReadableStream;
internal.WritableStream = WritableStream;
internal.TransformStream = TransformStream;
internal.TextDecoderStream = TextDecoderStream;
internal.TextEncoderStream = TextEncoderStream;

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

const kHeaders = Symbol("headers");

function validateHeaderName(name) {
  if (!/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name)) throw new TypeError(`Headers.append: "${name}" is an invalid header name.`);
}

class Headers {
  constructor(init = undefined) {
    this[kHeaders] = [];
    if (init === undefined || init === null) return;
    if (init instanceof Headers) {
      for (const [k, v] of init[kHeaders]) this[kHeaders].push([k, v]);
    } else if (typeof init[Symbol.iterator] === "function") {
      for (const pair of init) {
        const [k, v] = Array.from(pair);
        this.append(k, v);
      }
    } else if (typeof init === "object") {
      for (const k of Object.keys(init)) this.append(k, init[k]);
    }
  }
  append(name, value) {
    name = String(name);
    validateHeaderName(name);
    this[kHeaders].push([name.toLowerCase(), String(value).trim()]);
  }
  set(name, value) {
    name = String(name);
    validateHeaderName(name);
    const lower = name.toLowerCase();
    const idx = this[kHeaders].findIndex(([k]) => k === lower);
    if (idx < 0) this[kHeaders].push([lower, String(value).trim()]);
    else {
      this[kHeaders][idx][1] = String(value).trim();
      this[kHeaders] = this[kHeaders].filter(([k], i) => i <= idx || k !== lower);
    }
  }
  get(name) {
    const lower = String(name).toLowerCase();
    const vals = this[kHeaders].filter(([k]) => k === lower).map(([, v]) => v);
    return vals.length ? vals.join(", ") : null;
  }
  getSetCookie() {
    return this[kHeaders].filter(([k]) => k === "set-cookie").map(([, v]) => v);
  }
  has(name) {
    const lower = String(name).toLowerCase();
    return this[kHeaders].some(([k]) => k === lower);
  }
  delete(name) {
    const lower = String(name).toLowerCase();
    this[kHeaders] = this[kHeaders].filter(([k]) => k !== lower);
  }
  forEach(cb, thisArg) {
    for (const [k, v] of this.entries()) cb.call(thisArg, v, k, this);
  }
  *entries() {
    const names = [...new Set(this[kHeaders].map(([k]) => k))].sort();
    for (const n of names) {
      if (n === "set-cookie") for (const v of this.getSetCookie()) yield [n, v];
      else yield [n, this.get(n)];
    }
  }
  *keys() {
    for (const [k] of this.entries()) yield k;
  }
  *values() {
    for (const [, v] of this.entries()) yield v;
  }
  [Symbol.iterator]() {
    return this.entries();
  }
  get [Symbol.toStringTag]() {
    return "Headers";
  }
  [Symbol.for("nodejs.util.inspect.custom")](depth, options) {
    return `Headers ${internal.inspect(Object.fromEntries(this.entries()), options)}`;
  }
}

// ---------------------------------------------------------------------------
// FormData (minimal)
// ---------------------------------------------------------------------------

class FormData {
  #entries = [];
  append(name, value, filename) {
    this.#entries.push([String(name), value instanceof internal.Blob ? value : String(value), filename]);
  }
  set(name, value, filename) {
    this.delete(name);
    this.append(name, value, filename);
  }
  get(name) {
    return this.#entries.find(([k]) => k === name)?.[1] ?? null;
  }
  getAll(name) {
    return this.#entries.filter(([k]) => k === name).map(([, v]) => v);
  }
  has(name) {
    return this.#entries.some(([k]) => k === name);
  }
  delete(name) {
    this.#entries = this.#entries.filter(([k]) => k !== name);
  }
  *entries() {
    for (const [k, v] of this.#entries) yield [k, v];
  }
  keys() {
    return this.#entries.map(([k]) => k)[Symbol.iterator]();
  }
  values() {
    return this.#entries.map(([, v]) => v)[Symbol.iterator]();
  }
  [Symbol.iterator]() {
    return this.entries();
  }
  async _encode() {
    const boundary = `----pi-jsrt-${ops.randomUUID()}`;
    const parts = [];
    for (const [k, v, filename] of this.#entries) {
      if (v instanceof internal.Blob) {
        const name = filename ?? v.name ?? "blob";
        parts.push(Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="${k}"; filename="${name}"\r\nContent-Type: ${v.type || "application/octet-stream"}\r\n\r\n`));
        parts.push(Buffer.from(await v.arrayBuffer()));
        parts.push(Buffer.from("\r\n"));
      } else {
        parts.push(Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="${k}"\r\n\r\n${v}\r\n`));
      }
    }
    parts.push(Buffer.from(`--${boundary}--\r\n`));
    return { bytes: Buffer.concat(parts), contentType: `multipart/form-data; boundary=${boundary}` };
  }
}

// ---------------------------------------------------------------------------
// Body handling
// ---------------------------------------------------------------------------

function streamFromBytes(bytes) {
  return new ReadableStream({
    start(c) {
      if (bytes.length) c.enqueue(bytes);
      c.close();
    },
  });
}

// Normalize a body init into { stream | bytes, contentType }.
function extractBody(body) {
  if (body === null || body === undefined) return { bytes: null, stream: null, contentType: null };
  if (typeof body === "string") return { bytes: ops.utf8Encode(body), contentType: "text/plain;charset=UTF-8" };
  if (body instanceof internal.URLSearchParams) return { bytes: ops.utf8Encode(body.toString()), contentType: "application/x-www-form-urlencoded;charset=UTF-8" };
  if (body instanceof ArrayBuffer) return { bytes: new Uint8Array(body.slice(0)), contentType: null };
  if (ArrayBuffer.isView(body)) return { bytes: new Uint8Array(body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength)), contentType: null };
  if (body instanceof internal.Blob) return { blob: body, contentType: body.type || null };
  if (body instanceof FormData) return { formData: body, contentType: null };
  if (body instanceof ReadableStream) return { stream: body, contentType: null };
  if (typeof body[Symbol.asyncIterator] === "function") return { stream: ReadableStream.from(body), contentType: null };
  return { bytes: ops.utf8Encode(String(body)), contentType: "text/plain;charset=UTF-8" };
}

async function readAll(stream) {
  const chunks = [];
  for await (const c of stream) chunks.push(typeof c === "string" ? ops.utf8Encode(c) : c instanceof Uint8Array ? c : new Uint8Array(c));
  return Buffer.concat(chunks);
}

const kBody = Symbol("body");

class BodyMixin {
  _initBody(init) {
    const extracted = extractBody(init);
    this[kBody] = { ...extracted, used: false, streamObj: null };
    return extracted.contentType;
  }
  get body() {
    const b = this[kBody];
    if (b.streamObj) return b.streamObj;
    if (b.stream) b.streamObj = b.stream;
    else if (b.bytes) b.streamObj = streamFromBytes(b.bytes);
    else if (b.blob) b.streamObj = b.blob.stream();
    else return null;
    return b.streamObj;
  }
  get bodyUsed() {
    return this[kBody].used;
  }
  async _consume() {
    const b = this[kBody];
    if (b.used) throw new TypeError("Body is unusable: Body has already been read");
    b.used = true;
    if (b.streamObj || b.stream) return readAll(b.streamObj ?? b.stream);
    if (b.bytes) return Buffer.from(b.bytes);
    if (b.blob) return Buffer.from(await b.blob.arrayBuffer());
    if (b.formData) return (await b.formData._encode()).bytes;
    return Buffer.alloc(0);
  }
  async arrayBuffer() {
    const b = await this._consume();
    return b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength);
  }
  async bytes() {
    return new Uint8Array(await this._consume());
  }
  async text() {
    return ops.utf8Decode(await this._consume(), false);
  }
  async json() {
    return JSON.parse(await this.text());
  }
  async blob() {
    return new internal.Blob([await this._consume()], { type: this.headers.get("content-type") ?? "" });
  }
  async formData() {
    const text = await this.text();
    const fd = new FormData();
    for (const [k, v] of new internal.URLSearchParams(text)) fd.append(k, v);
    return fd;
  }
}

function mixBody(Ctor) {
  for (const key of Object.getOwnPropertyNames(BodyMixin.prototype)) {
    if (key === "constructor") continue;
    Object.defineProperty(Ctor.prototype, key, Object.getOwnPropertyDescriptor(BodyMixin.prototype, key));
  }
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

class Request {
  constructor(input, init = {}) {
    let base = {};
    if (input instanceof Request) {
      base = { url: input.url, method: input.method, headers: input.headers, signal: input.signal, redirect: input.redirect, body: input[kBody] };
    } else {
      base.url = new internal.URL(String(input)).href;
    }
    this.url = base.url;
    this.method = String(init.method ?? base.method ?? "GET").toUpperCase();
    this.headers = new Headers(init.headers ?? base.headers);
    this.signal = init.signal ?? base.signal ?? new AbortController().signal;
    this.redirect = init.redirect ?? base.redirect ?? "follow";
    this.credentials = init.credentials ?? "same-origin";
    this.mode = init.mode ?? "cors";
    this.cache = init.cache ?? "default";
    this.referrer = init.referrer ?? "about:client";
    this.keepalive = Boolean(init.keepalive);
    // Node's Request always reports half duplex.
    this.duplex = "half";
    if (init.body !== undefined && init.body !== null && (this.method === "GET" || this.method === "HEAD")) {
      throw new TypeError("Request with GET/HEAD method cannot have body.");
    }
    if (init.body !== undefined) {
      const ct = this._initBody(init.body);
      if (this[kBody].stream && init.duplex == null) throw new TypeError("RequestInit: duplex option is required when sending a body.");
      if (ct && !this.headers.has("content-type")) this.headers.set("content-type", ct);
    } else if (base.body) {
      this[kBody] = { ...base.body, used: false, streamObj: null };
    } else {
      this._initBody(null);
    }
  }
  clone() {
    return new Request(this);
  }
  get [Symbol.toStringTag]() {
    return "Request";
  }
}
mixBody(Request);

class Response {
  constructor(body = null, init = {}) {
    const status = init.status ?? 200;
    if (status < 200 || status > 599) throw new RangeError(`init["status"] must be in the range of 200 to 599, inclusive.`);
    this.status = status;
    this.statusText = init.statusText ?? "";
    this.headers = new Headers(init.headers);
    this.type = "default";
    this.url = "";
    this.redirected = false;
    const ct = this._initBody(body);
    if (ct && !this.headers.has("content-type")) this.headers.set("content-type", ct);
  }
  get ok() {
    return this.status >= 200 && this.status < 300;
  }
  clone() {
    if (this.bodyUsed) throw new TypeError("Response.clone: Body has already been consumed.");
    const b = this[kBody];
    const r = new Response(null, { status: this.status, statusText: this.statusText, headers: this.headers });
    r.url = this.url;
    r.redirected = this.redirected;
    r.type = this.type;
    if (b.stream || b.streamObj) {
      const [a, c] = (b.streamObj ?? b.stream).tee();
      b.streamObj = a;
      b.stream = a;
      r[kBody] = { stream: c, used: false, streamObj: null };
    } else {
      r[kBody] = { ...b, used: false, streamObj: null };
    }
    return r;
  }
  static json(data, init = {}) {
    const headers = new Headers(init.headers);
    if (!headers.has("content-type")) headers.set("content-type", "application/json");
    return new Response(JSON.stringify(data), { ...init, headers });
  }
  static error() {
    const r = new Response(null, { status: 200 });
    r.status = 0;
    r.type = "error";
    return r;
  }
  static redirect(url, status = 302) {
    return new Response(null, { status, headers: { location: new internal.URL(url).href } });
  }
  get [Symbol.toStringTag]() {
    return "Response";
  }
}
mixBody(Response);

function abortReason(signal) {
  if (signal.reason !== undefined) return signal.reason;
  const err = new internal.DOMException("This operation was aborted", "AbortError");
  return err;
}

// ---------------------------------------------------------------------------
// undici dispatchers
// ---------------------------------------------------------------------------
//
// `fetch(url, { dispatcher })` and undici's `setGlobalDispatcher` take npm `undici` Agent,
// ProxyAgent and EnvHttpProxyAgent objects. Their settings are read back from the fields undici
// stores them in (symbols named "options", "proxy agent options", ...; the same in undici 6 to
// 8) and applied natively: proxy, TLS, connect/headers/body timeouts, HTTP/2 opt-in.

// undici 7+ keeps its global dispatcher under `.2` (and a v1-API wrapper of it under `.1`).
const kGlobalDispatchers = [Symbol.for("undici.globalDispatcher.2"), Symbol.for("undici.globalDispatcher.1")];
const UNDICI_DEFAULTS = { headersTimeout: 300e3, bodyTimeout: 300e3, connectTimeout: 10e3 };

function symbolNamed(obj, description) {
  for (let o = obj; o && o !== Object.prototype; o = Object.getPrototypeOf(o)) {
    for (const s of Object.getOwnPropertySymbols(o)) if (s.description === description) return s;
  }
  return undefined;
}

function slot(obj, description) {
  const s = symbolNamed(obj, description);
  return s === undefined ? undefined : obj[s];
}

function pemList(v) {
  if (v === undefined || v === null) return [];
  if (Array.isArray(v)) return v.flatMap(pemList);
  return [typeof v === "string" ? v : Buffer.from(v).toString("latin1")];
}

function timeoutOption(v, fallback) {
  return typeof v === "number" && v >= 0 ? v : fallback;
}

// undici's NO_PROXY matching (EnvHttpProxyAgent#shouldProxy), for objects the probe below
// cannot ask.
function bypassesProxy(url, noProxy) {
  const host = url.hostname.replace(/^\[(.+)\]$/, "$1").toLowerCase().replace(/(.)\.$/, "$1");
  const port = Number.parseInt(url.port, 10) || (url.protocol === "https:" ? 443 : 80);
  for (const entry of noProxy.split(/[,\s]/)) {
    if (!entry) continue;
    const m = /^\[(.+)\]:(\d+)$/.exec(entry) ?? (entry.split(":").length === 2 ? /^(.+):(\d+)$/.exec(entry) : null);
    const name = (m ? m[1] : entry.replace(/^\[(.+)\]$/, "$1")).replace(/^\*?\./, "").replace(/^(.+)\.$/, "$1").toLowerCase();
    const entryPort = m ? Number.parseInt(m[2], 10) : 0;
    if (entryPort && entryPort !== port) continue;
    if (name === "*") return true;
    if ((!entry.startsWith("*") && host === name) || host.endsWith(`.${name}`)) return true;
  }
  return false;
}

// The child agent an EnvHttpProxyAgent dispatches `url` to. Its own selection logic is asked
// by running its dispatch method against the children with their `dispatch` stubbed out for
// the call (synchronous, so nothing else can observe the stubs).
function envProxyChild(agent, url) {
  const children = [slot(agent, "no proxy agent"), slot(agent, "http proxy agent"), slot(agent, "https proxy agent")];
  const dispatch = slot(agent, "dispatch");
  let chosen;
  if (typeof dispatch === "function" && children.every((c) => c && typeof c === "object")) {
    const saved = children.map((c) => Object.getOwnPropertyDescriptor(c, "dispatch"));
    for (const c of children) {
      Object.defineProperty(c, "dispatch", { value: () => ((chosen ??= c), true), configurable: true, writable: true });
    }
    try {
      dispatch.call(agent, { origin: url.origin, path: url.pathname + url.search, method: "GET", headers: {} }, {});
    } catch {
      // The fallback below decides.
    } finally {
      children.forEach((c, i) => (saved[i] ? Object.defineProperty(c, "dispatch", saved[i]) : delete c.dispatch));
    }
  }
  if (chosen) return chosen;
  const env = internal.process?.env ?? {};
  if (bypassesProxy(url, env.no_proxy ?? env.NO_PROXY ?? "")) return children[0];
  return url.protocol === "https:" ? children[2] : children[1];
}

// `{ settings, headersTimeout, bodyTimeout }` for a request to `url` through `dispatcher`.
function dispatcherSettings(dispatcher, url) {
  for (let depth = 0; dispatcher && typeof dispatcher === "object" && depth < 4; depth++) {
    if (symbolNamed(dispatcher, "no proxy agent")) {
      dispatcher = envProxyChild(dispatcher, url);
      continue;
    }
    const proxy = slot(dispatcher, "proxy agent options");
    if (proxy && typeof proxy === "object" && proxy.uri) {
      const options = slot(slot(dispatcher, "proxy agent"), "options") ?? {};
      const tls = slot(dispatcher, "request tls settings") ?? {};
      const uri = new internal.URL(proxy.uri);
      if (uri.protocol === "http:" || uri.protocol === "https:") {
        // The credentials are already in the proxy headers as proxy-authorization.
        uri.username = "";
        uri.password = "";
      }
      const headers = Object.entries(slot(dispatcher, "proxy headers") ?? {}).map(([k, v]) => [String(k), String(v)]);
      return settingsFrom(options, tls, { uri: uri.href, headers, tunnel: url.protocol === "http:" && slot(dispatcher, "tunnel proxy") === true });
    }
    const options = slot(dispatcher, "options");
    if (options && typeof options === "object") {
      return settingsFrom(options, typeof options.connect === "object" && options.connect ? options.connect : {}, null);
    }
    break;
  }
  return null;
}

function settingsFrom(options, tls, proxy) {
  const connectTimeout = timeoutOption(tls.timeout ?? options.connectTimeout, UNDICI_DEFAULTS.connectTimeout);
  const settings = {
    envProxy: false,
    proxy,
    ca: pemList(tls.ca),
    cert: pemList(tls.cert),
    key: pemList(tls.key),
    passphrase: tls.passphrase === undefined ? null : String(tls.passphrase),
    rejectUnauthorized: tls.rejectUnauthorized === undefined ? null : Boolean(tls.rejectUnauthorized),
    connectTimeout,
    allowH2: options.allowH2 === true,
  };
  return {
    settings: JSON.stringify(settings),
    headersTimeout: timeoutOption(options.headersTimeout, UNDICI_DEFAULTS.headersTimeout),
    bodyTimeout: timeoutOption(options.bodyTimeout, UNDICI_DEFAULTS.bodyTimeout),
  };
}

// The plain `new Agent()` undici installs as the global dispatcher when its module loads and
// none is set. In TS Pi that never happens (Pi installs its EnvHttpProxyAgent first), so it
// keeps the host default below.
function isModuleDefaultAgent(dispatcher) {
  if (symbolNamed(dispatcher, "proxy agent options") || symbolNamed(dispatcher, "no proxy agent")) return false;
  const options = slot(dispatcher, "options");
  return Boolean(options) && typeof options === "object" && Object.entries(options).every(([k, v]) => v === undefined || (k === "maxOrigins" && v === Infinity));
}

function globalDispatcher() {
  for (const key of kGlobalDispatchers) {
    const d = globalThis[key];
    if (d) return isModuleDefaultAgent(d) ? null : d;
  }
  return null;
}

// No dispatcher: Node's defaults, with the environment proxy honoured as Pi's global
// EnvHttpProxyAgent does.
const DEFAULT_SETTINGS = {
  settings: JSON.stringify({ envProxy: true, connectTimeout: UNDICI_DEFAULTS.connectTimeout }),
  headersTimeout: UNDICI_DEFAULTS.headersTimeout,
  bodyTimeout: UNDICI_DEFAULTS.bodyTimeout,
};

// Fetch's "bad port" list: such requests fail before any connection is made.
const BAD_PORTS = new Set(
  "1 7 9 11 13 15 17 19 20 21 22 23 25 37 42 43 53 69 77 79 87 95 101 102 103 104 109 110 111 113 115 117 119 123 135 137 139 143 161 179 389 427 465 512 513 514 515 526 530 531 532 540 548 554 556 563 587 601 636 989 990 993 995 1719 1720 1723 2049 3659 4045 4190 5060 5061 6000 6566 6665 6666 6667 6668 6669 6679 6697 10080".split(" "),
);

function chunkBytes(c) {
  if (c instanceof Uint8Array) return c;
  if (typeof c === "string") return ops.utf8Encode(c);
  if (c instanceof ArrayBuffer) return new Uint8Array(c);
  if (ArrayBuffer.isView(c)) return new Uint8Array(c.buffer, c.byteOffset, c.byteLength);
  throw new TypeError("Received non-Uint8Array chunk");
}

// Feed a streamed request body to the native request, chunk by chunk with backpressure.
async function pumpUpload(id, stream, failed) {
  const reader = stream.getReader();
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (!(await ops.fetchWrite(id, chunkBytes(value)))) {
        reader.cancel().catch(() => {});
        return;
      }
    }
    await ops.fetchWrite(id, null);
  } catch (err) {
    failed(err);
    ops.fetchWriteError(id, String(err?.message ?? err));
  } finally {
    reader.releaseLock();
  }
}

async function fetch(input, init = {}) {
  const request = input instanceof Request && Object.keys(init).length === 0 ? input : new Request(input, init);
  const signal = request.signal;
  if (signal?.aborted) throw abortReason(signal);
  const url = new internal.URL(request.url);
  if (url.protocol === "data:") {
    const m = /^data:([^,]*?)(;base64)?,(.*)$/s.exec(request.url);
    if (!m) throw new TypeError("fetch failed");
    const bytes = m[2] ? ops.base64Decode(m[3], false) : ops.utf8Encode(decodeURIComponent(m[3]));
    return new Response(bytes, { status: 200, headers: { "content-type": m[1] || "text/plain;charset=US-ASCII" } });
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new TypeError("fetch failed", { cause: new Error(`unknown scheme ${url.protocol}`) });
  }
  if (BAD_PORTS.has(url.port)) throw new TypeError("fetch failed", { cause: new Error("bad port") });
  const dispatcher = init?.dispatcher ?? globalDispatcher();
  const { settings, headersTimeout, bodyTimeout } = (dispatcher && dispatcherSettings(dispatcher, url)) || DEFAULT_SETTINGS;
  let bodyArg = null;
  let upload = null;
  const b = request[kBody];
  if (b.formData) {
    const enc = await b.formData._encode();
    bodyArg = enc.bytes;
    if (!request.headers.has("content-type")) request.headers.set("content-type", enc.contentType);
  } else if (b.stream || b.streamObj) {
    if (b.used) throw new TypeError("Body is unusable: Body has already been read");
    b.used = true;
    upload = b.streamObj ?? b.stream;
    bodyArg = true;
  } else if (b.bytes || b.blob) {
    bodyArg = await request._consume();
  }
  if (!request.headers.has("accept")) request.headers.set("accept", "*/*");
  if (!request.headers.has("user-agent")) request.headers.set("user-agent", "node");
  const headerPairs = [];
  for (const [k, v] of request.headers[kHeaders]) headerPairs.push(k, v);
  const id = ops.fetchId();
  let onAbort = null;
  const aborted = new Promise((_, reject) => {
    onAbort = () => {
      ops.fetchCancel(id);
      reject(abortReason(signal));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
  aborted.catch(() => {});
  let uploadError;
  let head;
  try {
    const started = ops.fetchStart(id, request.method, url.href, headerPairs, bodyArg, request.redirect, settings, headersTimeout, bodyTimeout);
    if (upload) pumpUpload(id, upload, (e) => (uploadError ??= e));
    head = await Promise.race([started, aborted]);
  } catch (err) {
    signal?.removeEventListener("abort", onAbort);
    if (err?.name === "AbortError" && signal?.aborted) throw abortReason(signal);
    if (err instanceof TypeError) throw err;
    if (err?.name === "TypeError") throw new TypeError(err.message);
    throw new TypeError("fetch failed", { cause: uploadError ?? err });
  }
  const headers = new Headers();
  for (let i = 0; i < head.headers.length; i += 2) headers.append(head.headers[i], head.headers[i + 1]);
  const nullBody = request.method === "HEAD" || head.status === 204 || head.status === 304;
  const stream = nullBody
    ? null
    : new ReadableStream({
        async pull(controller) {
          try {
            const chunk = await ops.fetchRead(id);
            if (chunk === null) {
              signal?.removeEventListener("abort", onAbort);
              controller.close();
            } else controller.enqueue(chunk);
          } catch (err) {
            signal?.removeEventListener("abort", onAbort);
            if (signal?.aborted) controller.error(abortReason(signal));
            else if (err?.name === "AbortError") controller.error(err);
            else controller.error(new TypeError("terminated", { cause: err }));
          }
        },
        cancel() {
          ops.fetchCancel(id);
        },
      },
      // Pull only when the body is read, so a host taking the body over (see `__piFetchId`)
      // finds it untouched instead of already drained by a prefetching stream.
      { highWaterMark: 0 });
  if (nullBody) ops.fetchCancel(id);
  const response = new Response(null, { status: Math.max(200, head.status), statusText: head.statusText, headers });
  response.status = head.status;
  response[kBody] = { stream, used: false, streamObj: null, bytes: null };
  // A host may take the body over instead of copying it through JS (see `take_fetch_body`).
  Object.defineProperty(response, "__piFetchId", { value: id, configurable: true });
  response.url = head.url;
  response.redirected = head.url !== url.href;
  response.type = "basic";
  return response;
}

// The pristine implementation, so a host can tell a patch from the real thing (and extensions may
// keep calling it). Also the value a "native" tag marks for modules that re-export it.
ops.nativeFetch = fetch;
Object.defineProperty(fetch, "__piNativeFetch", { value: true, configurable: true });

for (const [name, value] of Object.entries({
  ReadableStream,
  ReadableStreamDefaultReader,
  ReadableStreamDefaultController,
  WritableStream,
  TransformStream,
  TextDecoderStream,
  TextEncoderStream,
  CountQueuingStrategy,
  ByteLengthQueuingStrategy,
  Headers,
  Request,
  Response,
  FormData,
  fetch,
})) {
  internal.hidden(globalThis, name, value);
}
