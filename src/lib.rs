// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    DeriveInput, Error, Expr, ExprLit, ItemFn, ItemImpl, ItemStruct, Lit, LitByteStr, Meta, Path,
    Token, parse_macro_input,
};

// ---------------------------------------------------------------------------
// #[zq_function]
// ---------------------------------------------------------------------------

/// Marks a function as callable from JavaScript.
///
/// The signature must be:
/// ```ignore
/// fn name(ctx: ZqCtx, cc: ZqCallInfo) -> Result<ZqValue<'_>, ZqError>
/// ```
///
/// - `ctx: ZqCtx` — callback context (by value, Copy).
/// - `cc: ZqCallInfo` — provides `this()`, `len()`, `arg()`, `arg_as()`.
/// - Returns `Result<ZqValue<'_>, ZqError>` — the JS return value or error.
///
/// The function produces a `ZqFieldDef` (kind `ZQ_FIELD_FUNC`), so it can be
/// listed in `#[zq_module(exports = [...])]` as a function export.
#[proc_macro_attribute]
pub fn zq_function(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);

    match expand_function(func, args) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Marks a function as a lazy value module export (kind `ZQ_FIELD_VALUE`).
///
/// The signature must be:
/// ```ignore
/// fn name(ctx: ZqCtx) -> Result<ZqValue<'_>, ZqError>
/// ```
///
/// The value is built at module-evaluation time with a real `ZqEnv`, so it can
/// be any JS value (string, number, object, array, ...).
#[proc_macro_attribute]
pub fn zq_export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);

    match expand_export(func, args) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Generates the module entry point (`zq_module_register`).
///
/// `exports = [f1, c1, v1, ...]` lists every `#[zq_function]` / `#[zq_class]` /
/// `#[zq_export]` item to publish; each produces a `ZqFieldDef`. The list is
/// compiled into a `ZqFieldDef` array that the host materializes.
///
/// `name` is a decorative module label (host identity is the dylib URL).
///
/// The annotated function is ALWAYS the module init hook (any name works; the
/// standard is `module_init`), always registered — the host runs it once at
/// evaluation time with a real `ZqEnv`. Two signature forms:
/// `fn module_init(ctx: ZqCtx) -> Result<(), ZqError>` for real init logic, or
/// an empty `fn module_init() {}` for a registered no-op init.
#[proc_macro_attribute]
pub fn zq_module(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);

    match expand_module(func, args) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------------
// #[zq_class]
// ---------------------------------------------------------------------------

/// Declares a native class from a payload struct.
///
/// Generates the `ZqClassDef` (with a `finalize` that drops the boxed payload,
/// a `gc_trace` that runs the payload's `ZqGcTrace` impl, and a `call` wired to
/// the `#[zq_class_impl]` constructor) plus helpers:
///
/// - `class_def() -> &'static ZqClassDef`
/// - `register(ZqCtx) -> Result<(), ZqError>`
/// - `new_instance(ZqCtx, payload) -> Result<ZqValue, ZqError>` (with payload)
/// - `payload(ZqCtx, val) / payload_mut(...)` (with payload)
///
/// `name` is the JS class name (defaults to the struct name); `payload`
/// optionally names the struct holding per-instance state. A class without
/// `payload` has no native state (like `def_native_object!`'s `()` payload).
///
/// `gc_trace` is wired whenever `payload` is present (fool-proof: forgetting a
/// needed trace is a use-after-free). Pass `payload_no_gc` to opt out when the
/// payload is known to hold no GC nodes — the per-instance trace call is then
/// skipped entirely. `payload_no_gc` with a `ZqGcTrace`-holding payload is unsafe.
///
/// Every `#[zq_class]` struct must be paired with a `#[zq_class_impl]` impl
/// block in the same module, which provides the constructor and methods.
#[proc_macro_attribute]
pub fn zq_class(attr: TokenStream, item: TokenStream) -> TokenStream {
    let st = parse_macro_input!(item as ItemStruct);
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);

    match class::expand_class(st, args) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Fills in a `#[zq_class]` class's behavior. The attribute goes on the
