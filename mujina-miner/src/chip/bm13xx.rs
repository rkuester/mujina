//! This module implements an interface to the BM13xx chips.

use bitvec::prelude::*;
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use strum::FromRepr;
use tokio_util::codec::{Decoder, Encoder};

use crate::tracing::prelude::*;
use crc::*;

mod crc;

#[derive(FromRepr)]
#[repr(u8)]
pub enum RegisterAddress {
    ChipAddress = 0x00,
    // MiscControl = 0x18,
    // FastUartConfiguration = 0x28,
    // Pll1Parameter = 0x60,
    // VersionRolling = 0xa4,
    // RegA8 = 0xa8,
}

pub enum Register {
    ChipAddress {
        chip_id: u16,
        core_count: u8,
        address: u8,
    },
}

impl Register {
    fn decode(address: RegisterAddress, bytes: &[u8; 4]) -> Register {
        match address {
            RegisterAddress::ChipAddress => Register::ChipAddress {
                chip_id: u16::from_be_bytes(bytes[0..2].try_into().unwrap()),
                core_count: u8::from_be(bytes[2]),
                address: u8::from_be(bytes[3]),
            },
        }
    }
}

pub enum Command {
    ReadRegister {
        all: bool,
        chip_address: u8,
        register_address: RegisterAddress,
    },
}

struct CommandFieldBuilder {
    field: u8,
}

#[repr(u8)]
enum CommandFieldType {
    // Job = 1,
    Command = 2,
}

#[repr(u8)]
enum CommandFieldCmd {
    // SetAddress = 0,
    // WriteRegisterOrJob = 1,
    ReadRegister = 2,
    // ChainInactive = 3,
}

impl CommandFieldBuilder {
    fn new() -> Self {
        Self { field: 0 }
    }

    fn with_type(mut self, command_type: CommandFieldType) -> Self {
        let view = self.field.view_bits_mut::<Lsb0>();
        view[5..7].store(command_type as u8);
        self
    }

    fn with_type_for_command(self, command: &Command) -> Self {
        self.with_type(match command {
            Command::ReadRegister { .. } => CommandFieldType::Command,
        })
    }

    fn with_all(mut self, all: &bool) -> Self {
        let view = self.field.view_bits_mut::<Lsb0>();
        view[4..5].store(*all as u8);
        self
    }

    fn with_all_for_command(self, command: &Command) -> Self {
        self.with_all(match command {
            Command::ReadRegister { all, .. } => all,
        })
    }

    fn with_cmd(mut self, cmd: CommandFieldCmd) -> Self {
        let view = self.field.view_bits_mut::<Lsb0>();
        view[0..4].store(cmd as u8);
        self
    }

    fn with_cmd_for_command(self, command: &Command) -> Self {
        self.with_cmd(match command {
            Command::ReadRegister { .. } => CommandFieldCmd::ReadRegister,
        })
    }

    fn for_command(self, command: &Command) -> Self {
        self.with_type_for_command(command)
            .with_all_for_command(command)
            .with_cmd_for_command(command)
    }

    fn build(self) -> u8 {
        self.field
    }
}

#[derive(FromRepr)]
#[repr(u8)]
enum ResponseType {
    ReadRegister = 0,
    Nonce = 4,
}

pub enum Response {
    ReadRegister {
        chip_address: u8,
        register: Register,
    },
    Nonce,
}

impl Response {
    fn decode(bytes: &mut BytesMut, is_version_rolling: bool) -> Result<Response, String> {
        let type_and_crc = bytes[bytes.len() - 1].view_bits::<Lsb0>();
        let type_repr = type_and_crc[5..].load::<u8>();

        match ResponseType::from_repr(type_repr) {
            Some(ResponseType::ReadRegister) => {
                let value: &[u8; 4] = &bytes.split_to(4)[..].try_into().unwrap();
                let chip_address = bytes.get_u8();
                let register_address_repr = bytes.get_u8();

                if let Some(register_address) = RegisterAddress::from_repr(register_address_repr) {
                    let register = Register::decode(register_address, value);
                    Ok(Response::ReadRegister {
                        chip_address,
                        register,
                    })
                } else {
                    Err(format!(
                        "unknown register address 0x{:x}.",
                        register_address_repr
                    ))
                }
            }
            Some(ResponseType::Nonce) => {
                panic!("not implemented")
            }
            None => Err(format!("unknown response type 0x{:x}.", type_repr)),
        }
    }
}

