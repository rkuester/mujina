//! Wire framing for BM13xx serial communication.
//!
//! [`FrameCodec`] converts between typed commands or responses for
//! BM13xx chips and the bytes on the serial bus, handling the
//! preamble and CRC. It implements [`tokio_util::codec`] traits so a
//! single framed serial port handles both directions.

use bytes::{Buf, BufMut, BytesMut};
use std::{fmt, io};
use tokio_util::codec::{Decoder, Encoder};

use super::command::{JobCommand, RegisterCommand};
use super::register::ChipModel;
use super::response::Response;
use crate::asic::bm13xx::crc::{crc5, crc5_is_valid, crc16};
use crate::tracing::prelude::*;

pub struct FrameCodec {
    model: ChipModel,
}

impl FrameCodec {
    pub fn new(model: ChipModel) -> Self {
        Self { model }
    }
}

impl Encoder<RegisterCommand> for FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, command: RegisterCommand, dst: &mut BytesMut) -> Result<(), Self::Error> {
        const PREAMBLE: [u8; 2] = [0x55, 0xaa];
        dst.put_slice(&PREAMBLE);

        let start_pos = dst.len();
        command.encode(dst);
        let crc = crc5(&dst[start_pos..]);
        dst.put_u8(crc);

        trace!(
            cmd = ?command,
            bytes = dst.len(),
            frame = %HexBytes(dst.as_ref()),
            "TX BM13xx"
        );

        Ok(())
    }
}

impl Encoder<JobCommand> for FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, command: JobCommand, dst: &mut BytesMut) -> Result<(), Self::Error> {
        const PREAMBLE: [u8; 2] = [0x55, 0xaa];
        dst.put_slice(&PREAMBLE);

        let start_pos = dst.len();
        command.encode(dst);
        let crc = crc16(&dst[start_pos..]);
        dst.put_slice(&crc.to_be_bytes());

        trace!(
            cmd = ?command,
            bytes = dst.len(),
            frame = %HexBytes(dst.as_ref()),
            "TX BM13xx"
        );

        Ok(())
    }
}

impl Decoder for FrameCodec {
    type Item = Response;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Return Ok(Some) with a decoded frame, or Ok(None) when the
        // buffer holds less than a frame. Never return an error, which
        // would end the stream.
        //
        // Bytes that fail the CRC5 are junk: skip one and scan again.
        // A frame that passes the CRC5 but does not decode is one the
        // chip sent and this codec does not understand: skip it whole.
        // Both continue within this call. The framed reader answers
        // Ok(None) by waiting for more bytes, so returning early would
        // leave a frame already in the buffer unread until the next
        // bytes arrived.

        const PREAMBLE: [u8; 2] = [0xaa, 0x55];
        // All BM13xx responses are 11 bytes (2 preamble + 9 data)
        const FRAME_LEN: usize = PREAMBLE.len() + 9;

        loop {
            if src.len() < FRAME_LEN {
                return Ok(None);
            }

            if src[..PREAMBLE.len()] != PREAMBLE {
                src.advance(1);
                continue;
            }

            // CRC5 covers the 9 data bytes after the preamble
            if !crc5_is_valid(&src[PREAMBLE.len()..FRAME_LEN]) {
                trace!(
                    "Frame sync lost: CRC5 failed at the preamble. Searching for the next frame"
                );
                src.advance(1);
                continue;
            }

            let frame_bytes = src[..FRAME_LEN].to_vec();
            let mut decode_buf = BytesMut::from(&src[PREAMBLE.len()..FRAME_LEN]);
            let decoded = Response::decode(&mut decode_buf, self.model);
            src.advance(FRAME_LEN);

            match decoded {
                Ok(response) => {
                    trace!(
                        resp = ?response,
                        bytes = FRAME_LEN,
                        frame = %HexBytes(&frame_bytes),
                        "RX BM13xx"
                    );
                    return Ok(Some(response));
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        frame = %HexBytes(&frame_bytes),
                        "Dropping a response the codec cannot decode"
                    );
                }
            }
        }
    }
}

/// Wrapper for formatting byte slices as space-separated hex.
struct HexBytes<'a>(&'a [u8]);

impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}
