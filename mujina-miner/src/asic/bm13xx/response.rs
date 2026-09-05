//! Frames BM13xx chips send back to the host.
//!
//! BM13xx chips send back two kinds of frames: replies to register
//! reads, and nonce reports when a chip finds passing work.

use bitvec::prelude::*;
use bytes::{Buf, BytesMut};
use num_enum::TryFromPrimitive;

use super::register::{ChipModel, Register, RegisterAddress};
use crate::asic::bm13xx::error::ProtocolError;
use crate::job_source::GeneralPurposeBits;

#[derive(Debug)]
pub enum Response {
    ReadRegister(RegisterResponse),
    Nonce(NonceResponse),
}

/// Reply to a register read.
#[derive(Debug)]
pub struct RegisterResponse {
    pub chip_address: u8,
    pub register: Register,
}

/// Nonce report from a chip that found passing work.
#[derive(Debug)]
pub struct NonceResponse {
    pub nonce: u32,
    pub job_id: u8,
    /// Difficulty above the reporting threshold, in half bits on
    /// the log2 scale; reference code calls the byte Midstate_Num.
    pub excess_difficulty: u8,
    pub version: GeneralPurposeBits,
    pub subcore_id: u8,
}

impl NonceResponse {
    /// Returns the fields the chip packs into this nonce.
    pub fn decompose(&self, model: ChipModel) -> NonceFields {
        NonceFields::from_nonce(model, self.nonce)
    }
}

/// The fields the chip packs into a nonce.
///
/// The chip's byte order is most significant byte first, the
/// reverse of the block header's. [`NonceFields::from_nonce`]
/// takes the header-order nonce and reads it in the chip's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceFields {
    /// The BM1362 and BM1366 layout.
    ///
    /// - bits 31-25: core ID
    /// - bits 24-18: the chip's address shifted right one
    /// - bits 17-0: sweep counter
    ///
    /// The chip seeds the 25 bits below the core ID with its address
    /// in bits 24-17, so bits 24-18 name the chip only while addresses
    /// are even; the chain assigns them that way.
    BM1362 {
        core_id: u8,
        /// The chip's address, bits 24-18 shifted left one.
        chip_address: u8,
        counter: u32,
    },
    /// The BM1370 layout. No field names the chip; the nonce's
    /// value falls in the slice of the chip that produced it.
    ///
    /// - bits 31-25: core ID
    /// - bits 24-0: counter, seeded from CHIP_NONCE_OFFSET
    BM1370 { core_id: u8, counter: u32 },
}

impl NonceFields {
    /// Divides a header-order nonce into the chip's fields.
    pub fn from_nonce(model: ChipModel, nonce: u32) -> Self {
        let value = nonce.swap_bytes();
        let core_id = (value >> 25) as u8;
        match model {
            ChipModel::BM1362 | ChipModel::BM1366 => Self::BM1362 {
                core_id,
                chip_address: (value >> 18 & 0x7f) as u8 * 2,
                counter: value & 0x3ffff,
            },
            ChipModel::BM1370 => Self::BM1370 {
                core_id,
                counter: value & 0x1ff_ffff,
            },
        }
    }
}

