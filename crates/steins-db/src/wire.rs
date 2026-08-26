//! The artifact payload codec (ADR-0092 §2, issue #504): a compact,
//! schema-carrying binary form for the values the per-package sections store.
//!
//! # Why this is not serde_json
//!
//! Measured on `nikic/PHP-Parser` (341 files, 1.20 MiB of PHP) with
//! `cargo xtask artifact-bytes`, the JSON encoding of the lowered trees spends
//! its bytes like this:
//!
//! | bucket | share | is it content? |
//! |---|---|---|
//! | field names (`"span":`) | 53.9% | no — the schema is compiled in |
//! | structure (`{`, `[`, `,`) | 11.6% | no — replaced by length prefixes |
//! | strings | 23.4% | yes — names, spellings, docblocks |
//! | numbers, `true`/`false`/`null` | 11.1% | yes — spans, slots, discriminants |
//!
//! Two thirds of the section is the codec restating the schema on every node,
//! and the artifact is written *whole* on every publish (a container has no
//! partial write), so that two thirds is paid again on every warm edit. This
//! codec removes it: the reader knows the schema, so the bytes carry only what
//! the schema cannot.
//!
//! # The format
//!
//! Self-describing formats can be read without the type; this one cannot, and
//! that is the entire saving. Nothing on the wire says what anything *is*.
//!
//! | value | bytes |
//! |---|---|
//! | `bool` | one byte, `0` or `1` — nothing else |
//! | unsigned integer | LEB128, canonical (no redundant trailing zero group) |
//! | signed integer | zigzag, then LEB128 |
//! | `f32` / `f64` | 4 / 8 bytes, IEEE-754 bits, little-endian |
//! | `char` | its scalar value as an unsigned integer |
//! | `str`, bytes | length, then the bytes verbatim |
//! | `Option` | `0`, or `1` then the value |
//! | unit, unit struct | nothing |
//! | newtype struct | the inner value |
//! | seq, map | length, then the elements (a map's as key/value pairs) |
//! | tuple, tuple struct, struct | the fields in declaration order, no length |
//! | enum | the variant *index*, then the variant's payload |
//!
//! An enum travels by index, so **reordering a variant is a format change** —
//! as is reordering a struct's fields, or changing a field's type. None of that
//! is a hazard here and all of it would be under an interchange format: the
//! artifacts are a cache with a schema version and no migration path (ADR-0092
//! §2), so an artifact written by another schema is refused by
//! [`steins_gen::SCHEMA_VERSION`] before a byte of payload is read, and the
//! remedy for every disagreement is one rebuild.
//!
//! # Strictness
//!
//! The house discipline the JSON codec had, kept: the inverse is strict, every
//! failure is an error the section reader degrades to a [`steins_gen::Miss`],
//! and no partial value ever escapes. Specifically —
//!
//! * [`from_slice`] refuses **trailing bytes**: a payload is exactly one value.
//! * A `bool` byte outside `{0, 1}`, an `Option` tag outside `{0, 1}`, a
//!   `char` outside the Unicode scalar values, a `str` that is not UTF-8, an
//!   integer wider than its type, and a variant index the enum does not have
//!   are each an error.
//! * A length prefix that overruns the remaining input is an error at the
//!   point it is read, so a doctored length allocates nothing.
//! * LEB128 is read **canonically**: an encoding with a redundant continuation
//!   is refused, so one value has exactly one encoding.
//! * Decoding is depth-limited to [`RECURSION_LIMIT`], the same ceiling
//!   serde_json applied to these payloads before the swap — a hostile payload
//!   cannot recurse the decoder into the stack guard.
//!
//! What this codec deliberately cannot do is decode a value whose shape is not
//! known statically: [`serde::Deserializer::deserialize_any`] is an error, by
//! construction. That is why the two sections whose payload is a dynamic
//! `serde_json::Value` — the fold table's rows (ADR-0092 §4) and the
//! generation's own identity block — keep JSON, and it is the line between the
//! two codecs.

