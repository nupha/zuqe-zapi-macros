# zapi-macros

A collection of procedural macros for zuqe native extensions (zapi cdylib). It lets
extension authors declare JS-callable functions, instantiable classes, and exportable
values using **only safe Rust**, without touching `extern "C"`, raw pointers,
trampolines, or other FFI details.

- Companion crates: `zuqe_zapi` (ABI contract + safe layer), `zuqe_zapi_macros`
  (this crate, procedural macros).
- Reference projects: `~/devel/zhuque/examples/zapi_basic`, `zapi_event`; `~/workspace/demo_llm/aike-extensions` (module `tools`).

---

## Design goals

1. **Zero raw FFI**: the macros generate the trampolines, `ZqFieldDef`, and
   `zq_module_register`; authors only touch safe types like `ZqCtx` / `ZqValue` /
   `ZqCallInfo`.
2. **Explicit export list**: `#[zq_module(exports = [...])]` — one list decides which
   items the module **actually exports** (covering functions/classes/values), with
   compile-time checks that each item generated a `ZqFieldDef`; no external
   dependencies like linkme required.
3. **Foolproof GC**: once a class declares a `payload`, `gc_trace` is mounted
   automatically by default (forgetting to mount it = silent UAF); when the payload is
   known to have no GC nodes you can explicitly use `payload_no_gc` to skip the
   per-instance trace calls.
4. **Extensible**: the `#[zq_module]` init hook carries module-load initialization
   (e.g. registering a `thread_local` GC trace), and its optional `finalize` hook
   carries per-runtime teardown.
5. **Single-module model**: one dylib = one module (the sole `zq_module_register`
   symbol); module identity is determined by the dylib's absolute path URL (also
   readable from Rust via `zuqe_zapi::module_path()`), `name` is only decorative.

## Cargo.toml configuration (extension crate)

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
zuqe_zapi = "0.3"
zuqe_zapi_macros = "0.3"
```

---

## Macro overview

| Macro                  | Purpose                                                                                                                  | Attached to    |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------ | -------------- |
| `#[zq_function]`       | Declares a Rust function as JS-callable and makes it **exportable** (generates `ZqFieldDef`)                             | function       |
| `#[zq_export]`         | Declares a lazily built **JS value export**                                                                              | function       |
| `#[zq_class]`          | Declares a native class (payload + GC trace)                                                                             | struct         |
| `#[zq_class_impl]`     | Fills in class behavior (constructor + methods)                                                                          | impl block     |
| `#[zq_method]`         | Marks an instance method/accessor (on the prototype)                                                                     | method in impl |
| `#[zq_static_method]`  | Marks a static method/accessor (on the constructor)                                                                      | method in impl |
| `#[zq_module]`         | Generates the module entry; `exports = [...]` decides actual exports; may carry an init hook and a runtime-finalize hook | function       |
| `#[derive(ZqGcTrace)]` | GC trace derivation for payloads                                                                                         | struct         |