/// inherent `impl` block of the class struct: `fn constructor` is recognized
/// as the constructor body, `#[zq_method]` methods become instance methods
/// (installed on the prototype, `ee.method(...)`), and `#[zq_static_method]`
/// methods become static methods (installed on the constructor). The impl
/// block stays intact, so methods remain callable from Rust.
#[proc_macro_attribute]
pub fn zq_class_impl(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let im = parse_macro_input!(item as ItemImpl);

    match class::expand_class_impl(im) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Marks a method inside a `#[zq_class_impl]` block as an instance method
/// (on the prototype). No-op outside that context; supports
/// `#[zq_method(name = "js_name", get, set, magic = N)]`.
#[proc_macro_attribute]
pub fn zq_method(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marks a method inside a `#[zq_class_impl]` block as a static method
/// (on the constructor). No-op outside that context; supports
/// `#[zq_static_method(name = "js_name", get, set, magic = N)]`.
#[proc_macro_attribute]
pub fn zq_static_method(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Derives `ZqGcTrace` for a struct by tracing each field.
#[proc_macro_derive(ZqGcTrace)]
pub fn derive_gc_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = input.ident;
    let fields = match &input.data {
        syn::Data::Struct(s) => &s.fields,
        _ => {
            return Error::new_spanned(&ident, "ZqGcTrace can only be derived on structs")
                .to_compile_error()
                .into();
        }
    };
    let traces = fields.iter().map(|f| {
        let name = f.ident.as_ref().expect("ZqGcTrace requires named fields");
        quote! {
            ::zuqe_zapi::ZqGcTrace::trace(&self.#name, gcx);
        }
    });
    (quote! {
        impl ::zuqe_zapi::ZqGcTrace for #ident {
            fn trace(&self, gcx: &mut ::zuqe_zapi::ZqGcTraceCtx) {
                #(#traces)*
            }
        }
    })
    .into()
}

// ---------------------------------------------------------------------------
// expand_function
// ---------------------------------------------------------------------------

fn expand_function(
    func: ItemFn,
    args: Punctuated<Meta, Token![,]>,
) -> syn::Result<proc_macro2::TokenStream> {
    let fn_name = func.sig.ident.clone();
    let vis = func.vis.clone();
    let js_name = func_arg_name(&args)?.unwrap_or_else(|| fn_name.to_string());
    let magic = magic_arg(&args)?;

    // Validate signature: 2 params, return Result<ZqValue<'_>, ZqError>
    if func.sig.inputs.len() != 2 {
        return Err(Error::new_spanned(
            &func.sig,
            "#[zq_function] requires signature: fn name(ZqCtx, ZqCallInfo) -> Result<ZqValue, ZqError>",
        ));
    }

    let trampoline_fn = format_ident!("__zq_trampoline_{}", fn_name);
    let impl_fn = format_ident!("__zq_impl_{}", fn_name);
    let mut body = func;
    body.sig.ident = impl_fn.clone();
    body.vis = syn::Visibility::Inherited;

    let mut name_bytes = js_name.as_bytes().to_vec();
    name_bytes.push(0);
    let name_lit = LitByteStr::new(&name_bytes, Span::call_site());

    Ok(quote! {
        #[doc(hidden)]
        #body

        #[doc(hidden)]
        #[allow(non_snake_case)]
        unsafe extern "C" fn #trampoline_fn(
            __env: ::zuqe_zapi::__private::ZqEnv,
            __info: ::zuqe_zapi::__private::ZqCallbackInfo,
        ) -> ::zuqe_zapi::__private::ZqRawValue {
            let __cx = unsafe { ::zuqe_zapi::__private::ZqCtx::from_raw(__env) };
            let __cc = ::zuqe_zapi::ZqCallInfo::new(__info);
            match #impl_fn(__cx, __cc) {
                Ok(__v) => __v.raw(),
                Err(__e) => {
                    let __ex = __e.exception().unwrap_or_else(|| __cx.throw_error(&__e));
                    unsafe { (*__info).thrown = __ex };
                    ::zuqe_zapi::__private::exception_value()
                }
            }
        }

        #vis struct #fn_name;

        impl #fn_name {
            #[allow(dead_code)]
            #vis const fn trampoline() -> ::zuqe_zapi::ffi::ZqBodyFn {
                #trampoline_fn
            }

            #[allow(dead_code)]
            #vis fn call<'a>(
                __cx: ::zuqe_zapi::ZqCtx<'a>,
                __cc: ::zuqe_zapi::ZqCallInfo<'a>,
            ) -> Result<::zuqe_zapi::ZqValue<'a>, ::zuqe_zapi::ZqError> {
                #impl_fn(__cx, __cc)
            }

            /// The `ZqFieldDef` for this function (for `#[zq_module(exports = ...)]`).
            #vis const fn item() -> ::zuqe_zapi::ffi::ZqFieldDef {
                ::zuqe_zapi::ffi::ZqFieldDef::func(
                    #name_lit.as_ptr() as *const ::core::ffi::c_char,
                    #trampoline_fn,
                    0,
                    #magic,
                )
            }
        }
    })
}