#[derive(Default)]
pub struct FrameCodec {
    // Controls whether to use the alternative frame format required when version rolling
    // is enabled. When true, uses version rolling compatible format. (default: false)
    version_rolling: bool,
}

impl Encoder<Command> for FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, command: Command, dst: &mut BytesMut) -> Result<(), Self::Error> {
        const COMMAND_PREAMBLE: &[u8] = &[0x55, 0xaa];
        dst.put_slice(COMMAND_PREAMBLE);

        let command_field = CommandFieldBuilder::new().for_command(&command).build();
        dst.put_u8(command_field);

        match command {
            Command::ReadRegister {
                all: _,
                chip_address: address,
                register_address: register,
            } => {
                const LENGTH: u8 = 5;
                dst.put_u8(LENGTH);
                dst.put_u8(address);
                dst.put_u8(register as u8);
            }
        }

        let crc = crc5(&dst[2..]);
        dst.put_u8(crc);

        Ok(())
    }
}

impl Decoder for FrameCodec {
    type Item = Response;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Return Ok(Item) with a valid frame, or Ok(None) if to be called again, potentially with
        // more data. Returning an Error causes the stream to be terminated, so don't do that.
        //
        // There are three cases:
        //
        // 1. More data needed
        // 2. Invalid frame
        // 3. Valid frame
        //
        // In the case of an invalid frame, consume the first byte and request another call by
        // returning Ok(None). In the case of a valid frame, consume that frame's worth of bytes.

        const PREAMBLE: &[u8] = &[0xaa, 0x55];
        const NONROLLING_FRAME_LEN: usize = PREAMBLE.len() + 7;
        const ROLLING_FRAME_LEN: usize = PREAMBLE.len() + 9;
        const CALL_AGAIN: Result<Option<Response>, io::Error> = Ok(None);

        let frame_len = if self.version_rolling {
            ROLLING_FRAME_LEN
        } else {
            NONROLLING_FRAME_LEN
        };

        if src.len() < frame_len {
            return CALL_AGAIN;
        }

        let mut prospect = src.clone(); // avoid consuming real buffer as we provisionally parse

        if prospect.get_u8() != PREAMBLE[0] {
            src.advance(1);
            return CALL_AGAIN;
        }

        if prospect.get_u8() != PREAMBLE[1] {
            src.advance(1);
            return CALL_AGAIN;
        }

        if !crc5_is_valid(&prospect[..]) {
            src.advance(1);
            return CALL_AGAIN;
        } else {
            src.advance(frame_len);
        }

        match Response::decode(&mut prospect, self.version_rolling) {
            Ok(response) => Ok(Some(response)),
            Err(msg) => {
                warn!(msg);
                CALL_AGAIN
            }
        }
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn read_register() {
        assert_frame_eq(
            Command::ReadRegister {
                all: true,
                chip_address: 0,
                register_address: RegisterAddress::ChipAddress,
            },
            &[0x55, 0xaa, 0x52, 0x05, 0x00, 0x00, 0x0a],
        );
    }

    fn assert_frame_eq(cmd: Command, expect: &[u8]) {
        let mut codec = FrameCodec::default();
        let mut frame = BytesMut::new();
        codec.encode(cmd, &mut frame).unwrap();
        if frame != expect {
            panic!(
                "mismatch!\nexpected: {}\nactual: {}",
                as_hex(expect),
                as_hex(&frame[..])
            )
        }
    }

    fn as_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<String>>()
            .join(" ")
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn read_register() {
        let wire = &[0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x06];
        let response = decode_frame(wire).unwrap();

        let Response::ReadRegister {
            chip_address,
            register,
        } = response
        else {
            panic!();
        };

        assert_eq!(chip_address, 0x00);

        let Register::ChipAddress {
            chip_id,
            core_count,
            address,
        } = register
        else {
            panic!();
        };

        assert_eq!(chip_id, 0x1370);
        assert_eq!(core_count, 0x00);
        assert_eq!(address, 0x00);
    }

    fn decode_frame(frame: &[u8]) -> Option<Response> {
        let mut buf = BytesMut::from(frame);
        let mut codec = FrameCodec::default();
        codec.decode(&mut buf).unwrap()
    }
}

// Bytes go out on the wire least-significant byte first.
// Multi-byte fields are sent most-significant byte first, i.e., big-endian.
