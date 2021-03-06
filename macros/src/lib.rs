extern crate proc_macro;
extern crate proc_macro2;
extern crate quote;
extern crate rand;
extern crate syn;

use proc_macro::TokenStream;
use std::{
    collections::HashSet,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use proc_macro2::Span;
use quote::quote;
use rand::{Rng, SeedableRng};
use syn::{
    parse, parse_macro_input, spanned::Spanned, Ident, Item, ItemFn, ItemStatic, ReturnType, Stmt,
    Type, Visibility,
};

/// Attribute to declare the entry point of the program
///
/// The specified function will be called by the reset handler *after* RAM has been initialized.
///
/// The type of the specified function must be `[unsafe] fn() -> !` (never ending function)
///
/// # Properties
///
/// The entry point will be called by the reset handler. The program can't reference to the entry
/// point, much less invoke it.
///
/// `static mut` variables declared within the entry point are safe to access. The compiler can't
/// prove this is safe so the attribute will help by making a transformation to the source code: for
/// this reason a variable like `static mut FOO: u32` will become `let FOO: &'static mut u32;`. Note
/// that `&'static mut` references have move semantics.
///
/// # Examples
///
/// - Simple entry point
///
/// ``` no_run
/// # #![no_main]
/// # use msp430_rt_macros::entry;
/// #[entry]
/// fn main() -> ! {
///     loop {
///         /* .. */
///     }
/// }
/// ```
///
/// - `static mut` variables local to the entry point are safe to modify.
///
/// ``` no_run
/// # #![no_main]
/// # use msp430_macros::entry;
/// #[entry]
/// fn main() -> ! {
///     static mut FOO: u32 = 0;
///
///     let foo: &'static mut u32 = FOO;
///     assert_eq!(*foo, 0);
///     *foo = 1;
///     assert_eq!(*foo, 1);
///
///     loop {
///         /* .. */
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn entry(args: TokenStream, input: TokenStream) -> TokenStream {
    let f = parse_macro_input!(input as ItemFn);

    // check the function signature
    let valid_signature = f.constness.is_none()
        && f.vis == Visibility::Inherited
        && f.abi.is_none()
        && f.decl.inputs.is_empty()
        && f.decl.generics.params.is_empty()
        && f.decl.generics.where_clause.is_none()
        && f.decl.variadic.is_none()
        && match f.decl.output {
            ReturnType::Default => false,
            ReturnType::Type(_, ref ty) => match **ty {
                Type::Never(_) => true,
                _ => false,
            },
        };

    if !valid_signature {
        return parse::Error::new(
            f.span(),
            "`#[entry]` function must have signature `[unsafe] fn() -> !`",
        )
        .to_compile_error()
        .into();
    }

    if !args.is_empty() {
        return parse::Error::new(Span::call_site(), "This attribute accepts no arguments")
            .to_compile_error()
            .into();
    }

    // XXX should we blacklist other attributes?
    let attrs = f.attrs;
    let unsafety = f.unsafety;
    let hash = random_ident();
    let (statics, stmts) = match extract_static_muts(f.block.stmts) {
        Err(e) => return e.to_compile_error().into(),
        Ok(x) => x,
    };

    let vars = statics
        .into_iter()
        .map(|var| {
            let attrs = var.attrs;
            let ident = var.ident;
            let ty = var.ty;
            let expr = var.expr;

            quote!(
                #[allow(non_snake_case)]
                let #ident: &'static mut #ty = unsafe {
                    #(#attrs)*
                    static mut #ident: #ty = #expr;

                    &mut #ident
                };
            )
        })
        .collect::<Vec<_>>();

    quote!(
        #[export_name = "main"]
        #(#attrs)*
        pub #unsafety fn #hash() -> ! {
            #(#vars)*

            #(#stmts)*
        }
    )
    .into()
}

