//! Small, bounded protobuf wire reader for Antigravity's persisted metadata.
//! Unknown fields are skipped; malformed framing is never treated as zero usage.
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
pub type Message<'a> = BTreeMap<u32, Vec<Value<'a>>>;

pub fn varint(bytes: &[u8], offset: &mut usize) -> Result<u64, String> {
    let mut result = 0;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*offset).ok_or("Truncated protobuf varint")?;
        *offset += 1;
        if shift == 63 && b > 1 {
            return Err("Protobuf varint overflow".into());
        }
        result |= u64::from(b & 127) << shift;
        if b < 128 {
            return Ok(result);
        }
    }
    Err("Protobuf varint overflow".into())
}

pub fn decode(bytes: &[u8]) -> Result<Message<'_>, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Protobuf message exceeds 16 MiB".into());
    }
    let mut fields = Message::new();
    let mut offset = 0;
    let mut count = 0;
    while offset < bytes.len() {
        count += 1;
        if count > 100_000 {
            return Err("Too many protobuf fields".into());
        }
        let tag = varint(bytes, &mut offset)?;
        let number = u32::try_from(tag >> 3).map_err(|_| "Invalid protobuf tag")?;
        if number == 0 || number > 0x1fff_ffff {
            return Err("Invalid protobuf tag".into());
        }
        let value = match tag & 7 {
            0 => Value::Number(varint(bytes, &mut offset)?),
            1 | 2 | 5 => {
                let size = match tag & 7 {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(varint(bytes, &mut offset)?)
                        .map_err(|_| "Invalid protobuf size")?,
                };
                let end = offset
                    .checked_add(size)
                    .filter(|end| *end <= bytes.len())
                    .ok_or("Truncated protobuf field")?;
                let value = Value::Bytes(&bytes[offset..end]);
                offset = end;
                value
            }
            _ => return Err("Unsupported protobuf wire type".into()),
        };
        fields.entry(number).or_default().push(value);
    }
    Ok(fields)
}

pub fn bytes<'a>(m: &Message<'a>, key: u32) -> Option<&'a [u8]> {
    match m.get(&key)?.first()? {
        Value::Bytes(v) => Some(v),
        _ => None,
    }
}
pub fn number(m: &Message<'_>, key: u32) -> Result<u64, String> {
    match m.get(&key).and_then(|v| v.first()) {
        None => Ok(0),
        Some(Value::Number(v)) => Ok(*v),
        _ => Err("Wrong protobuf scalar type".into()),
    }
}
pub fn text(m: &Message<'_>, key: u32) -> Option<String> {
    std::str::from_utf8(bytes(m, key)?)
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
pub fn packed(m: &Message<'_>, key: u32) -> Result<Vec<u64>, String> {
    let mut result = Vec::new();
    for value in m.get(&key).into_iter().flatten() {
        match value {
            Value::Number(n) => result.push(*n),
            Value::Bytes(b) => {
                let mut i = 0;
                while i < b.len() {
                    result.push(varint(b, &mut i)?);
                }
            }
        }
        if result.len() > 100_000 {
            return Err("Too many packed protobuf values".into());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncation_overflow_groups_and_zero_tags() {
        for b in [
            &[0u8][..],
            &[10, 4, 1],
            &[8, 255],
            &[11],
            &[8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2],
        ] {
            assert!(decode(b).is_err());
        }
        let m = decode(&[8, 7, 18, 2, 1, 3]).unwrap();
        assert_eq!(number(&m, 1).unwrap(), 7);
        assert_eq!(packed(&m, 2).unwrap(), vec![1, 3]);
        assert_eq!(number(&m, 5).unwrap(), 0);
        assert!(number(&m, 2).is_err());
    }
}