> **"Export" semantics**: `#[zq_function]` / `#[zq_export]` / `#[zq_class]` only make an
> item **exportable** (generating a `ZqFieldDef`, via each item's `item()` / `items()`); it
> is only actually `import`ed into the module namespace once the item is listed in
> `#[zq_module(exports = [...])]`.

---

## Detailed macro docs

### `#[zq_function]`

**Purpose**: declares a Rust function as a JS-callable native function. Note that it
only makes it **exportable** (generating a `ZqFieldDef`); whether it is actually
exported is decided by `#[zq_module(exports = [...])]`.

**Signature** (required):

```rust
#[zq_function(name = "add")]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError>
```

- `cx`: callback context (by value, Copy); build values with `ZqValue::number(n)`,
  `ZqValue::bool(b)`, `ZqValue::new_string(cx, s)?`, `ZqValue::new_object(cx)?`,
  `ZqValue::new_array(cx, &[...])?`, `ZqValue::undefined()`, etc.
- `cc`: arguments/this; `cc.arg(i)` returns the raw `ZqValue`, typed helpers
  `cc.arg_f64(cx, i)`, `cc.arg_i32(...)`, `cc.arg_bool(...)`, `cc.arg_string(...)`, plus
  `cc.len()`, `cc.this()`, `cc.magic()`.
- Optional `magic = N` registers the dispatch value readable via `cc.magic()` (see
  `#[zq_method]` magic dispatch below).
- Returns `Result<ZqValue, ZqError>`; an `Err` is thrown back to JS at the boundary.

**Stacking for several JS names**: repeat the attribute on the same function to publish
one body under multiple JS names. Each stacked `#[zq_function]` contributes its own
`name` / `magic`; the body reads `cc.magic()` to tell which name it was called under. The
names must be unique (a duplicate is a compile error). List the function once in `exports`
and every stacked name is exported.

```rust
#[zq_function(name = "fire", magic = 1)]
#[zq_function(name = "spark", magic = 2)]
fn ignite<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    match cc.magic() {
        1 => Ok(ZqValue::new_string(cx, "fire")?),
        _ => Ok(ZqValue::new_string(cx, "spark")?),
    }
}
```

The macro generates: the FFI trampoline, a marker struct exposing `item()` (the first
`ZqFieldDef::Func`), `items()` (every name a stacked attribute added), `call()` (invokes
the Rust fn directly), and `trampoline()`.

**Example**:

```rust
#[zq_function(name = "add")]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg_f64(cx, 0)?;
    let b = cc.arg_f64(cx, 1)?;
    Ok(ZqValue::number(a + b))
}

#[zq_function(name = "echoString")]
fn echo_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg_string(cx, 0)?;
    ZqValue::new_string(cx, &format!("echo:{s}"))
}
```

On the JS side: `import { add, echoString } from "./x.dylib"; add(1, 2)`.

### `#[zq_export]`

**Purpose**: declares a **lazily built** JS value export (again, only actually exported
once referenced by `exports = [...]`). The value is built with a real `ZqCtx` during
module evaluation, so it can be any JS value (string, number, object, array…). The
result is held by the module's export binding and traced by the engine's GC — no need
to manage liveness yourself.

**Signature** (required):

```rust
#[zq_export(name = "VERSION")]
fn version<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError>
```

The macro generates: the value-building trampoline, a marker type exposing
`item()` (`ZqFieldDef::Value`) and `items()`.

**Example**:

```rust
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = ZqValue::new_object(cx)?;
    obj.set_property_str(cx, "version", ZqValue::new_string(cx, "0.1.0")?)?;
    obj.set_property_str(cx, "copyright", ZqValue::new_string(cx, "© 2026 aike")?)?;
    Ok(obj)
}
```

On the JS side: `demo_obj.version` / `demo_obj.copyright`.

> `ZqValue` converts from `i32`/`u32`/`f64`/`bool` automatically; strings and other
> JS values must be built explicitly (`ZqValue::new_string(cx, ...)?`), e.g. for
> `set_property_str` / `set_property`.

### `#[zq_class]` + `#[zq_class_impl]` + `#[zq_method]` + `#[zq_static_method]`

**Purpose**: declares a native class. `#[zq_class]` declares the class and its payload;
`#[zq_class_impl]` fills in the behavior (`constructor` is recognized automatically as
the constructor, `#[zq_method]` goes on the prototype, `#[zq_static_method]` goes on
the constructor).

**`#[zq_class]` parameters**:

- `name`: JS class name (defaults to the struct name).
- `payload`: per-instance native state type (optional; without it the class is stateless).
- `payload_no_gc`: explicitly declares that the payload has no GC nodes, skipping
  `gc_trace` mounting (saves per-instance calls). Default is **mounted** (foolproof).

**Payload GC trace**: once a payload is declared, the `gc_trace` callback is mounted
automatically by default; the payload struct is `#[derive(ZqGcTrace)]`, and fields
holding JS values use `ZqRawValue` (auto `mark`ed during trace).

**Example** (a simplified EventEmitter):

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{ZqGcTrace, zq_class, zq_class_impl, zq_method, zq_static_method};

#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,            // JS value → needs GC trace
}

#[zq_class(name = "Counter", payload = CounterPayload)]
struct Counter;

