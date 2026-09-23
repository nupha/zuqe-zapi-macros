// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Error, Expr, ExprLit, ImplItemFn, ItemImpl, ItemStruct, Lit, LitByteStr, Meta, Path, Token,
    Type, punctuated::Punctuated,
};

pub(super) struct ClassArgs {
    name: Option<String>,
    pub(super) payload: Option<Path>,
    pub(super) payload_no_gc: bool,
}

fn parse_class_args(args: &Punctuated<Meta, Token![,]>) -> syn::Result<ClassArgs> {
    let mut out = ClassArgs {
        name: None,
        payload: None,
        payload_no_gc: false,
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
            Meta::NameValue(nv) if nv.path.is_ident("payload") => {
                let Expr::Path(p) = &nv.value else {
                    return Err(Error::new_spanned(
                        &nv.value,
                        "`payload` must be a type path",
                    ));
                };
                out.payload = Some(p.path.clone());
            }
            Meta::Path(p) if p.is_ident("payload_no_gc") => {
                out.payload_no_gc = true;
            }
            other => {
                return Err(Error::new_spanned(
                    other,
                    "expected `name`, `payload` or `payload_no_gc`",
                ));
            }
        }
    }
    Ok(out)
}

pub(super) fn expand_class(
    st: ItemStruct,
    args: Punctuated<Meta, Token![,]>,
) -> syn::Result<proc_macro2::TokenStream> {
    let ty = st.ident.clone();
    let parsed = parse_class_args(&args)?;
    let name = parsed.name.clone().unwrap_or_else(|| ty.to_string());

    let trace_enabled = parsed.payload.is_some() && !parsed.payload_no_gc;

    let mut name_bytes = name.as_bytes().to_vec();
    name_bytes.push(0);
    let name_lit = LitByteStr::new(&name_bytes, Span::call_site());

    let finalize = format_ident!("__zq_class_finalize_{}", ty);
    let gc_trace = format_ident!("__zq_class_gc_trace_{}", ty);
    let class_def = format_ident!("__ZQ_CLASS_DEF_{}", ty);
    let ctor_sym = format_ident!("__ZQ_CTOR_{}", ty);
    let proto_entries_sym = format_ident!("__ZQ_PROTO_ENTRIES_{}", ty);
    let static_entries_sym = format_ident!("__ZQ_STATIC_ENTRIES_{}", ty);

    let (finalize_body, gc_trace_body, new_instance, accessors) = match &parsed.payload {
        Some(payload) => {
            let finalize_body = quote! {
                if !opaque.is_null() {
                    unsafe { drop(Box::from_raw(opaque as *mut #payload)) };
                }
            };
            let gc_trace_body = quote! {
                if opaque.is_null() {
                    return;
                }
                let payload = unsafe { &*(opaque as *const #payload) };
                let mut ctx = unsafe { ::zuqe_zapi::ZqGcTraceCtx::from(gcx) };
                ::zuqe_zapi::ZqGcTrace::trace(payload, &mut ctx);
            };
            let new_instance = quote! {
                /// Creates a JS instance wrapping `payload`.
                pub fn new_instance<'c>(
                    ctx: ::zuqe_zapi::ZqCtx<'c>,
                    payload: #payload,
                ) -> Result<::zuqe_zapi::ZqValue<'c>, ::zuqe_zapi::ZqError> {
                    let opaque = Box::into_raw(Box::new(payload)) as *mut ::core::ffi::c_void;
                    match ctx.new_instance(&#class_def, opaque) {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            unsafe { drop(Box::from_raw(opaque as *mut #payload)) };
                            Err(e)
                        }
                    }
                }
            };
            let accessors = quote! {
                /// Returns the payload of a class instance.
                pub fn payload<'a>(
                    _ctx: ::zuqe_zapi::ZqCtx<'a>,
                    val: ::zuqe_zapi::ZqValue<'a>,
                ) -> Option<&'a #payload> {
                    unsafe {
                        let p = ::zuqe_zapi::__private::get_object_payload_ptr(val);
                        if p.is_null() {
                            None
                        } else {
                            Some(&*(p as *const #payload))
                        }
                    }
                }

                /// Returns the payload of a class instance, mutably.
                pub fn payload_mut<'a>(
                    _ctx: ::zuqe_zapi::ZqCtx<'a>,
                    val: ::zuqe_zapi::ZqValue<'a>,
                ) -> Option<&'a mut #payload> {
                    unsafe {
                        let p = ::zuqe_zapi::__private::get_object_payload_ptr(val);
                        if p.is_null() {
                            None
                        } else {
                            Some(&mut *(p as *mut #payload))
                        }
                    }
                }

                /// Detaches the payload of a class instance, returning it as an
                /// owned `Box`. The instance keeps no payload afterwards (its
                /// finalizer and GC trace see null), and the engine will no
                /// longer free or trace the returned data — dropping the `Box`
                /// releases it. Use this to release the payload early instead
                /// of waiting for GC. Unsafe by type contract: `val` must be an
                /// instance of this class.
                pub fn take_payload<'a>(
                    _: ::zuqe_zapi::ZqCtx<'a>,
                    val: ::zuqe_zapi::ZqValue<'a>,
                ) -> Option<::std::boxed::Box<#payload>> {
                    unsafe {
                        let p = ::zuqe_zapi::__private::take_object_payload_ptr(val);
                        if p.is_null() {
                            None
                        } else {
                            Some(::std::boxed::Box::from_raw(p as *mut #payload))
                        }
                    }
                }
            };
            (finalize_body, gc_trace_body, new_instance, accessors)
        }
        None => {
            let finalize_body = quote! { let _ = opaque; };
            let gc_trace_body = quote! { let _ = (opaque, gcx); };
            let new_instance = quote! {
                /// Creates a JS instance with no native payload.
                pub fn new_instance<'c>(
                    ctx: ::zuqe_zapi::ZqCtx<'c>,
                ) -> Result<::zuqe_zapi::ZqValue<'c>, ::zuqe_zapi::ZqError> {
                    ctx.new_instance(&#class_def, ::core::ptr::null_mut())
                }
            };
            let accessors = quote! {};
            (finalize_body, gc_trace_body, new_instance, accessors)
        }
    };

    let trace_fn_item = if trace_enabled {
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            unsafe extern "C" fn #gc_trace(
                gcx: *mut ::zuqe_zapi::ffi::ZqGcTraceCtxS,
                opaque: *mut ::core::ffi::c_void,
            ) {
                #gc_trace_body
            }
        }
    } else {
        quote! {}
    };
    let gc_trace_field = if trace_enabled {
        quote! { Some(#gc_trace) }
    } else {
        quote! { ::core::option::Option::None }
    };

    Ok(quote! {
        #st

        #[doc(hidden)]
        #[allow(non_snake_case)]
        unsafe extern "C" fn #finalize(
            _rt: *mut ::core::ffi::c_void,
            opaque: *mut ::core::ffi::c_void,
        ) {
            #finalize_body
        }

        #trace_fn_item

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        static #class_def: ::zuqe_zapi::ffi::ZqClassDef = ::zuqe_zapi::ffi::ZqClassDef {
            name: #name_lit.as_ptr() as *const ::core::ffi::c_char,
            parent_class: ::core::ptr::null(),
            finalize: Some(#finalize),
            gc_trace: #gc_trace_field,
            call: #ctor_sym,
            has_instance: None,
            call_arity: 0,
            proto_entries: #proto_entries_sym.as_ptr(),
            proto_entries_len: #proto_entries_sym.len(),
            static_entries: #static_entries_sym.as_ptr(),
            static_entries_len: #static_entries_sym.len(),
        };

        impl #ty {
            /// Returns the `ZqClassDef` for this class (registered by the module).
            pub const fn class_def() -> &'static ::zuqe_zapi::ffi::ZqClassDef {
                &#class_def
            }

            /// The `ZqFieldDef` for this class (for `#[zq_module(exports = ...)]`).
            pub const fn item() -> ::zuqe_zapi::ffi::ZqFieldDef {
                ::zuqe_zapi::ffi::ZqFieldDef::class(
                    Self::class_def().name,
                    Self::class_def(),
                )
            }

            /// Registers the class with the host.
            pub fn register(ctx: ::zuqe_zapi::ZqCtx) -> Result<(), ::zuqe_zapi::ZqError> {
                ctx.register_class(&#class_def).map(|_| ())
            }

            #new_instance
            #accessors
        }
    })
}

