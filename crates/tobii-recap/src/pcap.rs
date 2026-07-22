//! Hand-rolled pcap container reader (classic `.pcap`, not pcapng).
//!
//! Global header (24 bytes): magic(u32), version_major(u16), version_minor(u16),
//! thiszone(i32), sigfigs(u32), snaplen(u32), linktype(u32). The magic's byte
//! order selects endianness and microsecond-vs-nanosecond timestamps.
//!
//! Record header (16 bytes): ts_sec(u32), ts_frac(u32), incl_len(u32),
//! orig_len(u32), followed by `incl_len` bytes of packet data.

use crate::error::RecapError;

pub const GLOBAL_HEADER_LEN: usize = 24;
pub const RECORD_HEADER_LEN: usize = 16;

/// Parsed pcap global header plus the byte-order / timestamp resolution the
/// magic selected.
#[derive(Debug, Clone, Copy)]
pub struct PcapGlobal {
    pub big_endian: bool,
    /// True when timestamps are nanoseconds (magic a1b23c4d), else microseconds.
    pub nanosecond: bool,
    pub snaplen: u32,
    pub linktype: u32,
}

/// One captured record: its timestamp and its raw packet bytes (the usbmon
/// pseudo-header + payload).
#[derive(Debug, Clone)]
pub struct Record<'a> {
    pub ts_sec: u32,
    pub ts_frac: u32,
    pub data: &'a [u8],
}

impl Record<'_> {
    /// Timestamp as seconds, honouring micro/nanosecond resolution.
    pub fn ts_seconds(&self, nanosecond: bool) -> f64 {
        let div = if nanosecond { 1e9 } else { 1e6 };
        self.ts_sec as f64 + self.ts_frac as f64 / div
    }
}

fn u16_at(b: &[u8], at: usize, big: bool) -> u16 {
    let a = [b[at], b[at + 1]];
    if big {
        u16::from_be_bytes(a)
    } else {
        u16::from_le_bytes(a)
    }
}

pub(crate) fn u32_at(b: &[u8], at: usize, big: bool) -> u32 {
    let a = [b[at], b[at + 1], b[at + 2], b[at + 3]];
    if big {
        u32::from_be_bytes(a)
    } else {
        u32::from_le_bytes(a)
    }
}

impl PcapGlobal {
    /// Parse the 24-byte global header from the front of `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<PcapGlobal, RecapError> {
        if bytes.len() < GLOBAL_HEADER_LEN {
            return Err(RecapError::ShortGlobalHeader);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let (big_endian, nanosecond) = match magic {
            [0xa1, 0xb2, 0xc3, 0xd4] => (true, false),
            [0xd4, 0xc3, 0xb2, 0xa1] => (false, false),
            [0xa1, 0xb2, 0x3c, 0x4d] => (true, true),
            [0x4d, 0x3c, 0xb2, 0xa1] => (false, true),
            _ => return Err(RecapError::BadMagic(magic)),
        };
        let _version_major = u16_at(bytes, 4, big_endian);
        let snaplen = u32_at(bytes, 16, big_endian);
        let linktype = u32_at(bytes, 20, big_endian);
        Ok(PcapGlobal {
            big_endian,
            nanosecond,
            snaplen,
            linktype,
        })
    }
}

/// Iterator over pcap records. Stops at end-of-data; a record header or body
/// that runs past the end yields a final [`Err`] (reported, never a panic).
pub struct RecordReader<'a> {
    global: PcapGlobal,
    rest: &'a [u8],
    done: bool,
}

impl<'a> RecordReader<'a> {
    /// Split a full pcap file into (global header, record iterator).
    pub fn new(bytes: &'a [u8]) -> Result<(PcapGlobal, RecordReader<'a>), RecapError> {
        let global = PcapGlobal::parse(bytes)?;
        Ok((
            global,
            RecordReader {
                global,
                rest: &bytes[GLOBAL_HEADER_LEN..],
                done: false,
            },
        ))
    }
}