use std::fmt;

use serde::de::{self, IntoDeserializer};
use serde::{Deserialize, Serialize, ser};

/// The decoder's nesting ceiling — the same number serde_json applies to its
/// own deserializer, so a payload this codec accepts is one the previous codec
/// would have accepted too. Nesting deeper than this was already a miss (see
/// `crate::persist`'s module docs) and stays one.
pub const RECURSION_LIMIT: usize = 128;

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Why a value would not encode, or why bytes are not one.
///
/// The section readers map every one of these to a [`steins_gen::Miss`], so
/// the message is for a developer reading a test failure, never for a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

/// Shorthand for this module's results.
type Result<T> = std::result::Result<T, Error>;

fn err(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

// ---------------------------------------------------------------------------
// The entry points.
// ---------------------------------------------------------------------------

/// Encode one value. Fails only where serde itself cannot describe the value
/// to a non-self-describing format — a sequence or map of unknown length, or a
/// `Serialize` impl that raises its own error.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut ser = Serializer { out: Vec::new() };
    value.serialize(&mut ser)?;
    Ok(ser.out)
}

/// Decode one value from exactly `bytes`. Trailing bytes are an error: a
/// payload is one value and nothing after it.
pub fn from_slice<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T> {
    let mut de = Deserializer { input: bytes, depth: 0 };
    let value = T::deserialize(&mut de)?;
    if de.input.is_empty() {
        Ok(value)
    } else {
        Err(err(format!("{} trailing byte(s) after the value", de.input.len())))
    }
}

// ---------------------------------------------------------------------------
// Varints.
// ---------------------------------------------------------------------------

/// The most bytes a canonical LEB128 `u64` occupies.
const MAX_VARINT_LEN: usize = 10;

fn write_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Zigzag, so a small negative costs one byte rather than ten.
const fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

const fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// ---------------------------------------------------------------------------
// The serializer.
// ---------------------------------------------------------------------------

struct Serializer {
    out: Vec<u8>,
}

impl Serializer {
    fn len_prefixed(&mut self, bytes: &[u8]) {
        write_uvarint(&mut self.out, bytes.len() as u64);
        self.out.extend_from_slice(bytes);
    }
}

/// Serialization is *not* depth-limited, deliberately and exactly as
/// serde_json was: the value comes from memory, so its depth is whatever the
/// lowering built, and refusing to write a tree the analyzer already holds
/// would turn a deep expression into a lost cache entry at write time instead
/// of at read time.
impl ser::Serializer for &mut Serializer {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.out.push(u8::from(v));
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i64(self, v: i64) -> Result<()> {
        write_uvarint(&mut self.out, zigzag(v));
        Ok(())
    }

    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u32(self, v: u32) -> Result<()> {
        self.serialize_u64(u64::from(v))
    }

    fn serialize_u64(self, v: u64) -> Result<()> {
        write_uvarint(&mut self.out, v);
        Ok(())
    }