pub(super) fn expand_class_impl(im: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    if im.trait_.is_some() {
        return Err(Error::new_spanned(
            &im.self_ty,
            "class impl must be an inherent impl",
        ));
    }
    if !im.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &im.generics,
            "class impl cannot be generic",
        ));
    }
    let ty = match &*im.self_ty {
        Type::Path(tp) if tp.qself.is_none() => tp.path.get_ident().cloned().ok_or_else(|| {
            Error::new_spanned(
                &im.self_ty,
                "class impl self type must be a plain identifier",
            )
        })?,
        _ => {
            return Err(Error::new_spanned(
                &im.self_ty,
                "class impl self type must be a plain identifier",
            ));
        }
    };

    let mut ctor: Option<&ImplItemFn> = None;
    let mut all: Vec<(bool, String, bool, bool, i32, &ImplItemFn)> = Vec::new();
    for item in &im.items {
        let syn::ImplItem::Fn(f) = item else {
            continue;
        };
        if f.sig.ident == "constructor" {
            if ctor.is_some() {
                return Err(Error::new_spanned(&f.sig.ident, "duplicate `constructor`"));
            }
            ctor = Some(f);
        } else if f
            .attrs
            .iter()
            .any(|a| a.path().is_ident("zq_method") || a.path().is_ident("zq_static_method"))
        {
            for (is_static, name, get, set, magic) in zq_method_entries(f)? {
                all.push((is_static, name, get, set, magic, f));
            }
        }
    }

    for f in ctor
        .iter()
        .copied()
        .chain(all.iter().map(|(_, _, _, _, _, f)| *f))
    {
        if f.sig.inputs.len() != 2 {
            return Err(Error::new_spanned(
                &f.sig,
                "class methods require signature: fn(ZqCtx, ZqCallInfo) -> Result<ZqValue, ZqError>",
            ));
        }
    }

    let proto_entries: Vec<(String, bool, bool, i32, &ImplItemFn)> = all
        .iter()
        .filter(|(st, _, _, _, _, _)| !st)
        .map(|(_, n, g, s, m, f)| (n.clone(), *g, *s, *m, *f))
        .collect();
    let static_entries: Vec<(String, bool, bool, i32, &ImplItemFn)> = all
        .iter()
        .filter(|(st, _, _, _, _, _)| *st)
        .map(|(_, n, g, s, m, f)| (n.clone(), *g, *s, *m, *f))
        .collect();

    let mut unique_fns: Vec<&ImplItemFn> = Vec::new();
    let mut seen: Vec<syn::Ident> = Vec::new();
    for f in proto_entries
        .iter()
        .map(|(_, _, _, _, f)| *f)
        .chain(static_entries.iter().map(|(_, _, _, _, f)| *f))
    {
        if !seen.contains(&f.sig.ident) {
            seen.push(f.sig.ident.clone());
            unique_fns.push(f);
        }
    }

    let ctor_sym = format_ident!("__ZQ_CTOR_{}", ty);
    let ctor_tramp = format_ident!("__zq_class_ctor_{}", ty);
    let ctor_item = match ctor {
        Some(_) => quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            unsafe extern "C" fn #ctor_tramp(
                __env:                 ::zuqe_zapi::__private::ZqRawCtx,
                __info: *mut ::zuqe_zapi::ffi::ZqCallInfoS,
            ) -> ::zuqe_zapi::__private::ZqRawValue {
                let __cx = unsafe { ::zuqe_zapi::ZqCtx::from(__env) };
                let __cc = ::zuqe_zapi::ZqCallInfo::new(__info);
                match #ty::constructor(__cx, __cc) {
                    Ok(__v) => __v.raw(),
                    Err(_) => ::zuqe_zapi::ZqValue::exception_value(),
                }
            }

            #[allow(non_upper_case_globals)]
            const #ctor_sym: Option<::zuqe_zapi::__private::ZqCallbackFn> = Some(#ctor_tramp);
        },
        None => quote! {
            #[allow(non_upper_case_globals)]
            const #ctor_sym: Option<::zuqe_zapi::__private::ZqCallbackFn> = None;
        },
    };

    let method_tramp_items: Vec<proc_macro2::TokenStream> = unique_fns
        .iter()
        .map(|f| {
            let tramp = tramp_name(&ty, f);
            let ident = &f.sig.ident;
            quote! {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                unsafe extern "C" fn #tramp(
                    __env: ::zuqe_zapi::__private::ZqRawCtx,
                    __info: *mut ::zuqe_zapi::ffi::ZqCallInfoS,
                ) -> ::zuqe_zapi::__private::ZqRawValue {
                    let __cx = unsafe { ::zuqe_zapi::ZqCtx::from(__env) };
                    let __cc = ::zuqe_zapi::ZqCallInfo::new(__info);
                    match #ty::#ident(__cx, __cc) {
                        Ok(__v) => __v.raw(),
                        Err(_) => ::zuqe_zapi::ZqValue::exception_value(),
                    }
                }
            }
        })
        .collect();

    let proto_entries_sym = format_ident!("__ZQ_PROTO_ENTRIES_{}", ty);
    let static_entries_sym = format_ident!("__ZQ_STATIC_ENTRIES_{}", ty);

    let proto_entries_item = gen_entries_array(&proto_entries_sym, &proto_entries, &ty);
    let static_entries_item = gen_entries_array(&static_entries_sym, &static_entries, &ty);

    Ok(quote! {
        #im
        #ctor_item
        #(#method_tramp_items)*
        #proto_entries_item
        #static_entries_item
    })
}