#[zq_class_impl]
impl Counter {
    fn constructor<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(0).raw();
        Counter::new_instance(cx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = Counter::payload_mut(cx, cc.this())
            .ok_or_else(|| cx.throw_type("not a Counter"))?;
        p.count += 1;
        Ok(ZqValue::number(p.count as f64))
    }

    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(ZqValue::bool(Counter::payload(cx, cc.arg(0)).is_some()))
    }
}
```

Helpers generated by `#[zq_class]`: `class_def()`, `item()`, `items()`,
`register(ZqCtx)`, `new_instance(ZqCtx, payload)`, and (with payload)
`payload(ZqCtx, val)` / `payload_mut(ZqCtx, val)` / `take_payload(ZqCtx, val)`.
`take_payload` detaches the boxed payload from the instance (release it early instead of
waiting for GC); the instance is left without a payload afterwards.

Stateless class (no payload) example:

```rust
#[zq_class(name = "Marker")]
struct Marker;

#[zq_class_impl]
impl Marker {
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Marker::new_instance(cx)
    }
}
```

**Accessors (get/set) and magic dispatch**: `#[zq_method(get, set)]` (also on
`#[zq_static_method]`) generates accessors, which compile to `ZqFieldDef::getset`;
`#[zq_method(name = "fire", magic = 1)]` + `#[zq_method(name = "spark", magic = 2)]` on a
single method let one body serve multiple JS names, dispatched by `cc.magic()`.

### `#[zq_module]`

**Purpose**: generates the module entry `zq_module_register` (called by the host after
dlopen), and carries optional module-load initialization.

**Parameters**:

- `exports`: the module's **actual export** list. **The standard form is an inline list**
  `exports = [f1, c1, v1, ...]` (one entry each for `#[zq_function]` / `#[zq_class]` /
  `#[zq_export]`, bare item names; the macro expands each entry to its `::items()` and
  concatenates the slices). Advanced option: reference a `const`/`static` slice
  (`&[ZqFieldDef]` or `[ZqFieldDef; _]`, each entry explicitly `::item()`), handy for
  splitting the list into a separate file, composing it with `#[cfg]`, or reusing it.
- `name`: decorative module name (optional; host identity is the dylib path, not this
  name).
- `finalize`: optional path to a `fn(*mut ZqRuntime)` runtime-finalize hook, called once
  per runtime that imported this module when that runtime is torn down. Use it to release
  runtime-scoped native state (thread_locals, boxed GC-trace opaques); it must not call
  any JS API.

```rust
// Standard form: inline list
#[zq_module(name = "x", exports = [grep, Counter, version])]
fn module_init() {}

// Advanced (optional): named const slice, each entry explicitly `::item()`
const EXPORTS: &[ZqFieldDef] = &[grep::item(), Counter::item(), version::item()];
#[zq_module(name = "x", exports = EXPORTS)]
fn module_init() {}
```

**Init hook**: the annotated function is **always** the module init hook (name is
arbitrary, **standard is `module_init`**), **always registered**, run once with a real
`ZqCtx` during module evaluation; an empty body is just empty initialization
(registered once, run once, no-op, no performance impact). Two signatures:

```rust
// With init logic: gets the ZqCtx (e.g. registering a thread_local GC trace)
#[zq_module(name = "x", exports = [grep, demo_obj])]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> { ... }

// No init logic: empty initialization (still registered, still runs once, just does nothing)
#[zq_module(name = "x", exports = [grep, demo_obj])]
fn module_init() {}
```

**Example**:

```rust
#[zq_module(name = "tools", exports = [grep, demo_obj])]
fn module_init() {}
```

**Finalize hook**: pass `finalize = path_to_fn` to register `fn(*mut ZqRuntime)`, called
once per runtime that imported this module when that runtime is torn down:

```rust
fn cleanup(rt: *mut zuqe_zapi::ZqRuntime) {
    // release per-runtime native state; must not call any JS API
}

#[zq_module(name = "x", exports = [grep], finalize = cleanup)]
fn module_init() {}
```

### `#[derive(ZqGcTrace)]`

**Purpose**: derives GC trace for payload structs. **Essence**: recursive forwarding —
calls `ZqGcTrace::trace(&self.field, ctx)` for each field, and the **field type** decides
whether anything is actually reported:

