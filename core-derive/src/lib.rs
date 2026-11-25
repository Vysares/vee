//! Derives for the core's shared traits. Proc macros run on the build machine, inside the
//! compiler, and write Rust code that the compiler then compiles as if it had been typed.
//!
//! `#[derive(Code)]`: see the example on `derive_code`.
//! `#[derive(Message)]`: see the example on `derive_message`.
//! `#[derive(Command)]`: see the example on `derive_command`.
//! `#[derive(CmdSet)]`: see the example on `derive_cmd_set`.
//!
//! Each derive's own logic (parsing its input, writing its generated code) is in its own file
//! (`code.rs`, `message.rs`, `command.rs`, `cmd_set.rs`). The `#[proc_macro_derive]` functions
//! themselves must stay here, at the crate root: that is where rustc requires them.
#![forbid(unsafe_code)]

mod cmd_set;
mod code;
mod command;
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

/// Writes an args struct's wire encoding and decoding, and (unless the struct provides its own)
/// a `validate` that always succeeds. This struct is not itself a command — it has no opcode of
/// its own — it is the payload a `#[derive(CmdSet)]` variant wraps; see `derive_cmd_set`.
///
/// ```ignore
/// #[derive(Clone, Copy, Command)]
/// struct SetEventFilterArgs {
///     floor: u8,
/// }
/// ```
///
/// - `#[validate]` is optional. Without it, the derive generates a `validate` that always
///   succeeds. With it, the derive generates none, and the struct provides its own `validate(&self)
///   -> Result<(), cmd::CmdErr>` by hand (see `docs/design.md`'s "hook for validate").
/// - Every field must be `bool` or a fixed-width number (`u8`..`u64`, `i8`..`i64`, `f32`, `f64`),
///   the same restriction `#[derive(Message)]` has. Fields are packed tightly in declaration
///   order, with no padding.
///
/// What it generates, on the struct itself:
/// - `const WIRE_LEN: usize`.
/// - `fn encode(&self) -> [u8; WIRE_LEN]`: the wire bytes, big endian, declaration order. A
///   sender turns this into `cmd::Args` itself (`cmd::Args::new(&value.encode())`), the same as
///   any other caller with bytes that might not fit somewhere.
/// - `fn decode(bytes: &[u8]) -> Result<Self, cmd::CmdErr>`: the reverse; `BadArgs` if `bytes` is
///   too short.
/// - A check, made when the code is compiled, that `WIRE_LEN` fits in `cmd::CMD_ARGS_MAX_BYTES`.
///
/// The generated code names the core-api crate as `::vee_core_api`, so the crate using the derive
/// must depend on it.
#[proc_macro_derive(Command, attributes(validate))]
pub fn derive_command(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match command::expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

/// Writes a module's (or the core's) command dispatch: an enum whose variants each wrap one
/// `#[derive(Command)]` args struct — the union design.md's `AtsCmd` example shows. A variant's
/// own discriminant is its opcode, the same convention `#[derive(Code)]` uses for event codes.
///
/// ```ignore
/// #[derive(CmdSet)]
/// #[repr(u16)]
/// enum CoreCmd {
///     SetEventFilter(SetEventFilterArgs) = 1,
/// }
/// ```
///
/// - `#[repr(u16)]` is required: Rust must be told what integer type a discriminant is before it
///   will accept one on a variant that carries a field, which every `CmdSet` variant does.
/// - Each variant must be a tuple variant wrapping exactly one `#[derive(Command)]` struct, as
///   `Name(Args) = opcode`.
///
/// What it generates, on the enum itself:
/// - `fn decode(raw: &cmd::RawCmd) -> Result<Self, cmd::CmdErr>`: matches `raw.opcode` against
///   each variant's own opcode and decodes that variant's args, or `UnknownOpcode` if none
///   matches.
/// - `fn validate(&self) -> Result<(), cmd::CmdErr>`: delegates to the decoded variant's own
///   `validate`.
///
/// The generated code names the core-api crate as `::vee_core_api`, so the crate using the derive
/// must depend on it.
#[proc_macro_derive(CmdSet)]
pub fn derive_cmd_set(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match cmd_set::expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}