fn tramp_name(ty: &syn::Ident, f: &ImplItemFn) -> proc_macro2::Ident {
    format_ident!("__zq_class_method_{}_{}", ty, f.sig.ident)
}

fn gen_entries_array(
    sym: &proc_macro2::Ident,
    entries: &[(String, bool, bool, i32, &ImplItemFn)],
    ty: &syn::Ident,
) -> proc_macro2::TokenStream {
    let n = entries.len();
    let items = entries.iter().map(|(name, get, set, magic, f)| {
        let tramp = tramp_name(ty, f);
        let mut b = name.as_bytes().to_vec();
        b.push(0);
        let name_lit = LitByteStr::new(&b, Span::call_site());
        let name_ptr = quote! { #name_lit.as_ptr() as *const ::core::ffi::c_char };
        if *get || *set {
            let get_field = if *get {
                quote! { Some(#tramp) }
            } else {
                quote! { None }
            };
            let set_field = if *set {
                quote! { Some(#tramp) }
            } else {
                quote! { None }
            };
            quote! {
                ::zuqe_zapi::ffi::ZqFieldDef::getset(#name_ptr, #get_field, #set_field, #magic)
            }
        } else {
            quote! {
                ::zuqe_zapi::ffi::ZqFieldDef::func(#name_ptr, #tramp, 0, #magic)
            }
        }
    });
    quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        static #sym: [::zuqe_zapi::ffi::ZqFieldDef; #n] = [
            #(#items),*
        ];
    }
}

fn zq_method_entries(f: &ImplItemFn) -> syn::Result<Vec<(bool, String, bool, bool, i32)>> {
    let mut entries = Vec::new();
    for attr in &f.attrs {
        let is_static = if attr.path().is_ident("zq_static_method") {
            true
        } else if attr.path().is_ident("zq_method") {
            false
        } else {
            continue;
        };
        let mut name = f.sig.ident.to_string();
        let mut get = false;
        let mut set = false;
        let mut magic = 0i32;
        if let Ok(metas) = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) {
            for m in metas {
                match m {
                    Meta::Path(p) if p.is_ident("get") => get = true,
                    Meta::Path(p) if p.is_ident("set") => set = true,
                    Meta::NameValue(nv) if nv.path.is_ident("name") => {
                        if let Expr::Lit(ExprLit {
                            lit: Lit::Str(s), ..
                        }) = &nv.value
                        {
                            name = s.value();
                        }
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("magic") => {
                        if let Expr::Lit(ExprLit {
                            lit: Lit::Int(i), ..
                        }) = &nv.value
                        {
                            magic = i.base10_parse()?;
                        }
                    }
                    other => {
                        return Err(Error::new_spanned(
                            &other,
                            "expected `get`, `set`, `name` or `magic` in #[zq_method]",
                        ));
                    }
                }
            }
        }
        entries.push((is_static, name, get, set, magic));
    }
    Ok(entries)
}