// ---------------------------------------------------------------------------
// expand_export
// ---------------------------------------------------------------------------

fn expand_export(
    func: ItemFn,
    args: Punctuated<Meta, Token![,]>,
) -> syn::Result<proc_macro2::TokenStream> {
    let fn_name = func.sig.ident.clone();
    let js_name = func_arg_name(&args)?.unwrap_or_else(|| fn_name.to_string());

    // Validate signature: 1 param (ctx), returns Result<ZqValue, ZqError>.
    if func.sig.inputs.len() != 1 {
        return Err(Error::new_spanned(
            &func.sig,
            "#[zq_export] requires signature: fn name(ZqCtx) -> Result<ZqValue, ZqError>",
        ));
    }

    let impl_fn = format_ident!("__zq_impl_{}", fn_name);
    let value_fn = format_ident!("__zq_value_{}", fn_name);
    let mut body = func;
    body.sig.ident = impl_fn.clone();
    body.vis = syn::Visibility::Inherited;

    let mut name_bytes = js_name.as_bytes().to_vec();
    name_bytes.push(0);
    let name_lit = LitByteStr::new(&name_bytes, Span::call_site());

    Ok(quote! {
        #[doc(hidden)]
        #body

        #[doc(hidden)]
        #[allow(non_snake_case)]
        unsafe extern "C" fn #value_fn(
            __env: ::zuqe_zapi::ffi::ZqEnv,
            __out: *mut ::zuqe_zapi::ffi::ZqRawValue,
        ) -> i32 {
            let __cx = unsafe { ::zuqe_zapi::ZqCtx::from_raw(__env) };
            match #impl_fn(__cx) {
                Ok(__v) => {
                    unsafe { *__out = __v.raw() };
                    0
                }
                Err(_) => 1,
            }
        }

        // Marker type so `#[zq_module(exports = [...])]` can reference this item.
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy)]
        struct #fn_name;

        impl #fn_name {
            /// The `ZqFieldDef` for this value (for `#[zq_module(exports = ...)]`).
            pub const fn item() -> ::zuqe_zapi::ffi::ZqFieldDef {
                ::zuqe_zapi::ffi::ZqFieldDef::value(
                    #name_lit.as_ptr() as *const ::core::ffi::c_char,
                    #value_fn,
                )
            }
        }
    })
}

// ---------------------------------------------------------------------------
// expand_module
// ---------------------------------------------------------------------------

struct ModuleArgs {
    name: Option<String>,
    /// Inline list `exports = [a, b, c]` (each item exposes `::item() -> ZqFieldDef`).
    exports: Vec<Path>,
    /// Named slice `exports = MY_EXPORTS` (a `const`/`static` `[ZqFieldDef; _]`),
    /// referenced directly instead of an inline list.
    exports_named: Option<Path>,
}

fn parse_path_list(expr: &Expr) -> syn::Result<Vec<Path>> {
    let Expr::Array(arr) = expr else {
        return Err(Error::new_spanned(expr, "expected `[path, path, ..]`"));
    };
    arr.elems
        .iter()
        .map(|e| match e {
            Expr::Path(p) => Ok(p.path.clone()),
            other => Err(Error::new_spanned(other, "expected a path")),
        })
        .collect()
}

fn parse_module_args(args: &Punctuated<Meta, Token![,]>) -> syn::Result<ModuleArgs> {
    let mut out = ModuleArgs {
        name: None,
        exports: Vec::new(),
        exports_named: None,
    };
    for meta in args {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("name") => {
                let Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) = &nv.value
                else {
                    return Err(Error::new_spanned(
                        &nv.value,
                        "`name` must be a string literal",
                    ));
                };
                out.name = Some(s.value());
            }
            Meta::NameValue(nv) if nv.path.is_ident("exports") => match &nv.value {
                Expr::Array(_) => out.exports = parse_path_list(&nv.value)?,
                Expr::Path(p) => out.exports_named = Some(p.path.clone()),
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "`exports` must be a `[item, ..]` list or a path to a `[ZqFieldDef; _]` const",
                    ));
                }
            },
            other => {
                return Err(Error::new_spanned(other, "expected `name` or `exports`"));
            }
        }
    }
    Ok(out)
}

