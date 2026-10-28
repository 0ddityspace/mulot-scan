//! Decoding of the raw MJPEG frames produced by the scanner mouse.
//!
//! The mouse firmware hides its sensor data *inside* the JPEG headers:
//!
//! * the height/width fields of the SOF0 marker carry a frame counter and
//!   three bytes of "high bits";
//! * the last 15 coefficients of the second quantisation table (DQT) carry
//!   three bytes of "extra bits" and twelve payload bytes.
//!
//! Before the JPEG can be decoded those fields must be restored to valid
//! values (640x296 and a quantisation value of 0x19).  This mirrors
//! `FUN_10002fe0` in the original `DXCore1.dll`.

/// Real image geometry delivered by the camera in scan mode.
pub const FRAME_WIDTH: u32 = 640;
pub const FRAME_HEIGHT: u32 = 296;

/// Maximum number of bytes scanned for markers (same limit as the original driver).
const MARKER_SCAN_LIMIT: usize = 40_000;

/// The 32-byte side-channel record, laid out exactly like the buffer that the
/// original driver appended after the decoded RGB image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawMouseRecord(pub [u8; 32]);

/// Decoded view of the side-channel record (same field split as
/// `DaReadMouseData` in `DLL_OvtApi.dll`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MouseData {
    /// Frame counter incremented by the firmware for every frame (wraps at 256).
    pub counter: u8,
    /// Six big-endian 16-bit words (`DaReadMouseData` outputs 1..=6).
    pub words: [u16; 6],
    /// Surface quality of the two optical sensors.
    pub surface_quality: [u8; 2],
    /// Additional byte (`DaReadMouseData` output 9).
    pub extra: u8,
    /// Button / state flags (bits 0..=4 of byte 17).
    pub flags: u8,
    /// Set when the firmware counter skipped at least one frame.
    pub frame_dropped: bool,
}

impl MouseData {
    pub fn from_record(rec: &RawMouseRecord, frame_dropped: bool) -> Self {
        let p = &rec.0;
        let be = |o: usize| u16::from_be_bytes([p[o], p[o + 1]]);
        MouseData {
            counter: p[0],
            words: [be(2), be(4), be(6), be(8), be(10), be(12)],
            surface_quality: [p[14], p[15]],
            extra: p[16],
            flags: p[17] & 0x1f,
            frame_dropped,
        }
    }
}

/// Reverses the byte-stuffing used by the firmware (port of `FUN_10002f50`).
fn unstuff(v: u8, bit: u8, hi: u8) -> u8 {
    let v = v as i32;
    let b = ((bit & 1) as i32) * 4;
    let r = match hi & 3 {
        2 => b | (((v - 1).max(0) * 4 + 2) & 0xfb),
        3 => b | (((v - 1).max(0) * 4 + 3) & 0xfb),
        h => {
            let u = v * 4 + if h == 1 { 1 } else { 0 };
            b | (u & 0xfb)
        }
    };
    r as u8
}

/// Result of parsing one raw frame.
#[derive(Debug, Clone)]
pub struct ParsedFrame {
    /// A valid, decodable baseline JPEG (headers restored).
    pub jpeg: Vec<u8>,
}

/// Stateful parser (keeps the previous counter to detect dropped frames).
#[derive(Debug, Default)]
pub struct FrameParser {
    last_counter: Option<u32>,
}

impl FrameParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses and patches a raw MJPEG frame. Returns `None` when the frame
    /// does not contain the expected markers (e.g. a truncated frame or a
    /// frame produced while the mouse is not in scan mode).
    pub fn parse(&mut self, raw: &[u8]) -> Option<(ParsedFrame, MouseData)> {
        let mut jpeg = raw.to_vec();
        let mut p = [0u8; 32];
        let mut state = 0u8;
        let mut dqt_seen = 0;
        let limit = jpeg.len().min(MARKER_SCAN_LIMIT);
        let mut i = 0;
        while i + 1 < limit {
            if state == 7 {
                break;
            }
            if jpeg[i] == 0xff {
                match jpeg[i + 1] {
                    0xd8 => state |= 1,
                    0xc0 if i + 9 <= jpeg.len() => {
                        p[0] = jpeg[i + 5];
                        p[0x14] = jpeg[i + 6];
                        p[0x15] = jpeg[i + 7];
                        p[0x16] = jpeg[i + 8];
                        jpeg[i + 5..i + 9].copy_from_slice(&[0x01, 0x28, 0x02, 0x80]);
                        state |= 2;
                    }
                    0xdb => {
                        dqt_seen += 1;
                        if dqt_seen == 2 && i + 0x45 <= jpeg.len() {
                            p[0x18] = jpeg[i + 0x36];
                            p[0x19] = jpeg[i + 0x37];
                            p[0x1a] = jpeg[i + 0x38];
                            const DST: [usize; 12] = [2, 3, 4, 5, 8, 9, 10, 11, 14, 15, 16, 17];
                            for (j, &o) in DST.iter().enumerate() {
                                p[o] = jpeg[i + 0x39 + j];
                            }
                            for b in &mut jpeg[i + 0x36..i + 0x45] {
                                *b = 0x19;
                            }
                            state |= 4;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        if state != 7 {
            return None;
        }

        // Counter continuity (port of the DAT_100087e4 logic).
        let mut counter = p[0] as u32;
        let dropped = match self.last_counter {
            Some(last) => {
                if counter < last {
                    counter += 0x100;
                }
                last + 1 != counter
            }
            None => false,
        };
        self.last_counter = Some(counter & 0xff);
        p[0x13] = dropped as u8;

        let (b14, b15, b16) = (p[0x14], p[0x15], p[0x16]);
        let (b18, b19, b1a) = (p[0x18], p[0x19], p[0x1a]);
        let spec: [(usize, u8, u8); 12] = [
            (2, b18 >> 5, b14 >> 6),
            (3, b18 >> 4, b14 >> 4),
            (4, b18 >> 3, b14 >> 2),
            (5, b18 >> 2, b14),
            (8, b18 >> 1, b15 >> 6),
            (9, b19 >> 5, b15 >> 4),
            (10, b19 >> 4, b15 >> 2),
            (11, b19 >> 3, b15),
            (14, b19 >> 2, b16 >> 6),
            (15, b19 >> 1, b16 >> 4),
            (16, b1a >> 5, b16 >> 2),
            (17, b1a >> 4, b16),
        ];
        for (o, bit, hi) in spec {
            p[o] = unstuff(p[o], bit, hi);
        }

        let record = RawMouseRecord(p);
        let data = MouseData::from_record(&record, dropped);
        Some((ParsedFrame { jpeg }, data))
    }
}

/// Splits a byte stream containing several concatenated JPEG frames
/// (as written by `v4l2-ctl --stream-to`) into individual frames.
pub fn split_mjpeg_stream(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0xff && data[i + 1] == 0xd8 && data[i + 2] == 0xff {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstuff_matches_reference() {
        // Values computed with the reference implementation of FUN_10002f50.
        assert_eq!(unstuff(0, 0, 0), 0);
        // bit 2 of the result always comes from `bit`
        assert_eq!(unstuff(1, 0, 0), 0);
        assert_eq!(unstuff(1, 1, 0), 4);
        assert_eq!(unstuff(2, 0, 0), 8);
        assert_eq!(unstuff(0, 0, 1), 1);
        assert_eq!(unstuff(3, 0, 2), 0x0a);
        assert_eq!(unstuff(3, 1, 3), 0x0f);
        assert_eq!(unstuff(0, 0, 2), 2);
    }
}