    fn serialize_f32(self, v: f32) -> Result<()> {
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_f64(self, v: f64) -> Result<()> {
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_u64(u64::from(u32::from(v)))
    }

    fn serialize_str(self, v: &str) -> Result<()> {
        self.len_prefixed(v.as_bytes());
        Ok(())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.len_prefixed(v);
        Ok(())
    }

    fn serialize_none(self) -> Result<()> {
        self.out.push(0);
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<()> {
        self.out.push(1);
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
    ) -> Result<()> {
        self.serialize_u32(index)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<()> {
        write_uvarint(&mut self.out, u64::from(index));
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
        let len = len.ok_or_else(|| err("a sequence of unknown length cannot be encoded"))?;
        write_uvarint(&mut self.out, len as u64);
        Ok(self)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        write_uvarint(&mut self.out, u64::from(index));
        Ok(self)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
        let len = len.ok_or_else(|| err("a map of unknown length cannot be encoded"))?;
        write_uvarint(&mut self.out, len as u64);
        Ok(self)
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        write_uvarint(&mut self.out, u64::from(index));
        Ok(self)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

impl ser::SerializeSeq for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTuple for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeMap for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        key.serialize(&mut **self)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeStruct for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for &mut Serializer {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The deserializer.
// ---------------------------------------------------------------------------

struct Deserializer<'de> {
    input: &'de [u8],
    depth: usize,
}

impl<'de> Deserializer<'de> {
    fn take(&mut self, n: usize) -> Result<&'de [u8]> {
        if self.input.len() < n {
            return Err(err(format!(
                "the payload ends {} byte(s) early",
                n - self.input.len()
            )));
        }
        let (head, tail) = self.input.split_at(n);
        self.input = tail;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Read one canonical LEB128 unsigned integer.
    fn uvarint(&mut self) -> Result<u64> {
        let mut value: u64 = 0;
        for group in 0..MAX_VARINT_LEN {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7f);
            if group == MAX_VARINT_LEN - 1 && payload > 1 {
                return Err(err("an integer wider than 64 bits"));
            }
            value |= payload << (7 * group);
            if byte & 0x80 == 0 {
                // One value, one encoding: a continuation that added nothing
                // would give `3` two spellings, and a strict inverse has none.
                if group > 0 && byte == 0 {
                    return Err(err("a non-canonical integer encoding"));
                }
                return Ok(value);
            }
        }
        Err(err("an integer wider than 64 bits"))
    }

    fn ivarint(&mut self) -> Result<i64> {
        Ok(unzigzag(self.uvarint()?))
    }

    /// A length prefix, refused at once when it overruns what is left — so a
    /// doctored length is an error rather than an allocation.
    fn length(&mut self) -> Result<usize> {
        let len = self.uvarint()?;
        let len = usize::try_from(len).map_err(|_| err("a length wider than this machine"))?;
        if len > self.input.len() {
            return Err(err("a length prefix longer than the remaining payload"));
        }
        Ok(len)
    }

    fn bytes(&mut self) -> Result<&'de [u8]> {
        let len = self.length()?;
        self.take(len)
    }

    fn str(&mut self) -> Result<&'de str> {
        std::str::from_utf8(self.bytes()?).map_err(|_| err("a string that is not UTF-8"))
    }

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > RECURSION_LIMIT {
            return Err(err("a payload nested deeper than the decoder's ceiling"));
        }
        Ok(())
    }
}

/// Narrow a decoded `u64` to the width the schema declares. A stored value
/// that does not fit is a decode error, never a silent truncation.
fn narrow_u<T: TryFrom<u64>>(v: u64) -> Result<T> {
    T::try_from(v).map_err(|_| err("an unsigned integer wider than its field"))
}

fn narrow_i<T: TryFrom<i64>>(v: i64) -> Result<T> {
    T::try_from(v).map_err(|_| err("a signed integer wider than its field"))
}

macro_rules! forward_to_uint {
    ($($method:ident => $visit:ident : $ty:ty),* $(,)?) => {
        $(
            fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
                let v = self.uvarint()?;
                visitor.$visit(narrow_u::<$ty>(v)?)
            }
        )*
    };
}

