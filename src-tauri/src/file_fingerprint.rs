use std::{
    fs::File,
    hash::Hasher,
    io::{Read, Seek, SeekFrom},
};

/// Fingerprint processed bytes using bounded memory, independently of read chunk boundaries.
pub fn processed(file: &mut File, length: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let mut remaining = length;
    let mut buffer = [0u8; 32768];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let n = file.read(&mut buffer[..wanted])?;
        if n == 0 {
            break;
        }
        hash.write(&buffer[..n]);
        remaining -= n as u64;
    }
    let mut signature = b"CCW2".to_vec();
    signature.extend(hash.finish().to_le_bytes());
    Ok(signature)
}
