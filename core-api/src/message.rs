//! A message's wire layout: exact field kinds, a field's position, and reading one field out of
//! encoded bytes. See `docs/bus.md`, "Messages and their layout".
//!
//! `#[derive(bus::Message)]` writes two things, in two places:
//! - On the struct itself, as inherent items: `WIRE_LEN`, `LAYOUT` and `encode()`, for a typed
//!   caller that already knows the concrete message type (`Reading::LAYOUT`, `reading.encode()`).
//! - `impl Message for` the struct (below), for GENERIC code that only knows `T: Message` (the
//!   dyn view's `impl<T: Message> Topic for StateTopic<T>`, see `docs/bus.md`, "Access"). Its
//!   `WIRE_LEN`/`LAYOUT` repeat the same values the inherent ones hold; `encode_into` writes the
//!   same bytes `encode()` does, into a caller's buffer instead of returning a fixed-size array
//!   (an array sized by a generic `Self::WIRE_LEN` is not something a trait method can return on
//!   stable Rust).
//!
//! `read`, below, is written once, by hand, and works on any message's `LAYOUT`: a generic
//! consumer reads a field by its `Field`, with no knowledge of the concrete message type. Nothing
//! ties a `Field` to the bytes it is read from (see `docs/bus.md`, "Access", on why `Topic::read_latest`
//! exists instead of handing a `Field` to callers directly).
//!
//! Not supported yet (see docs/bus.md, "Open"): nested structs, arrays, text fields.

use crate::config;

config::setting! {
    /// The most bytes any message's `encode` writes. `Topic::read_latest`'s default needs a
    /// buffer this big on the stack to encode a value into before reading a field out of it
    /// (`docs/bus.md`'s "Settled": "a message is Copy plain data, up to about 3 kB"). Default
    /// 3072, 1..=65535.
    pub MESSAGE_MAX_BYTES: usize = 3072, 1..=65535;
}

/// A type that `#[derive(bus::Message)]` was used on: it has a wire encoding and a field layout.
/// Message types never implement this by hand; the derive does. Nothing needs it except the dyn
/// view's generic `Topic` impls (`bus.rs`): a typed caller uses the derive's inherent items
/// instead (`Reading::LAYOUT`, `reading.encode()`), which need no trait import.
pub trait Message: Copy {
    /// The size of this message's wire encoding. Same value as the derive's inherent `WIRE_LEN`.
    const WIRE_LEN: usize;
    /// This message type's field layout. Same value as the derive's inherent `LAYOUT`.
    const LAYOUT: &'static [Field];

    /// Writes this value's wire bytes into `out`, the same bytes `encode()` returns as an array,
    /// and returns how many were written (always `Self::WIRE_LEN`), or `None` if `out` is
    /// shorter than that: the message is never written half way.
    fn encode_into(&self, out: &mut [u8]) -> Option<usize>;
}

/// A field's exact wire type: how many bytes it is, and how to read them. Validation (a table
/// row's limit must fit the field's kind) and `read` both go by this, not by the Rust type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

/// One field of a message's wire layout. `#[derive(bus::Message)]` writes a `LAYOUT` of these,
/// one per field, in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The Rust field name (`stringify!`), for diagnostics and the ground dictionary. A table
    /// that names a field resolves it to an index into `LAYOUT` once, at load.
    pub name: &'static str,
    /// The field's offset in the WIRE encoding (`encode`'s output), never the in-memory struct:
    /// Rust does not fix a struct's layout, so only the wire layout can be relied on.
    pub offset: usize,
    pub kind: Kind,
}

/// A field's value, read out of wire bytes and widened so that values of different width or
/// signedness can be compared (a table row's limit need not be the field's exact kind).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
}

/// The `N` bytes at `offset`, as an array `from_be_bytes` takes directly, or `None` if they do
/// not fit in `bytes`.
fn chunk<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    bytes.get(offset..end)?.try_into().ok()
}

