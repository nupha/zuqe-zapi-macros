# zapi-macros

给 zuqe 原生扩展（zapi cdylib）用的过程宏集合。让扩展作者**只用安全 Rust**
声明 JS 可调用的函数、可实例化的类与可导出的值，完全不碰 `extern "C"`、裸指针、
trampoline 等 FFI 细节。

- 配套 crate：`zapi`（ABI 契约 + 安全糖层）、`zapi-macros`（本 crate，过程宏）。
- 参考工程：`~/devel/zhuque/examples/zapi_basic`、`zapi_event`；`~/workspace/demo_llm/aike-tools`。

---

## 设计目的

1. **零 raw FFI**：宏负责生成 trampoline、`ZqExportDef`、`zq_module_register`，作者只碰
   `ZqCtx` / `ZqValue` / `ZqCallInfo` 等安全类型。
2. **显式导出清单**：`#[zq_module(exports = [...])]` 一个列表决定模块**实际导出**
   哪些项（覆盖函数/类/值），编译期校验每一项都生成了 `ZqExportItem`；不需要 linkme
   之类的外部依赖。
3. **防呆 GC**：类一旦声明了 `payload`，默认自动挂载 `gc_trace`（忘了挂 = 静默 UAF）；
   确知 payload 无 GC 节点时可显式 `payload_no_gc` 省掉逐实例的 trace 调用。
4. **可扩展**：`#[zq_module]` 的 init 钩子承载模块加载初始化（如注册
   `thread_local` 的 GC trace）。
5. **单模块模型**：一个 dylib = 一个模块（`zq_module_register` 唯一符号），模块身份
   由 dylib 的绝对路径 URL 决定，`name` 只是装饰。

## Cargo.toml 配置（扩展 crate）

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
zapi = { path = "<zapi crate 所在路径>" }
zapi-macros = { path = "<本 crate 所在路径>" }
```

无其他必需依赖（不需要 linkme）。

---

## 宏总览

| 宏                     | 作用                                                                  | 挂在什么上    |
| ---------------------- | --------------------------------------------------------------------- | ------------- |
| `#[zq_function]`       | 把 Rust 函数声明为 JS 可调用，并使其**可导出**（生成 `ZqExportItem`） | 函数          |
| `#[zq_export]`         | 声明一个惰性构建的 **JS 值导出项**                                    | 函数          |
| `#[zq_class]`          | 声明原生类（payload + GC trace）                                      | 结构体        |
| `#[zq_class_impl]`     | 填类的行为（构造器 + 方法）                                           | impl 块       |
| `#[zq_method]`         | 标实例方法（原型上）                                                  | impl 里的方法 |
| `#[zq_static_method]`  | 标静态方法/访问器（构造器上）                                         | impl 里的方法 |
| `#[zq_module]`         | 生成模块入口；`exports = [...]` 决定实际导出；可带 init 钩子          | 函数          |
| `#[derive(ZqGcTrace)]` | payload 的 GC trace 派生                                              | 结构体        |

> **"导出"口径**：`#[zq_function]` / `#[zq_export]` / `#[zq_class]` 只让项
> **可导出**（生成 `ZqExportItem`）；真正被 `import` 到模块命名空间，是
> `#[zq_module(exports = [...])]` 里列出该项之后的事。

---

## 各宏详解

### `#[zq_function]`

**用途**：把一个 Rust 函数声明为 JS 可调用的原生函数。注意它只是**可导出**
（生成 `ZqExportItem`），真正导出与否由 `#[zq_module(exports = [...])]` 决定。

**签名**（必须）：

```rust
#[zq_function(name = "add", magic = 0)]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError>
```

- `cx`：回调上下文（by value，Copy），`cx.string()/number()/object()/array()` 造值。
- `cc`：参数/this，`cc.arg(&cx, i)`、`cc.arg_as::<T>(&cx, i)`、`cc.len()`、`cc.this(&cx)`。
- 返回 `Result<ZqValue, ZqError>`；`Err` 在边界抛回 JS。

宏生成：FFI trampoline、`ZqFunction` 实现、`ZqExportItem::Func`（`::item()`）。

**案例**：

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

