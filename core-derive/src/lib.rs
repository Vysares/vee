//! Derives for the core's shared traits. Proc macros run on the build machine, inside the
//! compiler, and write Rust code that the compiler then compiles as if it had been typed.
//!
//! `#[derive(Code)]`: see the example on `derive_code`.
//! `#[derive(Message)]`: see the example on `derive_message`.
//!
//! Each derive's own logic (parsing its input, writing its generated code) is in its own file
//! (`code.rs`, `message.rs`). The `#[proc_macro_derive]` functions themselves must stay here, at
//! the crate root: that is where rustc requires them.
#![forbid(unsafe_code)]

mod code;
mod message;

use proc_macro::TokenStream;

/// Writes `impl vee_core_api::event::Code` for an enum of event codes.
///
/// ```ignore
/// #[derive(Code)]
/// #[repr(u16)]
/// enum FsEv {
///     #[severity(Info)] Started,
///     #[severity(Err)] OpenFailed,
/// }
/// ```
///
/// - `#[repr(u16)]`: the value of a variant is the event's code. Values not written count up
///   from 0, as for any enum, and Rust refuses two variants with the same value, so codes are
///   unique. A variant may be given a value (`Started = 0x01`) to pin its code. Without one, a
///   code is the variant's position, so reordering or inserting variants renumbers the codes that
///   follow.
/// - `#[severity(Level)]` on every variant. The level is `Info`, `Warn` or `Err`.
/// - Variants have no fields: the event's details go in its text, not in the code.
///
/// What it generates:
/// - `impl event::Code`: `code`, `severity`, `index` and `from_index`. Each is one `match` with
///   an arm per variant (`code` casts the variant with `as u16`).
/// - A check, made when the code is compiled, that the enum has no more variants than
///   `throttle::EV_MAX_CODES`, the number of counters in a `Throttle`.
///
/// The generated code names the core-api crate as `::vee_core_api`, so the crate using the
/// derive must depend on it.
#[proc_macro_derive(Code, attributes(severity))]
pub fn derive_code(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match code::expand(&input) {
        Ok(tokens) => tokens.into(),
        // A `syn::Error` becomes a `compile_error!` that points at the code that caused it.
        Err(error) => error.into_compile_error().into(),
    }
}

/// Writes a message type's wire encoding and field layout, as an inherent `impl` (not a trait:
/// nothing needs one yet, see `docs/bus.md`, "Messages and their layout").
///
/// ```ignore
/// #[derive(Clone, Copy, Message)]
/// struct Reading {
///     volts: f32,
///     ok: bool,
/// }
/// ```
///
/// - Every field must be `bool` or a fixed-width number (`u8`..`u64`, `i8`..`i64`, `f32`, `f64`).
///   Nested structs, arrays and text fields are not supported yet.
/// - Fields are packed tightly in declaration order, with no padding: the WIRE layout, which is
///   not the same as the in-memory struct's (Rust does not fix that).
///
/// What it generates, on the struct itself (`Reading::WIRE_LEN`, not a trait method):
/// - `const WIRE_LEN: usize`: the size of `encode`'s output.
/// - `const LAYOUT: &'static [message::Field]`: one entry per field, in declaration order.
/// - `fn encode(&self) -> [u8; WIRE_LEN]`: the wire bytes, big endian, declaration order.
///
/// The generated code names the core-api crate as `::vee_core_api`, so the crate using the derive
/// must depend on it.
#[proc_macro_derive(Message)]
pub fn derive_message(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match message::expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}
