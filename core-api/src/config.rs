//! Build-time settings: a constant whose value a deployment can set when it builds Vee.
//!
//! A setting is declared with `config::setting!`, in the crate that uses it (the core, or a
//! module): its name, type, default, and the range of values that make sense.
//!
//! ```ignore
//! config::setting! {
//!     /// How many bytes of text an event carries.
//!     pub EV_TEXT_MAX_BYTES: usize = 50, 1..=4096;
//! }
//! ```
//!
//! What the macro writes: a constant `EV_TEXT_MAX_BYTES` that is the value of the environment
//! variable `VEE_EV_TEXT_MAX_BYTES` (the name with `VEE_` in front) as read while compiling, or
//! the default if it is not set, and a check that the value is inside the range.
//!
//! A deployment sets the variable in the `[env]` table of a `.cargo/config.toml` in the directory
//! cargo is run from (see `sample-deployment/`). Where a value can come from, highest first:
//!
//! 1. the real environment (`VEE_EV_TEXT_MAX_BYTES=80 cargo build`)
//! 2. `cargo build --config some.toml`
//! 3. `.cargo/config.toml` files, the nearest directory first
//! 4. the default in the code
//!
//! A value that is not a plain whole number, is too big, or is outside its range stops the build
//! with an error that points at the setting. That is a compile error, never a panic in flight.
//! The default is checked against the range too.
//!
//! Two settings with the same name share one variable, so a module starts its names with its own
//! name (`FS_MAX_OPEN`). Everything built from a deployment (the core, the modules and the ground
//! tool) must be built with the same values, because some are part of the log format.
//!
//! To list every setting, search for `setting!`. To add one, declare it where it is used, and add
//! a line to the sample deployment's config.

/// Declares a build-time setting. See the module documentation.
#[macro_export]
macro_rules! setting {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident: $ty:ty = $default:literal, $min:literal ..= $max:literal $(;)?
    ) => {
        $(#[$meta])*
        // A minimum of 0 makes `$min <= value` always true for an unsigned type. That is fine.
        #[allow(unused_comparisons)]
        $vis const $name: $ty = {
            // The range is checked as a `usize`, so the largest allowed value must fit the type
            // or the `as` below could cut a value down into the range.
            assert!(
                $max as u128 <= <$ty>::MAX as u128,
                concat!("the range of ", stringify!($name), " does not fit its type")
            );
            let value = $crate::config::parse_or(
                option_env!(concat!("VEE_", stringify!($name))),
                $default,
            );
            assert!(
                $min <= value && value <= $max,
                concat!("VEE_", stringify!($name), " is outside its allowed range")
            );
            value as $ty
        };
    };
}

// So a setting is declared as `config::setting!`, like a function in a module.
pub use setting;

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

#[cfg(test)]
mod tests {
    use super as config;

    #[test]
    fn parse_or_gives_the_default_when_the_variable_is_not_set() {
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

    // Settings declared with the macro, to test what it writes. (Nothing sets these variables.)
    config::setting! {
        /// A plain setting.
        SAMPLE_SETTING: usize = 7, 1..=10
    }
    config::setting! {
        pub(super) SAMPLE_WIDE_SETTING: u64 = 1024, 1..=4_294_967_295
    }
    config::setting! {
        SAMPLE_SMALL_SETTING: u8 = 200, 0..=255;
    }

    #[test]
    fn a_setting_that_is_not_set_is_its_default() {
        assert_eq!(SAMPLE_SETTING, 7);
        assert_eq!(SAMPLE_WIDE_SETTING, 1024);
        assert_eq!(SAMPLE_SMALL_SETTING, 200);
    }

    #[test]
    fn a_setting_has_the_type_it_was_declared_with() {
        let _: usize = SAMPLE_SETTING;
        let _: u64 = SAMPLE_WIDE_SETTING;
        let _: u8 = SAMPLE_SMALL_SETTING;
    }
}