fn expand_module(
    func: ItemFn,
    args: Punctuated<Meta, Token![,]>,
) -> syn::Result<proc_macro2::TokenStream> {
    let parsed = parse_module_args(&args)?;
    let fn_name = func.sig.ident.clone();

    // The annotated function is ALWAYS the module init hook (any name; standard
    // is `module_init`), always registered. Two signature forms:
    //   - `fn module_init(ctx: ZqCtx) -> Result<(), ZqError>` — real init logic;
    //   - `fn module_init() {}` — empty init (registered, runs once, no-op).
    let init_takes_cx = match func.sig.inputs.len() {
        0 => false,
        1 => true,
        n => {
            return Err(Error::new_spanned(
                &func.sig,
                format!("`#[zq_module]` init hook must take 0 or 1 parameter (got {n})"),
            ));
        }
    };

    let mut func = func;
    func.attrs.push(syn::parse_quote!(#[allow(dead_code)]));

    // Optional decorative module name (host identity is the dylib URL).
    let name_ptr = match &parsed.name {
        Some(n) => {
            let mut b = n.as_bytes().to_vec();
            b.push(0);
            let lit = LitByteStr::new(&b, Span::call_site());
            quote! { #lit.as_ptr() as *const ::core::ffi::c_char }
        }
        None => quote! { ::core::ptr::null() },
    };

    // Module init hook: wrapped so the host can call it at evaluation time
    // with a real `ZqEnv`; non-zero return signals failure. Always registered
    // (an empty `fn module_init() {}` is a registered no-op init).
    let init_body = if init_takes_cx {
        quote! {
            let __cx = unsafe { ::zuqe_zapi::ZqCtx::from_raw(__env) };
            match #fn_name(&__cx) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
    } else {
        quote! {
            #fn_name();
            0
        }
    };
    let init_tramp = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        #[allow(unused_variables)]
        unsafe extern "C" fn __zq_module_init(__env: ::zuqe_zapi::ffi::ZqEnv) -> i32 {
            #init_body
        }
    };
    let init_expr = quote! { Some(__zq_module_init) };

    // Either a named `const`/`static` slice of `ZqFieldDef`, or an inline list
    // built via each item's `::item()`.
    let exports_expr = if let Some(named) = &parsed.exports_named {
        quote! { #named.iter().copied().collect::<::std::vec::Vec<_>>() }
    } else {
        let exports_items = &parsed.exports;
        quote! {
            ::std::vec![
                #(#exports_items::item()),*
            ]
        }
    };

    Ok(quote! {
        #func
        #init_tramp

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn zq_module_register(
            __api: *mut ::zuqe_zapi::ffi::ZqApi,
            __info: *mut ::zuqe_zapi::ffi::ZqModuleInfo,
        ) {
            assert!(!__api.is_null() && !__info.is_null());
            unsafe { ::zuqe_zapi::__private::zapi_init(__api as *const _) };
            // Stash the host-provided dylib path (see `zuqe_zapi::module_path`).
            unsafe { ::zuqe_zapi::__private::path_init((*__info).path) };

            // Materialize the exports into a leaked slice that stays valid for
            // the process lifetime (the library is dlopen'd once and kept alive
            // by the host via `mem::forget`).
            let __exports: ::std::vec::Vec<::zuqe_zapi::ffi::ZqFieldDef> = #exports_expr;
            let __exports = ::std::boxed::Box::leak(__exports.into_boxed_slice());

            unsafe {
                (*__info).name = #name_ptr;
                (*__info).exports = __exports.as_ptr();
                (*__info).exports_len = __exports.len();
                (*__info).init = #init_expr;
                (*__info).struct_size =
                    ::std::mem::size_of::<::zuqe_zapi::__private::ZqModuleInfo>() as u32;
                (*__info).version = ::zuqe_zapi::__private::ZQ_VERSION_PACKED;
            }
        }
    })
}

fn magic_arg(args: &Punctuated<Meta, Token![,]>) -> syn::Result<i32> {
    let mut magic = 0i32;
    for meta in args {
        let Meta::NameValue(nv) = meta else {
            continue;
        };
        if nv.path.is_ident("magic") {
            let Expr::Lit(ExprLit {
                lit: Lit::Int(i), ..
            }) = &nv.value
            else {
                return Err(Error::new_spanned(
                    &nv.value,
                    "`magic` must be an integer literal",
                ));
            };
            magic = i.base10_parse()?;
        }
    }
    Ok(magic)
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn func_arg_name(args: &Punctuated<Meta, Token![,]>) -> syn::Result<Option<String>> {
    let mut out = None;
    for meta in args {
        let Meta::NameValue(nv) = meta else {
            continue;
        };
        if nv.path.is_ident("name") {
            let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
            else {
                return Err(Error::new_spanned(
                    &nv.value,
                    "`name` must be a string literal",
                ));
            };
            out = Some(s.value());
        }
    }
    Ok(out)
}