impl Response {
    pub(super) fn decode(
        bytes: &mut BytesMut,
        model: ChipModel,
    ) -> Result<Response, ProtocolError> {
        let type_and_crc = bytes[bytes.len() - 1].view_bits::<Lsb0>();
        let type_repr = type_and_crc[5..].load::<u8>();

        match ResponseType::try_from(type_repr).ok() {
            Some(ResponseType::ReadRegister) => {
                let value_bytes = bytes.split_to(4);
                let value: [u8; 4] =
                    value_bytes[..]
                        .try_into()
                        .map_err(|_| ProtocolError::BufferTooSmall {
                            need: 4,
                            have: value_bytes.len(),
                        })?;
                let chip_address = bytes.get_u8();
                let register_address_repr = bytes.get_u8();

                if let Ok(register_address) = RegisterAddress::try_from(register_address_repr) {
                    let register = Register::decode(register_address, value)?;
                    Ok(Response::ReadRegister(RegisterResponse {
                        chip_address,
                        register,
                    }))
                } else {
                    Err(ProtocolError::InvalidRegisterAddress(register_address_repr))
                }
            }
            Some(ResponseType::Nonce) => {
                // Nonce response format (11 bytes total, including preamble):
                // Already consumed: preamble (2 bytes)
                // Remaining: nonce(4) + excess_difficulty(1) + result_header(1) + version(2) + crc(1)
                let nonce = bytes.get_u32_le();
                let excess_difficulty = bytes.get_u8();
                let result_header = bytes.get_u8();

                // Version rolling field: 2 bytes, big-endian
                // Occupies bits 28-13 of block version when shifted left 13
                let version_bytes = [bytes.get_u8(), bytes.get_u8()];
                let version = GeneralPurposeBits::from(version_bytes);
                // CRC already consumed

                // The result header packs the job id above the subcore
                // id, split per model: BM1362 and BM1366 use a 5-bit
                // job id with a 3-bit subcore id, BM1370 a 4-bit job
                // id with a 4-bit subcore id. Captures verify the
                // BM1362 and BM1370 splits. No capture covers the
                // BM1366; it gets the BM1362 split because a
                // reference driver decodes both identically. The
                // match is deliberately exhaustive: a new model does
                // not compile until its split is decided here.
                let (job_id, subcore_id) = match model {
                    ChipModel::BM1362 | ChipModel::BM1366 => {
                        (result_header >> 3, result_header & 0x07)
                    }
                    ChipModel::BM1370 => ((result_header >> 4) & 0x0f, result_header & 0x0f),
                };

                Ok(Response::Nonce(NonceResponse {
                    nonce,
                    job_id,
                    excess_difficulty,
                    version,
                    subcore_id,
                }))
            }
            None => Err(ProtocolError::InvalidResponseType(type_repr)),
        }
    }
}

#[derive(TryFromPrimitive)]
#[repr(u8)]
enum ResponseType {
    ReadRegister = 0,
    Nonce = 4,
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use tokio_util::codec::Decoder;

    use super::super::codec::FrameCodec;
    use super::super::register::{ChipId, ChipModel, CoreRegisterValue, Register};
    use super::*;
    use crate::asic::bm13xx::crc::crc5_is_valid;

    #[test]
    fn verify_crc_calculation() {
        // Test that our known good frame has valid CRC
        let frame = &[0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10]; // without preamble
        assert!(
            crc5_is_valid(frame),
            "Known good frame should have valid CRC"
        );
    }