/// Attribute to declare an interrupt handler
///
/// When the `device` feature is disabled this attribute can only be used to override the
/// DefaultHandler.
///
/// When the `device` feature is enabled this attribute can be used to override other interrupt
/// handlers but only when imported from a PAC (Peripheral Access Crate) crate which re-exports it.
/// Importing this attribute from the `msp430-rt` crate and using it on a function will result in a
/// compiler error.
///
/// # Syntax
///
/// ``` ignore
/// extern crate device;
///
/// // the attribute comes from the device crate not from msp430-rt
/// use device::interrupt;
///
/// #[interrupt]
/// fn USART1() {
///     // ..
/// }
/// ```
///
/// where the name of the function must be `DefaultHandler` or one of the device interrupts.
///
/// # Usage
///
/// `#[interrupt] fn Name(..` overrides the default handler for the interrupt with the given `Name`.
/// These handlers must have signature `[unsafe] fn() [-> !]`. It's possible to add state to these
/// handlers by declaring `static mut` variables at the beginning of the body of the function. These
/// variables will be safe to access from the function body.
///
/// If the interrupt handler has not been overridden it will be dispatched by the default interrupt
/// handler (`DefaultHandler`).
///
/// `#[interrupt] fn DefaultHandler(..` can be used to override the default interrupt handler. When
/// not overridden `DefaultHandler` defaults to an infinite loop.
///
/// # Properties
///
/// Interrupts handlers can only be called by the hardware. Other parts of the program can't refer
/// to the interrupt handlers, much less invoke them as if they were functions.
///
/// `static mut` variables declared within an interrupt handler are safe to access and can be used
/// to preserve state across invocations of the handler. The compiler can't prove this is safe so
/// the attribute will help by making a transformation to the source code: for this reason a
/// variable like `static mut FOO: u32` will become `let FOO: &mut u32;`.
///
/// # Examples
///
/// - Using state within an interrupt handler
///
/// ``` ignore
/// extern crate device;
///
/// use device::interrupt;
///
/// #[interrupt]
/// fn TIM2() {
///     static mut COUNT: i32 = 0;
///
///     // `COUNT` is safe to access and has type `&mut i32`
///     *COUNT += 1;
///
///     println!("{}", COUNT);
/// }
/// ```
#[proc_macro_attribute]
pub fn interrupt(args: TokenStream, input: TokenStream) -> TokenStream {
    let f: ItemFn = syn::parse(input).expect("`#[interrupt]` must be applied to a function");

    if !args.is_empty() {
        return parse::Error::new(Span::call_site(), "This attribute accepts no arguments")
            .to_compile_error()
            .into();
    }

    let fspan = f.span();
    let ident = f.ident;
    let ident_s = ident.to_string();

    let check = if ident.to_string() == "DefaultHandler" {
        None
    } else if cfg!(feature = "device") {
        Some(quote!(interrupt::#ident;))
    } else {
        return parse::Error::new(
            ident.span(),
            "only the DefaultHandler can be overridden when the `device` feature is disabled",
        )
            .to_compile_error()
            .into();
    };

    // XXX should we blacklist other attributes?
    let attrs = f.attrs;
    let block = f.block;
    let stmts = block.stmts;
    let unsafety = f.unsafety;

    let valid_signature = f.constness.is_none()
        && f.vis == Visibility::Inherited
        && f.abi.is_none()
        && f.decl.inputs.is_empty()
        && f.decl.generics.params.is_empty()
        && f.decl.generics.where_clause.is_none()
        && f.decl.variadic.is_none()
        && match f.decl.output {
            ReturnType::Default => true,
            ReturnType::Type(_, ref ty) => match **ty {
                Type::Tuple(ref tuple) => tuple.elems.is_empty(),
                Type::Never(..) => true,
                _ => false,
            },
        };

    if !valid_signature {
        return parse::Error::new(
            fspan,
            "`#[interrupt]` handlers must have signature `[unsafe] fn() [-> !]`",
        )
        .to_compile_error()
        .into();
    }

    let (statics, stmts) = match extract_static_muts(stmts) {
        Err(e) => return e.to_compile_error().into(),
        Ok(x) => x,
    };

    let vars = statics
        .into_iter()
        .map(|var| {
            let attrs = var.attrs;
            let ident = var.ident;
            let ty = var.ty;
            let expr = var.expr;

            quote!(
                #[allow(non_snake_case)]
                let #ident: &mut #ty = unsafe {
                    #(#attrs)*
                    static mut #ident: #ty = #expr;

                    &mut #ident
                };
            )
        })
        .collect::<Vec<_>>();

    let hash = random_ident();
    quote!(
        #[export_name = #ident_s]
        #(#attrs)*
        #unsafety extern "msp430-interrupt" fn #hash() {
            #check

            #(#vars)*

            #(#stmts)*
        }
    )
    .into()
}

