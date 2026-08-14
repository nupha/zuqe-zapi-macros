# zapi-macros

A collection of procedural macros for zuqe native extensions (zapi cdylib). It lets
extension authors declare JS-callable functions, instantiable classes, and exportable
values using **only safe Rust**, without touching `extern "C"`, raw pointers,
trampolines, or other FFI details.

- Companion crates: `zapi` (ABI contract + safe layer), `zapi-macros` (this crate, procedural macros).
- Reference projects: `~/devel/zhuque/examples/zapi_basic`, `zapi_event`; `~/workspace/demo_llm/aike-tools`.

---

## Design goals

1. **Zero raw FFI**: the macros generate the trampolines, `ZqExportDef`, and
   `zq_module_register`; authors only touch safe types like `ZqCtx` / `ZqValue` /
   `ZqCallInfo`.
2. **Explicit export list**: `#[zq_module(exports = [...])]` — one list decides which
   items the module **actually exports** (covering functions/classes/values), with
   compile-time checks that each item generated a `ZqExportItem`; no external
   dependencies like linkme required.
3. **Foolproof GC**: once a class declares a `payload`, `gc_trace` is mounted
   automatically by default (forgetting to mount it = silent UAF); when the payload is
   known to have no GC nodes you can explicitly use `payload_no_gc` to skip the
   per-instance trace calls.
4. **Extensible**: the `#[zq_module]` init hook carries module-load initialization
   (e.g. registering a `thread_local` GC trace).
5. **Single-module model**: one dylib = one module (the sole `zq_module_register`
   symbol); module identity is determined by the dylib's absolute path URL, `name` is
   only decorative.

## Cargo.toml configuration (extension crate)

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
zapi = { path = "<path to the zapi crate>" }
zapi-macros = { path = "<path to this crate>" }
```

No other required dependencies (linkme not needed).

---

## Macro overview

| Macro                  | Purpose                                                                                        | Attached to    |
| ---------------------- | ---------------------------------------------------------------------------------------------- | -------------- |
| `#[zq_function]`       | Declares a Rust function as JS-callable and makes it **exportable** (generates `ZqExportItem`) | function       |
| `#[zq_export]`         | Declares a lazily built **JS value export**                                                    | function       |
| `#[zq_class]`          | Declares a native class (payload + GC trace)                                                   | struct         |
| `#[zq_class_impl]`     | Fills in class behavior (constructor + methods)                                                | impl block     |
| `#[zq_method]`         | Marks an instance method (on the prototype)                                                    | method in impl |
| `#[zq_static_method]`  | Marks a static method/accessor (on the constructor)                                            | method in impl |
| `#[zq_module]`         | Generates the module entry; `exports = [...]` decides actual exports; may carry an init hook   | function       |
| `#[derive(ZqGcTrace)]` | GC trace derivation for payloads                                                               | struct         |

> **"Export" semantics**: `#[zq_function]` / `#[zq_export]` / `#[zq_class]` only make an
> item **exportable** (generating a `ZqExportItem`); it is only actually `import`ed into
> the module namespace once the item is listed in `#[zq_module(exports = [...])]`.

---

## Detailed macro docs

### `#[zq_function]`

**Purpose**: declares a Rust function as a JS-callable native function. Note that it
only makes it **exportable** (generating a `ZqExportItem`); whether it is actually
exported is decided by `#[zq_module(exports = [...])]`.

**Signature** (required):

```rust
#[zq_function(name = "add", magic = 0)]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError>
```

- `cx`: callback context (by value, Copy); `cx.string()/number()/object()/array()` build values.
- `cc`: arguments/this; `cc.arg(&cx, i)`, `cc.arg_as::<T>(&cx, i)`, `cc.len()`, `cc.this(&cx)`.
- Returns `Result<ZqValue, ZqError>`; an `Err` is thrown back to JS at the boundary.

The macro generates: the FFI trampoline, a `ZqFunction` impl, and
`ZqExportItem::Func` (`::item()`).

**Example**:

```rust
#[zq_function(name = "add")]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg_as::<f64>(&cx, 0)?;
    let b = cc.arg_as::<f64>(&cx, 1)?;
    Ok(cx.number(a + b))
}

#[zq_function(name = "echoString")]
fn echo_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg_as::<String>(&cx, 0)?;
    Ok(cx.string(&format!("echo:{s}")))
}
```

On the JS side: `import { add, echoString } from "./x.dylib"; add(1, 2)`.

### `#[zq_export]`

**Purpose**: declares a **lazily built** JS value export (again, only actually exported
once referenced by `exports = [...]`). The value is built with a real `ZqEnv` during
module evaluation, so it can be any JS value (string, number, object, array…). The
result is held by the module's export binding and traced by the engine's GC — no need
to manage liveness yourself.

**Signature** (required):

```rust
#[zq_export(name = "VERSION")]
fn version<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError>
```

The macro generates: the value-building trampoline, a marker type + `ZqExportItem::Value`
(`::item()`).

**Example**:

```rust
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cx.object();
    obj.set(&cx, "version", "0.1.0")?;
    obj.set(&cx, "copyright", "© 2026 aike")?;
    Ok(obj)
}
```