    #[test]
    fn decoder_with_exact_frame_size() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Exactly 11 bytes - a complete frame
        let mut buf = BytesMut::new();
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ]);

        let result = codec.decode(&mut buf).unwrap();
        assert!(
            result.is_some(),
            "Should decode frame when buffer has exactly 11 bytes"
        );
    }

    #[test]
    fn read_register() {
        // 11-byte register read response from captures
        let wire = &[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ];
        let response = decode_frame(wire, ChipModel::BM1370)
            .expect("decode_frame should return Some for valid frame");

        let Response::ReadRegister(RegisterResponse {
            chip_address,
            register,
        }) = response
        else {
            panic!("Expected ReadRegister response, got {:?}", response);
        };

        assert_eq!(chip_address, 0x00);

        let Register::ChipId(ChipId {
            model,
            unknown,
            address,
        }) = register
        else {
            panic!("Expected ChipId register, got {:?}", register);
        };

        assert_eq!(model, ChipModel::BM1370);
        assert_eq!(unknown, 0x00);
        assert_eq!(address, 0x00);
    }

    fn decode_frame(frame: &[u8], model: ChipModel) -> Option<Response> {
        let mut buf = BytesMut::from(frame);
        let mut codec = FrameCodec::new(model);
        codec.decode(&mut buf).expect("Failed to decode frame")
    }

    /// A chip id reply from the Bitaxe capture, with its preamble.
    const CHIP_ID_FRAME: [u8; 11] = [
        0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    ];

    /// Fills in the CRC5 of a response body whose last byte holds
    /// the response type in its top three bits.
    fn with_crc5(mut body: [u8; 9]) -> [u8; 9] {
        let type_bits = body[8] & 0xe0;
        body[8] = (0..32)
            .map(|crc| type_bits | crc)
            .find(|&last| {
                body[8] = last;
                crc5_is_valid(&body)
            })
            .expect("some five-bit value satisfies the CRC");
        body
    }

    #[test]
    fn decode_core_register_value_report() {
        // An EmberOne00 chip at address 0x08 sends this unasked
        let wire = &[
            0xaa, 0x55, 0x00, 0x00, 0x00, 0x00, 0x08, 0x40, 0x00, 0x00, 0x0e,
        ];
        let response = decode_frame(wire, ChipModel::BM1362).expect("a valid frame");

        let Response::ReadRegister(RegisterResponse {
            chip_address,
            register,
        }) = response
        else {
            panic!("expected a register response, got {response:?}");
        };
        assert_eq!(chip_address, 0x08);
        assert_eq!(
            register,
            Register::CoreRegisterValue(CoreRegisterValue([0, 0, 0, 0]))
        );
    }

    #[test]
    fn decoder_drops_an_undecodable_frame_whole() {
        // A CRC-valid chip id reply naming no known model, ahead of
        // a good reply in the same buffer
        let unknown_chip = with_crc5([0x12, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut buf = BytesMut::from(&[0xaa, 0x55][..]);
        buf.put_slice(&unknown_chip);
        buf.put_slice(&CHIP_ID_FRAME);
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        let response = codec.decode(&mut buf).unwrap();

        assert!(matches!(response, Some(Response::ReadRegister(_))));
        assert!(buf.is_empty(), "both frames are consumed");
    }

    #[test]
    fn reject_register_response_with_unknown_chip_id() {
        // A ChipId register read response whose id bytes match no
        // supported model; body only, preamble stripped as
        // Response::decode expects
        let mut buf = BytesMut::from(&[0x12, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00][..]);

        let result = Response::decode(&mut buf, ChipModel::BM1370);

        assert!(matches!(
            result,
            Err(ProtocolError::UnknownChipId([0x12, 0x34]))
        ));
    }

    #[test]
    fn decode_nonce_response_from_capture() {
        // From Bitaxe capture: RX: AA 55 18 00 A6 40 02 99 22 F9 91
        let wire = &[
            0xaa, 0x55, 0x18, 0x00, 0xa6, 0x40, 0x02, 0x99, 0x22, 0xf9, 0x91,
        ];
        let response = decode_frame(wire, ChipModel::BM1370)
            .expect("decode_frame should return Some for valid frame");

        let Response::Nonce(NonceResponse {
            nonce,
            job_id,
            excess_difficulty,
            version,
            subcore_id,
        }) = response
        else {
            panic!("Expected nonce response");
        };

        // From the chip reference: wire bytes 18 00 A6 40 are the chip's
        // value 0x1800A640; the decoder holds the header-order word
        assert_eq!(nonce, 0x40a60018);
        assert_eq!(excess_difficulty, 0x02);

        // Result header: 0x99 -> bits[7:4]=9 (job_id), bits[3:0]=9 (subcore_id)
        assert_eq!(job_id, 9);
        assert_eq!(subcore_id, 9);

        // Version
        assert_eq!(version, GeneralPurposeBits::new([0x22, 0xF9]));

        // The chip's fields, per the reference's worked example
        assert_eq!(
            NonceFields::from_nonce(ChipModel::BM1370, nonce),
            NonceFields::BM1370 {
                core_id: 12,
                counter: 0xA640,
            }
        );
    }

    #[test]
    fn nonce_fields_bm1362_worked_example() {
        // From the chip reference's BM1362 example: wire bytes
        // 6D B8 8E E1 deserialize to the header nonce 0xE18EB86D;
        // the chip's value 0x6DB88EE1 names core 54 on chip 0xDC.
        assert_eq!(
            NonceFields::from_nonce(ChipModel::BM1362, 0xE18EB86D),
            NonceFields::BM1362 {
                core_id: 54,
                chip_address: 0xDC,
                counter: 0x08EE1,
            }
        );
    }

    #[test]
    fn nonce_response_decompose_uses_model() {
        // The chip reference's BM1370 example response, as the
        // decoder returns it
        let response = NonceResponse {
            nonce: 0x40A60018,
            job_id: 9,
            excess_difficulty: 2,
            version: GeneralPurposeBits::new([0x22, 0xF9]),
            subcore_id: 9,
        };
        assert_eq!(
            response.decompose(ChipModel::BM1370),
            NonceFields::BM1370 {
                core_id: 12,
                counter: 0xA640,
            }
        );
    }

    #[test]
    fn decode_multiple_nonce_responses() {
        // Additional nonce responses from S21 Pro capture
        let test_cases = vec![
            // RX: AA 55 07 35 CD CF 02 5E 00 2E 96
            // result_header=0x5e: bits[7:4]=5, bits[3:0]=14
            // version bytes [0x00, 0x2E] big-endian = 0x002E
            (
                &[
                    0xaa, 0x55, 0x07, 0x35, 0xcd, 0xcf, 0x02, 0x5e, 0x00, 0x2e, 0x96,
                ],
                0xcfcd3507,
                0x02,
                5,
                14,
                GeneralPurposeBits::new([0x00, 0x2E]),
            ),
            // RX: AA 55 46 03 32 E7 00 C3 2C 83 99
            // result_header=0xc3: bits[7:4]=12, bits[3:0]=3
            // version bytes [0x2C, 0x83] big-endian = 0x2C83
            (
                &[
                    0xaa, 0x55, 0x46, 0x03, 0x32, 0xe7, 0x00, 0xc3, 0x2c, 0x83, 0x99,
                ],
                0xe7320346,
                0x00,
                12,
                3,
                GeneralPurposeBits::new([0x2C, 0x83]),
            ),
        ];

        for (wire, exp_nonce, exp_excess, exp_job_id, exp_subcore, exp_version) in test_cases {
            let response = decode_frame(wire, ChipModel::BM1370)
                .expect("decode_frame should return Some for valid frame");

            let Response::Nonce(NonceResponse {
                nonce,
                job_id,
                excess_difficulty,
                version,
                subcore_id,
            }) = response
            else {
                panic!("Expected nonce response");
            };

            assert_eq!(nonce, exp_nonce);
            assert_eq!(excess_difficulty, exp_excess);
            assert_eq!(job_id, exp_job_id);
            assert_eq!(subcore_id, exp_subcore);
            assert_eq!(version, exp_version);
        }
    }

    #[test]
    fn decode_nonce_response_from_s19jpro_capture() {
        // From S19 J Pro capture: RX: AA 55 81 C9 77 D0 00 F2 7C 3E 89
        let wire = &[
            0xaa, 0x55, 0x81, 0xc9, 0x77, 0xd0, 0x00, 0xf2, 0x7c, 0x3e, 0x89,
        ];
        let response = decode_frame(wire, ChipModel::BM1362)
            .expect("decode_frame should return Some for valid frame");

        let Response::Nonce(NonceResponse {
            nonce,
            job_id,
            excess_difficulty,
            version,
            subcore_id,
        }) = response
        else {
            panic!("Expected nonce response");
        };

        assert_eq!(nonce, 0xd077c981);
        assert_eq!(excess_difficulty, 0x00);

        // Result header 0xf2: job_id 30 needs the 5-bit split; the
        // BM1370 packing cannot represent it
        assert_eq!(job_id, 30);
        assert_eq!(subcore_id, 2);

        assert_eq!(version, GeneralPurposeBits::new([0x7c, 0x3e]));
    }

    #[test]
    fn decoder_handles_partial_frames() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Test with incomplete frame (less than 11 bytes)
        let mut buf = BytesMut::new();
        buf.put_slice(&[0xaa, 0x55, 0x13, 0x70, 0x00]); // Only 5 bytes

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none(), "Should return None for incomplete frame");
        assert_eq!(buf.len(), 5, "Buffer should not be consumed");

        // Add more bytes to complete the frame
        buf.put_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x10]); // Complete to 11 bytes

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_some(), "Should decode complete frame");
        assert_eq!(buf.len(), 0, "Buffer should be fully consumed");
    }

    #[test]
    fn decoder_handles_corrupted_crc() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Valid frame with corrupted CRC (last byte)
        let mut buf = BytesMut::new();
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF,
        ]); // Bad CRC

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none(), "Should reject frame with bad CRC");
        assert_eq!(
            buf.len(),
            10,
            "Should consume 1 byte when searching for valid frame"
        );
    }

    #[test]
    fn decoder_finds_frame_after_garbage() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Garbage bytes followed by valid frame
        let mut buf = BytesMut::new();
        buf.put_slice(&[0xFF, 0xEE, 0xDD]); // Garbage
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ]); // Valid frame

        // One call skips the garbage and returns the frame behind
        // it; returning None would make the reader wait for bytes
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_some(), "Should find valid frame after garbage");
        assert_eq!(buf.len(), 0, "All data should be consumed");
    }

    #[test]
    fn decoder_handles_false_start() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Frame that starts with 0xAA but not followed by 0x55
        let mut buf = BytesMut::new();
        buf.put_slice(&[0xaa, 0x00]); // False start
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ]); // Valid frame

        // One call skips the false start and the byte after it
        // and returns the frame
        let result = codec.decode(&mut buf).unwrap();
        assert!(matches!(result, Some(Response::ReadRegister(_))));
        assert_eq!(buf.len(), 0, "All data should be consumed");
    }

    #[test]
    fn decoder_handles_back_to_back_frames() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Two valid frames back-to-back
        let mut buf = BytesMut::new();
        // First frame: register read
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ]);
        // Second frame: nonce response
        buf.put_slice(&[
            0xaa, 0x55, 0x18, 0x00, 0xa6, 0x40, 0x02, 0x99, 0x22, 0xf9, 0x91,
        ]);

        // Decode first frame
        let result1 = codec.decode(&mut buf).unwrap();
        assert!(matches!(result1, Some(Response::ReadRegister(_))));
        assert_eq!(buf.len(), 11, "Should have second frame remaining");

        // Decode second frame
        let result2 = codec.decode(&mut buf).unwrap();
        assert!(matches!(result2, Some(Response::Nonce(_))));
        assert_eq!(buf.len(), 0, "Buffer should be empty");
    }

    #[test]
    fn decoder_handles_real_s21_pro_frames() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Real frames from S21 Pro capture
        let frames = vec![
            [
                0xaa, 0x55, 0x07, 0x35, 0xcd, 0xcf, 0x02, 0x5e, 0x00, 0x2e, 0x96,
            ],
            [
                0xaa, 0x55, 0x7b, 0x8d, 0x81, 0x60, 0x02, 0x55, 0x00, 0x85, 0x81,
            ],
            [
                0xaa, 0x55, 0x32, 0x2a, 0x84, 0x5a, 0x02, 0x52, 0x01, 0xb2, 0x8c,
            ],
        ];

        for frame in frames {
            let mut buf = BytesMut::new();
            buf.put_slice(&frame);

            let result = codec.decode(&mut buf).unwrap();
            assert!(result.is_some(), "Should decode real S21 Pro frame");
            assert!(
                matches!(result, Some(Response::Nonce(_))),
                "Should be nonce response"
            );
        }
    }

    #[test]
    fn decoder_handles_stream_with_lost_bytes() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Simulate a stream where some bytes in the middle are lost
        let mut buf = BytesMut::new();
        // Start of first frame
        buf.put_slice(&[0xaa, 0x55, 0x13, 0x70, 0x00]); // 5 bytes
        // Lost bytes... skip to middle of nowhere
        buf.put_slice(&[0x99, 0x22, 0xf9]); // Random bytes
        // Valid complete frame
        buf.put_slice(&[
            0xaa, 0x55, 0x18, 0x00, 0xa6, 0x40, 0x02, 0x99, 0x22, 0xf9, 0x91,
        ]);

        // Decoder should skip the incomplete/corrupted data and find the valid frame
        let mut found_valid = false;
        for _ in 0..20 {
            // Try up to 20 times
            if let Some(response) = codec.decode(&mut buf).unwrap() {
                assert!(matches!(response, Response::Nonce(_)));
                found_valid = true;
                break;
            }
        }
        assert!(found_valid, "Should eventually find the valid frame");
    }

    #[test]
    fn decoder_handles_mid_frame_start() {
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Start reading in the middle of a frame
        let mut buf = BytesMut::new();
        // Last 5 bytes of some frame
        buf.put_slice(&[0x02, 0x99, 0x22, 0xf9, 0x91]);
        // Valid complete frame
        buf.put_slice(&[
            0xaa, 0x55, 0x50, 0x03, 0x41, 0xd6, 0x00, 0x81, 0x18, 0x01, 0x9b,
        ]);

        // One call skips the partial frame and returns the whole one
        let result = codec.decode(&mut buf).unwrap();
        assert_eq!(buf.len(), 0, "All data should be consumed");
        assert!(
            result.is_some(),
            "Should find valid frame after partial data"
        );
        assert!(
            matches!(result, Some(Response::Nonce(_))),
            "Should be nonce response"
        );
    }

    #[test]
    fn decoder_validates_real_register_responses() {
        // Test all register read responses are handled correctly
        let mut codec = FrameCodec::new(ChipModel::BM1370);

        // Standard chip detection response
        let mut buf = BytesMut::new();
        buf.put_slice(&[
            0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
        ]);

        let response = codec.decode(&mut buf).unwrap().unwrap();
        match response {
            Response::ReadRegister(RegisterResponse {
                chip_address,
                register,
            }) => {
                assert_eq!(chip_address, 0x00);
                assert!(matches!(register, Register::ChipId { .. }));
            }
            _ => panic!("Expected ReadRegister response"),
        }
    }

    #[test]
    fn decode_nonce_response_from_esp_miner_capture() {
        use crate::asic::bm13xx::test_data::esp_miner_job;

        // Decode nonce response from hardware capture and verify against test data
        let response = decode_frame(&esp_miner_job::wire_rx::FRAME, ChipModel::BM1370)
            .expect("Should decode valid frame");

        let Response::Nonce(NonceResponse {
            nonce,
            job_id,
            excess_difficulty,
            version,
            subcore_id,
        }) = response
        else {
            panic!("Expected nonce response");
        };

        // Verify all fields match test data
        assert_eq!(nonce, *esp_miner_job::wire_rx::NONCE);
        assert_eq!(excess_difficulty, *esp_miner_job::wire_rx::MIDSTATE_NUM);
        assert_eq!(job_id, *esp_miner_job::wire_rx::JOB_ID);
        assert_eq!(subcore_id, *esp_miner_job::wire_rx::SUBCORE_ID);
        // VERSION_ROLLING_FIELD is u16, convert to big-endian bytes
        let expected_bytes = esp_miner_job::wire_rx::VERSION_ROLLING_FIELD.to_be_bytes();
        assert_eq!(version, GeneralPurposeBits::new(expected_bytes));

        // Verify version rolling field shifted left 13 matches submit VERSION
        let bits_as_u16 = u16::from_be_bytes(*version.as_bytes());
        let version_shifted = (bits_as_u16 as u32) << 13;
        assert_eq!(
            version_shifted,
            *esp_miner_job::submit::VERSION,
            "Version rolling field << 13 should match mining.submit version"
        );
    }

    #[test]
    fn test_full_mining_round_trip() {
        use bitcoin::block::Header as BlockHeader;
        use tokio_util::codec::Encoder;

        use super::super::command::{JobCommand, JobFullFormat};
        use crate::asic::bm13xx::test_data::esp_miner_job;
        use crate::types::Difficulty;

        // Build JobFullFormat, encode to wire, decode nonce response,
        // apply version rolling, compute hash, and verify difficulty.
        let job = JobFullFormat {
            job_id: *esp_miner_job::wire_tx::JOB_ID,
            num_midstates: esp_miner_job::wire_tx::NUM_MIDSTATES_BYTE[0],
            starting_nonce: u32::from_le_bytes(
                (*esp_miner_job::wire_tx::STARTING_NONCE_BYTES)
                    .try_into()
                    .unwrap(),
            ),
            nbits: *esp_miner_job::notify::NBITS,
            ntime: *esp_miner_job::notify::NTIME,
            merkle_root: *esp_miner_job::notify::MERKLE_ROOT,
            prev_block_hash: *esp_miner_job::notify::PREV_BLOCKHASH,
            version: *esp_miner_job::notify::VERSION,
        };

        let mut codec = FrameCodec::new(ChipModel::BM1370);
        let mut tx_frame = BytesMut::new();
        codec
            .encode(JobCommand::JobFull(job.clone()), &mut tx_frame)
            .expect("Should encode JobFull command");

        // Our body bytes match esp-miner's wire capture. Byte 3 (length
        // byte) and bytes 86..88 (CRC16) intentionally differ; see the
        // length-byte comment in JobFullFormat::encode.
        assert_eq!(&tx_frame[4..86], &esp_miner_job::wire_tx::FRAME[4..86]);

        let rx_response = decode_frame(&esp_miner_job::wire_rx::FRAME, ChipModel::BM1370)
            .expect("Should decode RX frame");

        let Response::Nonce(NonceResponse {
            nonce,
            job_id: rx_job_id,
            version: version_rolling,
            ..
        }) = rx_response
        else {
            panic!("Expected Nonce response");
        };

        assert_eq!(rx_job_id, job.job_id, "Job ID should round-trip");

        let full_version = version_rolling.apply_to_version(job.version);
        let header = BlockHeader {
            version: full_version,
            prev_blockhash: job.prev_block_hash,
            merkle_root: job.merkle_root,
            time: job.ntime,
            bits: job.nbits,
            nonce,
        };

        let hash = header.block_hash();
        let difficulty = Difficulty::from_hash(&hash);

        // Allow +/-1 tolerance for integer division rounding
        let expected = Difficulty::from(esp_miner_job::EXPECTED_HASH_DIFFICULTY as u64);
        assert!(
            difficulty >= Difficulty::from(expected.as_u64() - 1)
                && difficulty <= Difficulty::from(expected.as_u64() + 1),
            "Hash difficulty should match esp-miner result"
        );
        assert!(
            difficulty >= Difficulty::from(esp_miner_job::POOL_SHARE_DIFFICULTY_INT),
            "Hash should meet pool difficulty"
        );
    }
}