macro_rules! forward_to_int {
    ($($method:ident => $visit:ident : $ty:ty),* $(,)?) => {
        $(
            fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
                let v = self.ivarint()?;
                visitor.$visit(narrow_i::<$ty>(v)?)
            }
        )*
    };
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = Error;

    /// The whole point of the format: nothing on the wire says what it is, so
    /// a value whose shape is not known statically cannot be read.
    fn deserialize_any<V: de::Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(err("the wire codec is not self-describing"))
    }

    fn deserialize_ignored_any<V: de::Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(err("the wire codec cannot skip a value it does not know"))
    }

    fn deserialize_bool<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.byte()? {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            _ => Err(err("a boolean byte that is neither zero nor one")),
        }
    }

    forward_to_int! {
        deserialize_i8 => visit_i8: i8,
        deserialize_i16 => visit_i16: i16,
        deserialize_i32 => visit_i32: i32,
    }

    fn deserialize_i64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let v = self.ivarint()?;
        visitor.visit_i64(v)
    }

    forward_to_uint! {
        deserialize_u8 => visit_u8: u8,
        deserialize_u16 => visit_u16: u16,
        deserialize_u32 => visit_u32: u32,
    }

    fn deserialize_u64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let v = self.uvarint()?;
        visitor.visit_u64(v)
    }

    fn deserialize_f32<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bits = self.take(4)?;
        visitor.visit_f32(f32::from_bits(u32::from_le_bytes(
            bits.try_into().expect("four bytes read"),
        )))
    }

    fn deserialize_f64<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let bits = self.take(8)?;
        visitor.visit_f64(f64::from_bits(u64::from_le_bytes(
            bits.try_into().expect("eight bytes read"),
        )))
    }

    fn deserialize_char<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let scalar: u32 = narrow_u(self.uvarint()?)?;
        let c = char::from_u32(scalar).ok_or_else(|| err("not a Unicode scalar value"))?;
        visitor.visit_char(c)
    }

    fn deserialize_str<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let s = self.str()?;
        visitor.visit_borrowed_str(s)
    }

    fn deserialize_string<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let b = self.bytes()?;
        visitor.visit_borrowed_bytes(b)
    }

    fn deserialize_byte_buf<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.byte()? {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(self),
            _ => Err(err("an option tag that is neither zero nor one")),
        }
    }

    fn deserialize_unit<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.uvarint()?;
        let len = usize::try_from(len).map_err(|_| err("a length wider than this machine"))?;
        self.enter()?;
        let out = visitor.visit_seq(Elements { de: &mut *self, remaining: len });
        self.depth -= 1;
        out
    }

    fn deserialize_tuple<V: de::Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        self.enter()?;
        let out = visitor.visit_seq(Elements { de: &mut *self, remaining: len });
        self.depth -= 1;
        out
    }

    fn deserialize_tuple_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.uvarint()?;
        let len = usize::try_from(len).map_err(|_| err("a length wider than this machine"))?;
        self.enter()?;
        let out = visitor.visit_map(Elements { de: &mut *self, remaining: len });
        self.depth -= 1;
        out
    }

    /// A struct is its fields, in declaration order — the names are the
    /// schema's, not the payload's. `deny_unknown_fields` has nothing to do
    /// here for the same reason: a field this schema does not spell cannot be
    /// written down.
    fn deserialize_struct<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: de::Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.enter()?;
        let out = visitor.visit_enum(Variant { de: &mut *self });
        self.depth -= 1;
        out
    }

    /// Reached only through [`de::EnumAccess::variant_seed`], which hands the
    /// index over directly; a bare identifier has no spelling on this wire.
    fn deserialize_identifier<V: de::Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(err("the wire codec carries no identifiers"))
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

/// A counted run of values: a sequence's elements, a tuple's or struct's
/// fields, or a map's key/value pairs.
struct Elements<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    remaining: usize,
}

impl<'de> de::SeqAccess<'de> for Elements<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

impl<'de> de::MapAccess<'de> for Elements<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(&mut *self.de)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

/// One enum value: the variant index, then whatever that variant carries.
struct Variant<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'a, 'de> de::EnumAccess<'de> for Variant<'a, 'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: de::DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let index: u32 = narrow_u(self.de.uvarint()?)?;
        // An index the enum does not have fails here, in the derived
        // identifier visitor — the closed-set check the variant names bought
        // under the old codec, bought by position instead.
        let value = seed.deserialize(index.into_deserializer())?;
        Ok((value, self))
    }
}

