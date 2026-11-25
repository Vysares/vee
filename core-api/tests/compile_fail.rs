//! The derives and `config::setting!` must refuse what they are meant to refuse, with a message
//! that says what to fix, and must accept what is exactly on the allowed edge. Each file in
//! `tests/ui/fail` is a small program that must NOT compile; the message the compiler gives is
//! saved next to it (`.stderr`) and compared, so a changed or lost error shows up here. Each
//! file in `tests/ui/pass` must compile: those sit exactly at a limit (the most event codes in
//! one enum, a command as big as `CMD_ARGS_MAX_BYTES`, a setting at the top of its range).
//!
//! The limits named in the file names are the DEFAULT build-time settings. A deployment that
//! changes `VEE_EV_MAX_CODES` or `VEE_CMD_ARGS_MAX_BYTES` must run these with the defaults.
//!
//! When the compiler's wording changes (a new Rust version), the saved messages change too:
//! review them, then update with `TRYBUILD=overwrite cargo test -p vee-core-api --test
//! compile_fail`.

#[test]
fn what_must_not_compile_does_not_and_what_is_on_the_edge_does() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/fail/*.rs");
    cases.pass("tests/ui/pass/*.rs");
}
