//! A message's values on the wire. Both ends know the shape, so nothing names or tags a value:
//!
//! ```text
//! [ flag bits, packed, in field order: one per optional (present?), boolean (true?) and number
//!   (whole and small, so a varint?) ] [ values in field order ]
//! number  → f64, or a zigzag varint when whole and within ±2^53
//! string  → varint length, then UTF-8
//! Color, vector → 4 × f32
//! list    → varint count, then each item's flags and value
//! ```
//!
//! A list's items each carry their own flag bits, since the count isn't known up front.

use crate::Shape;

/// A value a message carries, shaped like its `Shape`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    /// A `Color` or `vector`: x, y, z, w
    Vector([f32; 4]),
    List(Vec<Value>),
    /// Fields in the shape's order
    Record(Vec<Value>),
    /// An optional field left out
    Absent,
}

const WHOLE_LIMIT: f64 = 9_007_199_254_740_992.0;

struct Bits {
    bytes: Vec<u8>,
    count: usize,
}

impl Bits {
    fn push(&mut self, bit: bool) {
        if self.count.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit {
            *self.bytes.last_mut().expect("a byte for this bit") |= 1 << (self.count % 8);
        }
        self.count += 1;
    }
}

fn whole(n: f64) -> Option<i64> {
    (n.fract() == 0.0 && n.abs() < WHOLE_LIMIT).then_some(n as i64)
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

/// The flag bits of a value's top level (lists start their own).
fn flags(shape: &Shape, value: &Value, bits: &mut Bits) {
    match (shape, value) {
        (Shape::Optional(_), Value::Absent) => bits.push(false),
        (Shape::Optional(inner), present) => {
            bits.push(true);
            flags(inner, present, bits);
        }
        (Shape::Boolean, Value::Boolean(b)) => bits.push(*b),
        (Shape::Number, Value::Number(n)) => bits.push(whole(*n).is_some()),
        (Shape::Record(fields), Value::Record(values)) => {
            for ((_, field), value) in fields.iter().zip(values) {
                flags(field, value, bits);
            }
        }
        _ => {}
    }
}

fn values(shape: &Shape, value: &Value, out: &mut Vec<u8>) {
    match (shape, value) {
        (Shape::Optional(inner), present) if *present != Value::Absent => values(inner, present, out),
        (Shape::Number, Value::Number(n)) => match whole(*n) {
            Some(n) => varint(out, zigzag(n)),
            None => out.extend_from_slice(&n.to_le_bytes()),
        },
        (Shape::String, Value::String(s)) => {
            varint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        (Shape::Color | Shape::Vector, Value::Vector(v)) => v.iter().for_each(|f| out.extend_from_slice(&f.to_le_bytes())),
        (Shape::List(item), Value::List(items)) => {
            varint(out, items.len() as u64);
            for item_value in items {
                out.extend(encode(item, item_value));
            }
        }
        (Shape::Record(fields), Value::Record(field_values)) => {
            for ((_, field), value) in fields.iter().zip(field_values) {
                values(field, value, out);
            }
        }
        _ => {}
    }
}

/// `value`, which must already fit `shape` (see `glassdeck_luau`'s conversion), packed.
pub fn encode(shape: &Shape, value: &Value) -> Vec<u8> {
    let mut bits = Bits { bytes: Vec::new(), count: 0 };
    flags(shape, value, &mut bits);
    let mut out = bits.bytes;
    values(shape, value, &mut out);
    out
}

/// Why bytes don't read as a message: the other half has a different `bridge.luau`, or they're damaged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Malformed> {
        let end = self.at.checked_add(n).filter(|end| *end <= self.bytes.len()).ok_or(Malformed("it ends early"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn varint(&mut self) -> Result<u64, Malformed> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.take(1)?[0];
            n |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(Malformed("a number runs on too long"))
    }

    fn f32(&mut self) -> Result<f32, Malformed> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }
}

/// How many flag bits a value's top level has, so its flag bytes can be skipped to reach its values.
fn flag_count(shape: &Shape) -> usize {
    match shape {
        Shape::Optional(inner) => 1 + flag_count(inner),
        Shape::Boolean | Shape::Number => 1,
        Shape::Record(fields) => fields.iter().map(|(_, f)| flag_count(f)).sum(),
        _ => 0,
    }
}

fn read(shape: &Shape, flags: &[u8], next_flag: &mut usize, r: &mut Reader) -> Result<Value, Malformed> {
    let mut flag = || {
        let i = *next_flag;
        *next_flag += 1;
        flags[i / 8] & (1 << (i % 8)) != 0
    };
    Ok(match shape {
        Shape::Optional(inner) => {
            if flag() {
                read(inner, flags, next_flag, r)?
            } else {
                Value::Absent
            }
        }
        Shape::Boolean => Value::Boolean(flag()),
        Shape::Number => {
            if flag() {
                let n = r.varint()?;
                Value::Number(((n >> 1) as i64 ^ -((n & 1) as i64)) as f64)
            } else {
                Value::Number(f64::from_le_bytes(r.take(8)?.try_into().expect("eight bytes")))
            }
        }
        Shape::String => {
            let len = usize::try_from(r.varint()?).map_err(|_| Malformed("a string's length is too big"))?;
            Value::String(String::from_utf8(r.take(len)?.to_vec()).map_err(|_| Malformed("a string isn't UTF-8"))?)
        }
        Shape::Color | Shape::Vector => Value::Vector([r.f32()?, r.f32()?, r.f32()?, r.f32()?]),
        Shape::List(item) => {
            let count = r.varint()?;
            let mut items = Vec::new();
            for _ in 0..count {
                items.push(decode_from(item, r)?);
            }
            Value::List(items)
        }
        Shape::Record(fields) => Value::Record(fields.iter().map(|(_, f)| read(f, flags, next_flag, r)).collect::<Result<_, _>>()?),
    })
}

fn decode_from(shape: &Shape, r: &mut Reader) -> Result<Value, Malformed> {
    let flag_bytes = flag_count(shape).div_ceil(8);
    let flags = r.take(flag_bytes)?.to_vec();
    read(shape, &flags, &mut 0, r)
}

/// A message's bytes as its shape's values.
pub fn decode(shape: &Shape, bytes: &[u8]) -> Result<Value, Malformed> {
    let mut r = Reader { bytes, at: 0 };
    let value = decode_from(shape, &mut r)?;
    if r.at != bytes.len() {
        return Err(Malformed("there's more after it"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn shape_of(source: &str) -> Shape {
        parse(source).unwrap().0.remove(0).shape
    }

    #[test]
    fn values_come_back_as_they_went() {
        let shape = shape_of("export type m = KeyboardState<{ title: string, n: number, half: number, on: boolean, tint: Color?, gone: string?, list: { { x: number, ok: boolean } } }>");
        let value = Value::Record(vec![
            Value::String("Low Tide".into()),
            Value::Number(-42.0),
            Value::Number(0.5),
            Value::Boolean(true),
            Value::Vector([1.0, 0.5, 0.25, 1.0]),
            Value::Absent,
            Value::List(vec![Value::Record(vec![Value::Number(3.0), Value::Boolean(false)]), Value::Record(vec![Value::Number(1e300), Value::Boolean(true)])]),
        ]);
        assert_eq!(decode(&shape, &encode(&shape, &value)), Ok(value));
    }

    #[test]
    fn a_tap_is_three_bytes_or_fewer() {
        let shape = shape_of("export type tapped = HostEvent<{ key: number }>");
        assert_eq!(encode(&shape, &Value::Record(vec![Value::Number(3.0)])), vec![0b1, 6]);
    }

    #[test]
    fn damaged_or_mismatched_bytes_are_refused() {
        let shape = shape_of("export type m = KeyboardEvent<{ title: string }>");
        assert!(decode(&shape, &[0x05, b'a']).is_err());
        assert!(decode(&shape, &[0x01, b'a', 0xff]).is_err());
    }
}