JS 侧：`import { add, echoString } from "./x.dylib"; add(1, 2)`。

### `#[zq_export]`

**用途**：声明一个**惰性构建**的 JS 值导出项（同样只有被 `exports = [...]`
引用后才真正导出）。值在模块求值期带真实 `ZqEnv` 构建，
所以可以是任意 JS 值（字符串、数字、对象、数组…）。结果由模块导出绑定持有，
被引擎 GC 追踪——不需要自己操心保活。

**签名**（必须）：

```rust
#[zq_export(name = "VERSION")]
fn version<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError>
```

宏生成：值构建 trampoline、marker 类型 + `ZqExportItem::Value`（`::item()`）。

**案例**：

```rust
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cx.object();
    obj.set(&cx, "version", "0.1.0")?;
    obj.set(&cx, "copyright", "© 2026 aike")?;
    Ok(obj)
}
```

JS 侧：`demo_obj.version` / `demo_obj.copyright`。

### `#[zq_class]` + `#[zq_class_impl]` + `#[zq_method]` + `#[zq_static_method]`

**用途**：声明原生类。`#[zq_class]` 声明类与 payload；`#[zq_class_impl]` 填行为
（`constructor` 自动识别为构造器，`#[zq_method]` 进原型，`#[zq_static_method]`
进构造器）。

**`#[zq_class]` 参数**：

- `name`：JS 类名（默认结构体名）。
- `payload`：每实例原生状态类型（可选；不设则类无状态）。
- `payload_no_gc`：显式声明 payload 无 GC 节点，跳过 gc_trace 挂载（省逐实例调用）。
  默认**挂载**（防呆）。

**payload 的 GC trace**：声明了 payload 默认自动挂载 gc_trace 回调；payload 结构体
`#[derive(ZqGcTrace)]`，字段里持有 JS 值的用 `ZqRawValue`（trace 时自动 `mark`）。

**案例**（EventEmitter 的简化版）：

```rust
use zapi::prelude::*;
use zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method};

#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,            // JS 值 → 需要 GC trace
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

`#[zq_class]` 生成的辅助：`class_def()`、`register(&ZqCtx)`、`new_instance(&ZqCtx, payload)`、
`payload(&ZqCtx, val)` / `payload_mut(...)`。

无状态类（无 payload）案例：

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

**访问器（get/set）与 magic 分发**：`#[zq_method(get, set)]` 生成访问器；
`#[zq_method(name = "fire", magic = 1)]` + `#[zq_method(name = "spark", magic = 2)]`
让一个 body 服务多个 JS 名，按 `cc.magic()` 分发。

### `#[zq_module]`

**用途**：生成模块入口 `zq_module_register`（host dlopen 后调用），并承载可选的
模块加载初始化。

**参数**：

- `exports`：模块的**实际导出**清单。**标准写法是内联列表**
  `exports = [f1, c1, v1, ...]`（`#[zq_function]` / `#[zq_class]` / `#[zq_export]`
  各一项，裸项名，宏负责 `::item()` 包装）。进阶可选：引用一个
  `const &[ZqExportItem]` 切片（每项显式 `::item()`），便于把清单拆到单独文件、
  用 `#[cfg]` 组合或复用。
- `name`：装饰性模块名（host 身份是 dylib 路径，不是这个名字）。

```rust
// 标准范式：内联列表
#[zq_module(name = "x", exports = [grep, Counter, version])]
fn module_init() {}

// 进阶（可选）：命名 const 切片，每项显式 `::item()`
const EXPORTS: &[ZqExportItem] = &[grep::item(), Counter::item(), version::item()];
#[zq_module(name = "x", exports = EXPORTS)]
fn module_init() {}
```

  **init 钩子**：被标注的函数**总是**模块初始化钩子（名字随意，**标准用
  `module_init`**），**总是注册**，模块求值期带真实 `ZqEnv` 运行一次；空 body 就是
  空初始化（注册一次、运行一次、无操作，性能无影响）。两种签名：