/// Reads one field out of a message's wire bytes, or `None` if `field.offset` and its kind's
/// width do not fit in `bytes` (a bad offset, never a panic). `bytes` is the whole message's
/// `encode`, not just this field's slice.
pub fn read(bytes: &[u8], field: &Field) -> Option<Value> {
    let offset = field.offset;
    Some(match field.kind {
        Kind::Bool => Value::Bool(*bytes.get(offset)? != 0),
        Kind::U8 => Value::Int(i128::from(u8::from_be_bytes(chunk(bytes, offset)?))),
        Kind::I8 => Value::Int(i128::from(i8::from_be_bytes(chunk(bytes, offset)?))),
        Kind::U16 => Value::Int(i128::from(u16::from_be_bytes(chunk(bytes, offset)?))),
        Kind::I16 => Value::Int(i128::from(i16::from_be_bytes(chunk(bytes, offset)?))),
        Kind::U32 => Value::Int(i128::from(u32::from_be_bytes(chunk(bytes, offset)?))),
        Kind::I32 => Value::Int(i128::from(i32::from_be_bytes(chunk(bytes, offset)?))),
        Kind::U64 => Value::Int(i128::from(u64::from_be_bytes(chunk(bytes, offset)?))),
        Kind::I64 => Value::Int(i128::from(i64::from_be_bytes(chunk(bytes, offset)?))),
        Kind::F32 => Value::Float(f64::from(f32::from_be_bytes(chunk(bytes, offset)?))),
        Kind::F64 => Value::Float(f64::from_be_bytes(chunk(bytes, offset)?)),
    })
}

#[cfg(test)]
mod tests {
    use super as message;

    const VOLTS: message::Field = message::Field {
        name: "volts",
        offset: 0,
        kind: message::Kind::F32,
    };
    const OK: message::Field = message::Field {
        name: "ok",
        offset: 4,
        kind: message::Kind::Bool,
    };

    #[test]
    fn the_default_message_max_bytes_is_3072() {
        assert_eq!(message::MESSAGE_MAX_BYTES, 3072);
    }

    #[test]
    fn reads_each_integer_kind_big_endian() {
        let cases: &[(message::Kind, &[u8], i128)] = &[
            (message::Kind::U8, &[7], 7),
            (message::Kind::I8, &[0xFF], -1),
            (message::Kind::U16, &[0x01, 0x02], 0x0102),
            (message::Kind::I16, &[0xFF, 0xFF], -1),
            (message::Kind::U32, &[0, 0, 1, 0], 256),
            (message::Kind::I32, &[0xFF, 0xFF, 0xFF, 0xFF], -1),
            (message::Kind::U64, &[0, 0, 0, 0, 0, 0, 0, 1], 1),
            (message::Kind::I64, &[0xFF; 8], -1),
        ];
        for (kind, bytes, expected) in cases {
            let field = message::Field {
                name: "x",
                offset: 0,
                kind: *kind,
            };
            assert_eq!(
                message::read(bytes, &field),
                Some(message::Value::Int(*expected))
            );
        }
    }

    /// The bytes of `value`, in an 8 byte array padded with zeros, and how many are the value's.
    fn be<const N: usize>(bytes: [u8; N]) -> ([u8; 8], usize) {
        let mut padded = [0u8; 8];
        for (slot, byte) in padded.iter_mut().zip(bytes) {
            *slot = byte;
        }
        (padded, N)
    }