impl<'de> de::VariantAccess<'de> for Variant<'_, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(&mut *self.de)
    }

    fn tuple_variant<V: de::Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Elements { de: &mut *self.de, remaining: len })
    }

    fn struct_variant<V: de::Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_seq(Elements { de: &mut *self.de, remaining: fields.len() })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;

    /// Every scalar shape, through the wire and back.
    #[test]
    fn scalars_round_trip() {
        fn trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(v: T) {
            let bytes = to_vec(&v).expect("encodes");
            assert_eq!(from_slice::<T>(&bytes).expect("decodes"), v);
        }
        trip(true);
        trip(false);
        trip(0u8);
        trip(u8::MAX);
        trip(u16::MAX);
        trip(u32::MAX);
        trip(u64::MAX);
        trip(0usize);
        trip(usize::MAX);
        trip(i8::MIN);
        trip(i64::MIN);
        trip(i64::MAX);
        trip(-1i32);
        trip('a');
        trip('\u{10FFFF}');
        trip(String::new());
        trip("héllo".to_owned());
        trip(Option::<u32>::None);
        trip(Some(7u32));
        trip(Some(Some(0u8)));
        trip(());
        trip((1u8, "x".to_owned(), false));
        trip(vec![1u64, 2, 3]);
        trip(Vec::<String>::new());
        trip(BTreeMap::from([("a".to_owned(), 1u8), ("b".to_owned(), 2)]));
        trip(HashMap::from([(3u32, vec![true, false])]));
    }

    /// Non-finite floats survive as bit patterns — the corner the payloads'
    /// `f64_bits` codec exists for, asserted here on the raw float too.
    #[test]
    fn non_finite_floats_round_trip_by_bits() {
        for v in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.0f64, f64::MIN, f64::MAX] {
            let bytes = to_vec(&v).expect("encodes");
            let back: f64 = from_slice(&bytes).expect("decodes");
            assert_eq!(back.to_bits(), v.to_bits(), "{v}");
        }
        for v in [f32::INFINITY, f32::NAN, -0.0f32] {
            let bytes = to_vec(&v).expect("encodes");
            let back: f32 = from_slice(&bytes).expect("decodes");
            assert_eq!(back.to_bits(), v.to_bits(), "{v}");
        }
    }

    /// Arbitrary bytes — the non-UTF-8 corner `PhpStr` rides on — travel
    /// verbatim, and cost exactly what the same bytes as a string would.
    #[test]
    fn byte_strings_travel_verbatim() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper(#[serde(with = "serde_bytes_shim")] Vec<u8>);

        mod serde_bytes_shim {
            pub fn serialize<S: serde::Serializer>(
                v: &[u8],
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_bytes(v)
            }
            pub fn deserialize<'de, D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Vec<u8>, D::Error> {
                struct V;
                impl serde::de::Visitor<'_> for V {
                    type Value = Vec<u8>;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("bytes")
                    }
                    fn visit_bytes<E: serde::de::Error>(
                        self,
                        v: &[u8],
                    ) -> std::result::Result<Vec<u8>, E> {
                        Ok(v.to_vec())
                    }
                }
                d.deserialize_bytes(V)
            }
        }

        let raw = Wrapper(vec![0xC0, 0xC1, 0x00, 0xFF]);
        let bytes = to_vec(&raw).expect("encodes");
        assert_eq!(from_slice::<Wrapper>(&bytes).expect("decodes"), raw);
        assert_eq!(bytes.len(), 5, "one length byte plus four content bytes");
        assert_eq!(to_vec("abcd").expect("encodes").len(), 5, "a string costs the same");
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    #[serde(deny_unknown_fields)]
    enum Shape {
        Unit,
        Newtype(u32),
        Tuple(u8, bool),
        Named { a: String, b: Option<u64> },
    }

    /// Enums travel by variant index, payload included.
    #[test]
    fn enum_variants_round_trip_by_index() {
        for v in [
            Shape::Unit,
            Shape::Newtype(300),
            Shape::Tuple(1, true),
            Shape::Named { a: "x".to_owned(), b: None },
            Shape::Named { a: String::new(), b: Some(u64::MAX) },
        ] {
            let bytes = to_vec(&v).expect("encodes");
            assert_eq!(from_slice::<Shape>(&bytes).expect("decodes"), v);
        }
        assert_eq!(to_vec(&Shape::Unit).expect("encodes"), vec![0]);
        assert_eq!(to_vec(&Shape::Newtype(1)).expect("encodes"), vec![1, 1]);
    }

    /// Every way bytes can fail to be a value: a variant the enum does not
    /// have, a truncated payload, trailing bytes, a non-canonical integer, an
    /// over-long integer, a length that overruns, a bad boolean, a bad option
    /// tag, and a string that is not UTF-8.
    #[test]
    fn a_doctored_payload_is_an_error_never_a_partial_value() {
        assert!(from_slice::<Shape>(&[9]).is_err(), "a variant index out of range");
        assert!(from_slice::<Shape>(&[1]).is_err(), "a newtype variant with no payload");
        assert!(from_slice::<Shape>(&[0, 0]).is_err(), "trailing bytes");
        assert!(from_slice::<u64>(&[0x81, 0x00]).is_err(), "a non-canonical integer");
        assert!(from_slice::<u64>(&[0xFF; 11]).is_err(), "an integer wider than 64 bits");
        assert!(from_slice::<u32>(&[0x80, 0x80, 0x80, 0x80, 0x10]).is_err(), "wider than u32");
        assert!(from_slice::<String>(&[9, b'a']).is_err(), "a length past the end");
        assert!(from_slice::<bool>(&[2]).is_err(), "a boolean byte that is neither zero nor one");
        assert!(from_slice::<Option<u8>>(&[2, 0]).is_err(), "an option tag that is neither zero nor one");
        assert!(from_slice::<String>(&[2, 0xC0, 0xC1]).is_err(), "a string that is not UTF-8");
        assert!(from_slice::<Vec<u32>>(&[3, 1]).is_err(), "a sequence shorter than its length");
    }

    /// The decoder's ceiling is a decode error, not a stack overflow.
    #[test]
    fn nesting_past_the_ceiling_is_an_error() {
        #[derive(Serialize, Deserialize, Debug)]
        struct Nest(Option<Box<Nest>>);

        let mut deep = Nest(None);
        for _ in 0..(RECURSION_LIMIT + 8) {
            deep = Nest(Some(Box::new(deep)));
        }
        // `Option` adds no nesting level, so a chain of newtype structs is the
        // shape that must *not* trip the guard; a chain of sequences is.
        let bytes = to_vec(&deep).expect("encodes");
        assert!(from_slice::<Nest>(&bytes).is_ok(), "options and newtypes do not nest");

        let mut nested: Vec<u8> = vec![0];
        for _ in 0..(RECURSION_LIMIT + 8) {
            let mut next = vec![1u8];
            next.extend_from_slice(&nested);
            nested = next;
        }
        // A sequence-of-sequence chain deeper than the ceiling: one element at
        // every level, so the bytes are trivially well-formed and only the
        // depth is wrong.
        assert!(
            from_slice::<DeepSeq>(&nested).is_err(),
            "a sequence nested past the ceiling must be a decode error"
        );
    }

    /// A sequence that contains itself: the shape whose only limit is the
    /// decoder's ceiling. Nothing reads the field — the decode is the test.
    #[derive(Deserialize, Debug)]
    struct DeepSeq(#[allow(dead_code)] Vec<DeepSeq>);

    /// Not self-describing, and it says so rather than guessing.
    #[test]
    fn a_dynamic_value_cannot_be_decoded() {
        assert!(from_slice::<serde_json::Value>(&[1, 2, 3]).is_err());
    }
}
