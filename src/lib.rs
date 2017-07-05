//! Minimal startup / runtime for MSP430 microcontrollers
//!
//! This crate is based on [cortex-m-rt](https://docs.rs/cortex-m-rt)
//! crate by Jorge Aparicio (@japaric).
//!
//! # Features
//!
//! This crate provides
//!
//! - Before main initialization of the `.bss` and `.data` sections
//!
//! - An overridable (\*) `panic_fmt` implementation that does nothing.
//!
//! - A minimal `start` lang item, to support vanilla `fn main()`. NOTE the
//!   processor goes into infinite loop after returning from `main`.
//!
//! - An opt-in linker script (`"linker-script"` Cargo feature) that encodes
//!   the memory layout of a generic MSP430 microcontroller. This linker
//!   script is missing the definitions of the FLASH, RAM and VECTORS memory
//!   regions of the device and of the `_stack_start` symbol (address where the
//!   call stack is allocated). This missing information must be supplied
//!   through a `memory.x` file (see example below).
//!
//! - A `_sheap` symbol at whose address you can locate the heap.
//!
//! (\*) To override the `panic_fmt` implementation, simply create a new
//! `rust_begin_unwind` symbol:
//!
//! ```
//! #[no_mangle]
//! pub unsafe extern "C" fn rust_begin_unwind(
//!     _args: ::core::fmt::Arguments,
//!     _file: &'static str,
//!     _line: u32,
//! ) -> ! {
//!     ..
//! }
//! ```
//!
//! (\*\*) All the device specific exceptions, i.e. the interrupts, are left
//! unpopulated. You must fill that part of the vector table by defining the
//! following static (with the right memory layout):
//!
//! ``` ignore,no_run
//! #[link_section = ".rodata.interrupts"]
//! #[used]
//! static INTERRUPTS: SomeStruct = SomeStruct { .. }
//! ```
//!
//! # Example
//!
//! ``` text
//! $ cargo new --bin app && cd $_
//!
//! $ cargo add msp430 msp430-rt
//!
//! $ edit Xargo.toml && cat $_
//! ```
//!
//! ``` text
//! [dependencies.core]
//!
//! [dependencies.compiler_builtins]
//! features = ["mem"]
//! git = "https://github.com/rust-lang-nursery/compiler-builtins"
//! stage = 1
//! ```
//!
//! ``` text
//! $ edit memory.x && cat $_
//! ```
//!
//! ``` text
//! MEMORY
//! {
//!   RAM              : ORIGIN = 0x0200, LENGTH = 0x0200
//!   FLASH            : ORIGIN = 0xC000, LENGTH = 0x3FDE
//!   VECTORS          : ORIGIN = 0xFFE0, LENGTH = 0x0020
//! }
//!
//! /* This is where the call stack will be allocated */
//! _stack_start = ORIGIN(RAM) + LENGTH(RAM);
//! ```
//!
//! ``` text
//! $ edit src/main.rs && cat $_
//! ```
//!
//! ``` ignore,no_run
//! #![feature(used)]
//! #![feature(abi_msp430_interrupt)]
//! #![no_std]
//!
//! extern crate msp430;
//! extern crate msp430_rt;
//!
//! use msp430::asm;
//!
//! fn main() {
//!     asm::nop();
//! }
//!
//! // As we are not using interrupts, we just register a dummy catch all
//! // handler
//! #[link_section = ".vector_table.interrupts"]
//! #[used]
//! static INTERRUPTS: [extern "msp430-interrupt" fn(); 15] =
//!     [default_handler; 15];
//!
//! extern "msp430-interrupt" fn default_handler() {
//!     loop {
//!     }
//! }
//! ```
//!
//! ``` text
//! $ cargo install xargo
//!
//! $ xargo rustc --target msp430 --release -- \
//!       -C link-arg=-Tlink.x \
//!       -C link-arg=-mmcu=msp430g2553 -C link-arg=-nostartfiles \
//!       -C linker=msp430-elf-gcc -Z linker-flavor=gcc
//!
//! $ msp430-elf-objdump -Cd $(find target -name app) | head
//!
//! Disassembly of section .text:
//!
//! 0000c000 <msp430_rt::reset_handler::h77ef04785a7efdda>:
//!     c000:	31 40 00 04 	mov	#1024,	r1	;#0x0400
//!     c004:	30 40 28 c0 	br	#0xc028		;
//! ```

#![cfg_attr(target_arch = "msp430", feature(core_intrinsics))]
#![deny(missing_docs)]
#![deny(warnings)]
#![feature(abi_msp430_interrupt)]
#![feature(asm)]
#![feature(compiler_builtins_lib)]
#![feature(lang_items)]
#![feature(linkage)]
#![feature(naked_functions)]
#![feature(used)]
#![no_std]

extern crate compiler_builtins;
extern crate msp430;
extern crate r0;

use msp430::interrupt;

mod lang_items;

#[cfg(target_arch = "msp430")]
extern "C" {
    // NOTE `rustc` forces this signature on us. See `src/lang_items.rs`
    fn main(argc: isize, argv: *const *const u8) -> isize;

    // Boundaries of the .bss section
    static mut _ebss: u16;
    static mut _sbss: u16;

    // Boundaries of the .data section
    static mut _edata: u16;
    static mut _sdata: u16;

    // Initial values of the .data section (stored in ROM)
    static _sidata: u16;
}

/// The reset handler.
#[cfg(target_arch = "msp430")]
unsafe extern "C" fn reset_handler() -> ! {
    r0::zero_bss(&mut _sbss, &mut _ebss);
    r0::init_data(&mut _sdata, &mut _edata, &_sidata);

    // Neither `argc` or `argv` make sense in bare metal context so we just
    // stub them
    main(0, core::ptr::null());

    // If `main` returns, then we go into infinite loop and wait for
    // interrupts.
    loop {}

    // This is the entry point of all programs
    #[link_section = ".vector_table.reset_handler"]
    #[naked]
    unsafe extern "msp430-interrupt" fn trampoline() -> ! {
        // "trampoline" to get to the real reset handler.
        asm!("mov #_stack_start, r1
              br $0"
             :
             : "i"(reset_handler as unsafe extern "C" fn() -> !)
             :
             : "volatile"
        );

        core::intrinsics::unreachable()
    }

    #[link_section = ".vector_table.reset_vector"]
    #[used]
    static RESET_VECTOR: unsafe extern "msp430-interrupt" fn() -> ! =
        trampoline;
}

#[allow(non_snake_case)]
#[allow(private_no_mangle_fns)]
#[linkage = "weak"]
#[no_mangle]
extern "C" fn DEFAULT_HANDLER() {
    interrupt::disable();
    loop {}
}

// make sure the compiler emits the DEFAULT_HANDLER symbol so the linker can
// find it!
#[used]
static KEEP: extern "C" fn() = DEFAULT_HANDLER;

/// This macro lets you override the default exception handler
///
/// The first and only argument to this macro is the path to the function that
/// will be used as the default handler. That function must have signature
/// `fn()`
///
/// # Examples
///
/// ``` ignore
/// default_handler!(foo::bar);
///
/// mod foo {
///     pub fn bar() {
///         ::cortex_m::asm::bkpt();
///         loop {}
///     }
/// }
/// ```
#[macro_export]
macro_rules! default_handler {
    ($body:path) => {
        #[allow(non_snake_case)]
        #[doc(hidden)]
        #[no_mangle]
        pub unsafe extern "C" fn DEFAULT_HANDLER() {
            // type checking
            let f: fn() = $body;
            f();
        }
    }
}