/// Attribute to mark which function will be called at the beginning of the reset handler.
///
/// **IMPORTANT**: This attribute can appear at most *once* in the dependency graph.
///
/// The function must have the signature of `unsafe fn()`.
///
/// The function passed will be called before static variables are initialized. Any access of static
/// variables will result in undefined behavior.
///
/// # Examples
///
/// ```
/// # use msp430_macros::pre_init;
/// #[pre_init]
/// unsafe fn before_main() {
///     // do something here
/// }
///
/// # fn main() {}
/// ```
#[proc_macro_attribute]
pub fn pre_init(args: TokenStream, input: TokenStream) -> TokenStream {
    let f = parse_macro_input!(input as ItemFn);

    // check the function signature
    let valid_signature = f.constness.is_none()
        && f.vis == Visibility::Inherited
        && f.unsafety.is_some()
        && f.abi.is_none()
        && f.decl.inputs.is_empty()
        && f.decl.generics.params.is_empty()
        && f.decl.generics.where_clause.is_none()
        && f.decl.variadic.is_none()
        && match f.decl.output {
            ReturnType::Default => true,
            ReturnType::Type(_, ref ty) => match **ty {
                Type::Tuple(ref tuple) => tuple.elems.is_empty(),
                _ => false,
            },
        };

    if !valid_signature {
        return parse::Error::new(
            f.span(),
            "`#[pre_init]` function must have signature `unsafe fn()`",
        )
        .to_compile_error()
        .into();
    }

    if !args.is_empty() {
        return parse::Error::new(Span::call_site(), "This attribute accepts no arguments")
            .to_compile_error()
            .into();
    }

    // XXX should we blacklist other attributes?
    let attrs = f.attrs;
    let ident = f.ident;
    let block = f.block;

    quote!(
        #[export_name = "__pre_init"]
        #(#attrs)*
        pub unsafe fn #ident() #block
    )
    .into()
}

// Creates a random identifier
fn random_ident() -> Ident {
    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let count: u64 = CALL_COUNT.fetch_add(1, Ordering::SeqCst) as u64;
    let mut seed: [u8; 16] = [0; 16];

    for (i, v) in seed.iter_mut().take(8).enumerate() {
        *v = ((secs >> (i * 8)) & 0xFF) as u8
    }

    for (i, v) in seed.iter_mut().skip(8).enumerate() {
        *v = ((count >> (i * 8)) & 0xFF) as u8
    }

    let mut rng = rand::rngs::SmallRng::from_seed(seed);
    Ident::new(
        &(0..16)
            .map(|i| {
                if i == 0 || rng.gen() {
                    ('a' as u8 + rng.gen::<u8>() % 25) as char
                } else {
                    ('0' as u8 + rng.gen::<u8>() % 10) as char
                }
            })
            .collect::<String>(),
        Span::call_site(),
    )
}

/// Extracts `static mut` vars from the beginning of the given statements
fn extract_static_muts(stmts: Vec<Stmt>) -> Result<(Vec<ItemStatic>, Vec<Stmt>), parse::Error> {
    let mut istmts = stmts.into_iter();

    let mut seen = HashSet::new();
    let mut statics = vec![];
    let mut stmts = vec![];
    while let Some(stmt) = istmts.next() {
        match stmt {
            Stmt::Item(Item::Static(var)) => {
                if var.mutability.is_some() {
                    if seen.contains(&var.ident) {
                        return Err(parse::Error::new(
                            var.ident.span(),
                            format!("the name `{}` is defined multiple times", var.ident),
                        ));
                    }

                    seen.insert(var.ident.clone());
                    statics.push(var);
                } else {
                    stmts.push(Stmt::Item(Item::Static(var)));
                }
            }
            _ => {
                stmts.push(stmt);
                break;
            }
        }
    }

    stmts.extend(istmts);

    Ok((statics, stmts))
}