- `ZqRawValue` → `mark` (no-op when the raw handle is a non-object / 0);
- `i32` / `i64` / `f64` / `bool` / `String` / `*mut c_void` → no-op (no GC nodes);
- `Vec<T>` / `Option<T>` / `Box<T>` → recurses into elements;
- any field type that doesn't implement `ZqGcTrace` → **compile error** (the derive
  never skips any field).

Nested custom types: put `#[derive(ZqGcTrace)]` on every layer; hand-write the impl for
layers that can't use the derive. Foreign/container types without an impl (like
`HashMap`): hand-write `impl ZqGcTrace` in the containing type, explicitly `ctx.mark`ing
the real GC nodes.

---

## Complete example: `grep` extension (aike-extensions)

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
zuqe_zapi = "0.3"
zuqe_zapi_macros = "0.3"
grep-matcher = "0.1"
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
glob = "0.3"
```

`src/lib.rs` (excerpt):

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_export, zq_function, zq_module};

// Declare a JS-callable native function (can be exported by the module)
#[zq_function(name = "grep")]
fn grep<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let pattern = cc.arg_string(cx, 0)?;
    // ...ripgrep-style search, collecting hit objects...
    let mut hits = Vec::new();
    // hits.push(hit_object);
    ZqValue::new_array(cx, &hits)
}

// Declare a value export (lazily built object)
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = ZqValue::new_object(cx)?;
    obj.set_property_str(cx, "version", ZqValue::new_string(cx, "0.1.0")?)?;
    obj.set_property_str(cx, "copyright", ZqValue::new_string(cx, "© 2026 aike")?)?;
    Ok(obj)
}

// Module entry: the exports list decides the actual exports
#[zq_module(name = "tools", exports = [grep, demo_obj])]
fn module_init() {}
```

On the JS side:

```js
import { grep, demo_obj } from "./tools.dylib";

const hits = grep("alpha", { cwd: "/path", include: ["*.txt"] });
console.log(hits[0].file, hits[0].lineNumber, hits[0].line);
console.log(demo_obj.version, demo_obj.copyright);
```

---

## GC trace topics

### Why it's needed

JS values are GC nodes. When an extension holds raw handles on the native side (payload,
`thread_local`, statics), the engine's GC **does not automatically scan those places**;
if a value is collected, the dangling handle is a use-after-free.

### class payload

- Automatic: `#[zq_class(payload = T)]` mounts the `gc_trace` callback by default;
  `T` is marked `#[derive(ZqGcTrace)]`, fields use `ZqRawValue` → auto `mark`. Forgetting
  = safe (foolproof).
- Optimization: when the payload is known to have no GC nodes, use
  `#[zq_class(payload = T, payload_no_gc)]` to skip per-instance trace calls; in that case
  `T` doesn't even need to implement `ZqGcTrace`.
- Classes without a payload always have `gc_trace: None` (no state to trace).

### thread_local (currently hand-written)

`ZqCtx::gc_set_trace_func` (zapi ABI) + a hand-written callback, registered in the
`#[zq_module]` init:

```rust
use std::cell::Cell;
use zuqe_zapi::{ZqGcTraceCtx, ZqRawValue};

thread_local! { static STASH: Cell<ZqRawValue> = const { Cell::new(0) }; }

// Callback: on every GC, report the non-zero raw in thread_local as a mark
unsafe extern "C" fn trace_stash(gcx: *mut zuqe_zapi::ffi::ZqGcTraceCtxS, _opaque: *mut core::ffi::c_void) {
    let raw = STASH.with(|c| c.get());
    if raw != 0 {
        let ctx = ZqGcTraceCtx::from(gcx);
        let _ = ctx.mark(raw);
    }
}

#[zq_module(name = "uaf_probe", exports = [store, peek, clear])]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    cx.gc_set_trace_func(Some(trace_stash), std::ptr::null_mut())
}
```

Registration must live in the init: only init has the `ZqCtx`, and it's called before any
exports. The host adds the callback to the Runtime's GC trace list, invoked during the
mark phase of every GC.

### Cost of an empty trace

An empty `ZqGcTrace` impl (no `mark`) **crosses no FFI and passes no values**; what
remains is a single function-pointer indirect call + check per instance per GC. With
`payload_no_gc` / no payload, this whole chain is skipped.