impl<'a> Iterator for RecordReader<'a> {
    type Item = Result<Record<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.rest.is_empty() {
            return None;
        }
        if self.rest.len() < RECORD_HEADER_LEN {
            self.done = true;
            return Some(Err(format!(
                "truncated record header: {} bytes left, need {RECORD_HEADER_LEN}",
                self.rest.len()
            )));
        }
        let big = self.global.big_endian;
        let ts_sec = u32_at(self.rest, 0, big);
        let ts_frac = u32_at(self.rest, 4, big);
        let incl_len = u32_at(self.rest, 8, big) as usize;
        let body_start = RECORD_HEADER_LEN;
        let body_end = body_start.checked_add(incl_len);
        match body_end {
            Some(end) if end <= self.rest.len() => {
                let data = &self.rest[body_start..end];
                self.rest = &self.rest[end..];
                Some(Ok(Record {
                    ts_sec,
                    ts_frac,
                    data,
                }))
            }
            _ => {
                self.done = true;
                Some(Err(format!(
                    "truncated record body: incl_len={incl_len} but only {} bytes remain",
                    self.rest.len() - body_start
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a global header with the given magic bytes and linktype.
    fn global(magic: [u8; 4], big: bool, linktype: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&magic);
        let put16 = |v: &mut Vec<u8>, x: u16| {
            if big {
                v.extend_from_slice(&x.to_be_bytes())
            } else {
                v.extend_from_slice(&x.to_le_bytes())
            }
        };
        let put32 = |v: &mut Vec<u8>, x: u32| {
            if big {
                v.extend_from_slice(&x.to_be_bytes())
            } else {
                v.extend_from_slice(&x.to_le_bytes())
            }
        };
        put16(&mut v, 2); // version_major
        put16(&mut v, 4); // version_minor
        put32(&mut v, 0); // thiszone
        put32(&mut v, 0); // sigfigs
        put32(&mut v, 65535); // snaplen
        put32(&mut v, linktype);
        v
    }

    fn record(big: bool, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let put32 = |v: &mut Vec<u8>, x: u32| {
            if big {
                v.extend_from_slice(&x.to_be_bytes())
            } else {
                v.extend_from_slice(&x.to_le_bytes())
            }
        };
        put32(&mut v, 1); // ts_sec
        put32(&mut v, 2); // ts_frac
        put32(&mut v, payload.len() as u32); // incl_len
        put32(&mut v, payload.len() as u32); // orig_len
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn detects_little_endian_magic() {
        let g = PcapGlobal::parse(&global([0xd4, 0xc3, 0xb2, 0xa1], false, 220)).unwrap();
        assert!(!g.big_endian);
        assert!(!g.nanosecond);
        assert_eq!(g.linktype, 220);
    }

    #[test]
    fn detects_big_endian_magic() {
        let g = PcapGlobal::parse(&global([0xa1, 0xb2, 0xc3, 0xd4], true, 189)).unwrap();
        assert!(g.big_endian);
        assert_eq!(g.linktype, 189);
    }

    #[test]
    fn detects_nanosecond_magic() {
        let g = PcapGlobal::parse(&global([0x4d, 0x3c, 0xb2, 0xa1], false, 220)).unwrap();
        assert!(g.nanosecond);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(matches!(
            PcapGlobal::parse(&[
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23
            ]),
            Err(RecapError::BadMagic(_))
        ));
    }

    #[test]
    fn iterates_two_records() {
        let mut file = global([0xd4, 0xc3, 0xb2, 0xa1], false, 220);
        file.extend(record(false, &[0xAA, 0xBB]));
        file.extend(record(false, &[0xCC]));
        let (_g, reader) = RecordReader::new(&file).unwrap();
        let recs: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].data, &[0xAA, 0xBB]);
        assert_eq!(recs[1].data, &[0xCC]);
    }

    #[test]
    fn truncated_record_body_is_reported_not_panicked() {
        let mut file = global([0xd4, 0xc3, 0xb2, 0xa1], false, 220);
        // A record header claiming 100 bytes but with only 3 following.
        let mut hdr = Vec::new();
        hdr.extend_from_slice(&1u32.to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        hdr.extend_from_slice(&100u32.to_le_bytes());
        hdr.extend_from_slice(&100u32.to_le_bytes());
        hdr.extend_from_slice(&[0x01, 0x02, 0x03]);
        file.extend(hdr);
        let (_g, reader) = RecordReader::new(&file).unwrap();
        let results: Vec<_> = reader.collect();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }
}
