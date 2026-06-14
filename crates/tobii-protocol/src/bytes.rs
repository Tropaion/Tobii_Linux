//! A tiny growable byte writer with explicit-endianness helpers.

/// Accumulates bytes for an outbound frame.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
        }
    }

    pub fn push_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn push_be32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn push_be64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn push_le32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn push_bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_big_and_little_endian() {
        let mut w = Writer::new();
        w.push_u8(0xAB);
        w.push_be32(0x0011_2233);
        w.push_le32(0x0011_2233);
        w.push_be64(0x0102_0304_0506_0708);
        w.push_bytes(&[0xEE, 0xFF]);
        assert_eq!(
            w.into_vec(),
            vec![
                0xAB, 0x00, 0x11, 0x22, 0x33, 0x33, 0x22, 0x11, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
                0x06, 0x07, 0x08, 0xEE, 0xFF,
            ]
        );
    }
}