    #[test]
    fn reads_the_smallest_and_largest_value_of_each_integer_kind() {
        let cases = [
            (message::Kind::U8, be(0u8.to_be_bytes()), 0),
            (message::Kind::U8, be(u8::MAX.to_be_bytes()), 255),
            (message::Kind::I8, be(i8::MIN.to_be_bytes()), -128),
            (message::Kind::I8, be(i8::MAX.to_be_bytes()), 127),
            (message::Kind::U16, be(u16::MAX.to_be_bytes()), 65_535),
            (message::Kind::I16, be(i16::MIN.to_be_bytes()), -32_768),
            (message::Kind::I16, be(i16::MAX.to_be_bytes()), 32_767),
            (
                message::Kind::U32,
                be(u32::MAX.to_be_bytes()),
                4_294_967_295,
            ),
            (
                message::Kind::I32,
                be(i32::MIN.to_be_bytes()),
                -2_147_483_648,
            ),
            (
                message::Kind::I32,
                be(i32::MAX.to_be_bytes()),
                2_147_483_647,
            ),
            (
                message::Kind::U64,
                be(u64::MAX.to_be_bytes()),
                i128::from(u64::MAX),
            ),
            (
                message::Kind::I64,
                be(i64::MIN.to_be_bytes()),
                i128::from(i64::MIN),
            ),
            (
                message::Kind::I64,
                be(i64::MAX.to_be_bytes()),
                i128::from(i64::MAX),
            ),
        ];
        for (kind, (padded, len), expected) in cases {
            let field = message::Field {
                name: "x",
                offset: 0,
                kind,
            };
            assert_eq!(
                message::read(&padded[..len], &field),
                Some(message::Value::Int(expected)),
                "{kind:?}"
            );
            // One byte short is `None`, never a partial read.
            assert_eq!(
                message::read(&padded[..len - 1], &field),
                None,
                "{kind:?} one byte short"
            );
        }
    }

    #[test]
    fn reads_a_64_bit_float_and_the_special_float_values() {
        let field = |kind| message::Field {
            name: "x",
            offset: 0,
            kind,
        };
        let f64_field = field(message::Kind::F64);
        assert_eq!(
            message::read(&(-2.25_f64).to_be_bytes(), &f64_field),
            Some(message::Value::Float(-2.25))
        );
        assert_eq!(
            message::read(&f64::INFINITY.to_be_bytes(), &f64_field),
            Some(message::Value::Float(f64::INFINITY))
        );
        assert_eq!(
            message::read(&f64::MIN_POSITIVE.to_be_bytes(), &f64_field),
            Some(message::Value::Float(f64::MIN_POSITIVE))
        );
        let Some(message::Value::Float(nan)) = message::read(&f64::NAN.to_be_bytes(), &f64_field)
        else {
            panic!("a NaN is still a float");
        };
        assert!(nan.is_nan());

        // A 32-bit float widens exactly.
        let f32_field = field(message::Kind::F32);
        assert_eq!(
            message::read(&f32::MAX.to_be_bytes(), &f32_field),
            Some(message::Value::Float(f64::from(f32::MAX)))
        );
        assert_eq!(
            message::read(&f32::NEG_INFINITY.to_be_bytes(), &f32_field),
            Some(message::Value::Float(f64::NEG_INFINITY))
        );
        // One byte short of either width is `None`.
        assert_eq!(message::read(&[0; 7], &f64_field), None);
        assert_eq!(message::read(&[0; 3], &f32_field), None);
    }

    #[test]
    fn reads_floats_and_bool() {
        let volts = 3.5_f32;
        assert_eq!(
            message::read(&volts.to_be_bytes(), &VOLTS),
            Some(message::Value::Float(f64::from(volts)))
        );
        assert_eq!(
            message::read(&[0, 0, 0, 0, 1], &OK),
            Some(message::Value::Bool(true))
        );
        assert_eq!(
            message::read(&[0, 0, 0, 0, 0], &OK),
            Some(message::Value::Bool(false))
        );
    }

    #[test]
    fn a_nonzero_byte_is_true_not_only_one() {
        assert_eq!(
            message::read(&[0, 0, 0, 0, 0xFF], &OK),
            Some(message::Value::Bool(true))
        );
    }

    #[test]
    fn an_offset_or_width_past_the_end_is_none_not_a_panic() {
        assert_eq!(message::read(&[1, 2, 3], &VOLTS), None); // F32 needs 4 bytes, only 3 given
        assert_eq!(message::read(&[1, 2, 3, 4], &OK), None); // OK's offset (4) is past the end
    }

    #[test]
    fn an_offset_at_the_very_end_of_usize_does_not_overflow() {
        let field = message::Field {
            name: "x",
            offset: usize::MAX,
            kind: message::Kind::U8,
        };
        assert_eq!(message::read(&[1, 2, 3], &field), None);
    }
}
