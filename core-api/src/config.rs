//! Build-time settings: how they are read, and the checks on their values.
//!
//! A setting is a constant defined next to the code that uses it, with its default there:
//!
//! ```ignore
//! pub const EV_TEXT_MAX_BYTES: usize = config::parse_or(option_env!("VEE_EV_TEXT_MAX_BYTES"), 50);
//! ```
//!
//! The environment variable is named `VEE_<NAME>` and read while compiling. A deployment sets it
//! in the `[env]` table of a `.cargo/config.toml` in the directory cargo is run from (see
//! `sample-deployment/`). Where a value can come from, highest first:
//!
//! 1. the real environment (`VEE_EV_TEXT_MAX_BYTES=80 cargo build`)
//! 2. `cargo build --config some.toml`
//! 3. `.cargo/config.toml` files, the nearest directory first
//! 4. the default in the code
//!
//! The checks on every setting's value are at the end of this file, so they can be audited in one
//! place. A value that is not a plain whole number, is too big, or breaks a check stops the build
//! with an error that points at the setting. That is a compile error, never a panic in flight.
//!
//! Everything built from a deployment (the core, the modules and the ground tool) must be built
//! with the same values, because some of them are part of the log format.
//!
//! To add a setting: define it where it is used (in this crate, or the check cannot see it), add
//! its check below, and add it to the sample deployment's config.

use crate::event;
use crate::reset;

/// The value of a setting: `text` (the environment variable, if it is set) read as a whole
/// number, or `default` if it is not set.
///
/// It is a `const fn` because it runs while the code is compiled. A `panic!` or a failed `assert!`
/// in a `const` is a compile error that stops the build, so a bad setting cannot reach flight.
pub const fn parse_or(text: Option<&str>, default: usize) -> usize {
    let Some(text) = text else {
        return default;
    };
    let digits = text.as_bytes();
    assert!(
        !digits.is_empty(),
        "a setting is empty: give it a number, or remove it to use the default"
    );
    let mut value: usize = 0;
    let mut index = 0;
    while index < digits.len() {
        let digit = digits[index];
        assert!(
            digit.is_ascii_digit(),
            "a setting must be a plain whole number, such as 50"
        );
        let Some(shifted) = value.checked_mul(10) else {
            panic!("a setting is too big");
        };
        let Some(next) = shifted.checked_add((digit - b'0') as usize) else {
            panic!("a setting is too big");
        };
        value = next;
        index += 1;
    }
    value
}

// ---- Checks on the settings ----

const _: () = assert!(
    event::EV_TEXT_MAX_BYTES >= 1,
    "VEE_EV_TEXT_MAX_BYTES must be at least 1"
);
const _: () = assert!(
    reset::RESET_LOG_MAX_RECORDS >= 1,
    "VEE_RESET_LOG_MAX_RECORDS must be at least 1"
);

#[cfg(test)]
mod tests {
    use super as config;

    #[test]
    fn a_setting_that_is_not_set_is_its_default() {
        assert_eq!(config::parse_or(None, 50), 50);
    }

    #[test]
    fn a_set_setting_is_its_number() {
        assert_eq!(config::parse_or(Some("80"), 50), 80);
        assert_eq!(config::parse_or(Some("0"), 50), 0);
        assert_eq!(config::parse_or(Some("007"), 50), 7);
    }

    #[test]
    fn the_largest_usize_is_accepted() {
        let largest = usize::MAX;
        // `usize::MAX` as text, written out by hand for both sizes.
        let text = if largest == u32::MAX as usize {
            "4294967295"
        } else {
            "18446744073709551615"
        };
        assert_eq!(config::parse_or(Some(text), 0), largest);
    }

    #[test]
    #[should_panic(expected = "empty")]
    fn an_empty_setting_is_rejected() {
        config::parse_or(Some(""), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn text_is_rejected() {
        config::parse_or(Some("abc"), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn a_minus_sign_is_rejected() {
        config::parse_or(Some("-1"), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn a_plus_sign_is_rejected() {
        config::parse_or(Some("+1"), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn a_space_is_rejected() {
        config::parse_or(Some(" 1"), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn an_underscore_is_rejected() {
        config::parse_or(Some("1_000"), 50);
    }

    #[test]
    #[should_panic(expected = "plain whole number")]
    fn a_decimal_point_is_rejected() {
        config::parse_or(Some("1.5"), 50);
    }

    #[test]
    #[should_panic(expected = "too big")]
    fn a_number_that_does_not_fit_is_rejected() {
        config::parse_or(Some("99999999999999999999999999"), 50);
    }
}