```rust
// 有初始化逻辑：拿到 ZqEnv（如注册 thread_local 的 GC trace）
#[zq_module(name = "x", exports = [grep, demo_obj])]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> { ... }

// 无初始化逻辑：空初始化（仍注册、仍运行一次，只是什么都不做）
#[zq_module(name = "x", exports = [grep, demo_obj])]
fn module_init() {}
```

**案例**：

```rust
#[zq_module(name = "tools", exports = [grep, demo_obj])]
fn module_init() {}
```

### `#[derive(ZqGcTrace)]`

**用途**：为 payload 结构体派生 GC trace。**本质**：递归转发——对每个字段调用
`ZqGcTrace::trace(&self.field, ctx)`，由**字段类型**决定是否真上报：

- `ZqRawValue` → 非零则 `mark`（真上报）；
- `i32` / `bool` / `String` 等 → 空操作（不含 GC 节点）；
- `Vec<T>` / `Option<T>` / `Box<T>` → 递归进元素；
- 任何字段类型没实现 `ZqGcTrace` → **编译错误**（derive 不会跳过任何字段）。

嵌套自定义类型：给每一层都标 `#[derive(ZqGcTrace)]`；用不了的层手写 impl。
外来/容器类型（如 `HashMap`）没有实现 → 在包含它的类型里手写 `impl ZqGcTrace`，
显式 `ctx.mark` 真正的 GC 节点。

---

## 完整案例：`grep` 扩展（aike-tools）

`Cargo.toml`：

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

`src/lib.rs`（节选）：

```rust
use zapi::prelude::*;
use zapi_macros::{zq_export, zq_function, zq_module};

// 声明一个 JS 可调用的原生函数（可被模块导出）
#[zq_function(name = "grep")]
fn grep<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let pattern = cc.arg_as::<String>(&cx, 0)?;
    // ...ripgrep 风格搜索，构建结果数组...
    Ok(cx.array(&results))
}

// 声明一个值导出项（惰性构建对象）
#[zq_export(name = "demo_obj")]
fn demo_obj<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cx.object();
    obj.set(&cx, "version", "0.1.0")?;
    obj.set(&cx, "copyright", "© 2026 aike")?;
    Ok(obj)
}

// 模块入口：exports 清单决定实际导出
#[zq_module(name = "tools", exports = [grep, demo_obj])]
fn module_init() {}
```

JS 侧：

```js
import { grep, demo_obj } from "./tools.dylib";

const hits = grep("alpha", { cwd: "/path", include: ["*.txt"] });
console.log(hits[0].file, hits[0].lineNumber, hits[0].line);
console.log(demo_obj.version, demo_obj.copyright);
```

---

## GC trace 主题

### 为什么需要

JS 值 = GC 节点。扩展在 native 侧（payload、`thread_local`、静态变量）持有 raw handle
时，引擎 GC **不会自动扫描这些地方**；若值被回收，句柄悬垂 = use-after-free。

### class payload

- 自动：`#[zq_class(payload = T)]` 默认挂载 `gc_trace` 回调，`T` 标 `#[derive(ZqGcTrace)]`，
  字段用 `ZqRawValue` → 自动 `mark`。忘了 = 安全（防呆）。
- 优化：payload 确知无 GC 节点时 `#[zq_class(payload = T, payload_no_gc)]`，
  跳过逐实例的 trace 调用；此时 `T` 甚至不需要实现 `ZqGcTrace`。
- 无 payload 的类恒为 `gc_trace: None`（没有状态可 trace）。

### thread_local（目前手写）

`set_gc_trace_func`（zapi ABI）+ 手写回调，在 `#[zq_module]` 的 init 里注册：

```rust
thread_local! { static STASH: Cell<ZqRawValue> = const { Cell::new(0) }; }

// 回调：每次 GC 把 thread_local 里的非零 raw mark 上报
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

注册必须放 init：init 才有 `ZqEnv`，且先于任何导出被调用。host 把回调加进 Runtime 的
GC trace 列表，每次 GC 的 mark 阶段调用。

### 空 trace 的成本

空 `ZqGcTrace` impl（无 `mark`）**不跨 FFI、不传值**；剩下的是每次 GC 对每个实例
一次函数指针间接调用 + 检查。`payload_no_gc` / 无 payload 时这整条链都省掉。