On the JS side: `demo_obj.version` / `demo_obj.copyright`.

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
use zapi::prelude::*;
use zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method};

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
        let label = cc.arg(&cx, 0).raw();
        Counter::new_instance(&cx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this(&cx);
        let p = Counter::payload_mut(&cx, c).ok_or_else(|| ZqError::new(ZqStatus::Error, None))?;
        p.count += 1;
        Ok(cx.number(p.count as f64))
    }

    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(cx.bool(Counter::payload(&cx, cc.arg(&cx, 0)).is_some()))
    }
}
```

Helpers generated by `#[zq_class]`: `class_def()`, `register(&ZqCtx)`,
`new_instance(&ZqCtx, payload)`, `payload(&ZqCtx, val)` / `payload_mut(...)`.

Stateless class (no payload) example:

```rust
#[zq_class(name = "Marker")]
struct Marker;

#[zq_class_impl]
impl Marker {
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Marker::new_instance(&cx)
    }
}
```

**Accessors (get/set) and magic dispatch**: `#[zq_method(get, set)]` generates
accessors; `#[zq_method(name = "fire", magic = 1)]` + `#[zq_method(name = "spark", magic = 2)]`
let a single body serve multiple JS names, dispatched by `cc.magic()`.

### `#[zq_module]`

**Purpose**: generates the module entry `zq_module_register` (called by the host after
dlopen), and carries optional module-load initialization.

**Parameters**:

- `exports`: the module's **actual export** list. **The standard form is an inline list**
  `exports = [f1, c1, v1, ...]` (one entry each for `#[zq_function]` / `#[zq_class]` /
  `#[zq_export]`, bare item names; the macro does the `::item()` wrapping). Advanced
  option: reference a `const &[ZqExportItem]` slice (each entry explicitly `::item()`),
  handy for splitting the list into a separate file, composing it with `#[cfg]`, or
  reusing it.
- `name`: decorative module name (host identity is the dylib path, not this name).

```rust
// Standard form: inline list
#[zq_module(name = "x", exports = [grep, Counter, version])]
fn module_init() {}

// Advanced (optional): named const slice, each entry explicitly `::item()`
const EXPORTS: &[ZqExportItem] = &[grep::item(), Counter::item(), version::item()];
#[zq_module(name = "x", exports = EXPORTS)]
fn module_init() {}
```

**Init hook**: the annotated function is **always** the module init hook (name is
arbitrary, **standard is `module_init`**), **always registered**, run once with a real
`ZqEnv` during module evaluation; an empty body is just empty initialization
(registered once, run once, no-op, no performance impact). Two signatures:

```rust
// With init logic: gets the ZqEnv (e.g. registering a thread_local GC trace)
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

### `#[derive(ZqGcTrace)]`

**Purpose**: derives GC trace for payload structs. **Essence**: recursive forwarding —
calls `ZqGcTrace::trace(&self.field, ctx)` for each field, and the **field type** decides
whether anything is actually reported:

- `ZqRawValue` → `mark` if non-zero (actually reported);
- `i32` / `bool` / `String` etc. → no-op (no GC nodes);
- `Vec<T>` / `Option<T>` / `Box<T>` → recurses into elements;
- any field type that doesn't implement `ZqGcTrace` → **compile error** (the derive
  never skips any field).

Nested custom types: put `#[derive(ZqGcTrace)]` on every layer; hand-write the impl for
layers that can't use the derive. Foreign/container types without an impl (like
`HashMap`): hand-write `impl ZqGcTrace` in the containing type, explicitly `ctx.mark`ing
the real GC nodes.

---

## Complete example: `grep` extension (aike-tools)

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
zapi = "0.1"
zapi-macros = "0.1"
grep-matcher = "0.1"
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
glob = "0.3"
```

`src/lib.rs` (excerpt):

```rust
use zapi::prelude::*;
use zapi_macros::{zq_export, zq_function, zq_module};

// Declare a JS-callable native function (can be exported by the module)
#[zq_function(name = "grep")]
fn grep<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let pattern = cc.arg_as::<String>(&cx, 0)?;
    // ...ripgrep-style search, building a results array...
    Ok(cx.array(&results))
}

// Declare a value export (lazily built object)
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cx.object();
    obj.set(&cx, "version", "0.1.0")?;
    obj.set(&cx, "copyright", "© 2026 aike")?;
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

`set_gc_trace_func` (zapi ABI) + a hand-written callback, registered in the
`#[zq_module]` init:

```rust
thread_local! { static STASH: Cell<ZqRawValue> = const { Cell::new(0) }; }

// Callback: on every GC, report the non-zero raw in thread_local as a mark
unsafe extern "C" fn trace_stash(tc: *mut zapi::ffi::ZqGcTraceCtxS, _o: *mut core::ffi::c_void) {
    let raw = STASH.with(|c| c.get());
    if raw != 0 {
        let ctx = unsafe { ZqGcTraceCtx::from_raw(tc) };
        let _ = ctx.mark(raw);
    }
}

#[zq_module(name = "uaf_probe", exports = [store, peek, clear])]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    cx.set_gc_trace_func(Some(trace_stash), std::ptr::null_mut())
}
```

Registration must live in the init: only init has the `ZqEnv`, and it's called before any
exports. The host adds the callback to the Runtime's GC trace list, invoked during the
mark phase of every GC.

### Cost of an empty trace

An empty `ZqGcTrace` impl (no `mark`) **crosses no FFI and passes no values**; what
remains is a single function-pointer indirect call + check per instance per GC. With
`payload_no_gc` / no payload, this whole chain is skipped.
