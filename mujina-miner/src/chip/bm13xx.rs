use bitvec::prelude::*;
use bytes::{Buf, BytesMut, BufMut};
use crc_all::Crc;
use std::io;
use tokio_util::codec::{Encoder, Decoder};

use crate::tracing::prelude::*;

#[repr(u8)]
pub enum Register {
    ChipAddress = 0,
}

impl TryFrom<u8> for Register {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            value if value == Register::ChipAddress  as u8 => Ok(Register::ChipAddress),
            _ => Err(()),
        }
    }
}

pub enum Command {
    ReadRegister { all: bool, address: u8, register: Register },
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
enum ResponseType {
    Command = 0,
    Job = 4,
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
        self.with_type(
            match command {
                Command::ReadRegister {..} => CommandFieldType::Command,
            }
        )
    }

    fn with_all(mut self, all: &bool) -> Self {
        let view = self.field.view_bits_mut::<Lsb0>();
        view[4..5].store(*all as u8);
        self
    }

    fn with_all_for_command(self, command: &Command) -> Self {
        self.with_all(
            match command {
                Command::ReadRegister {all, ..} => all,
            }
        )
    }

    fn with_cmd(mut self, cmd: CommandFieldCmd) -> Self {
        let view = self.field.view_bits_mut::<Lsb0>();
        view[0..4].store(cmd as u8);
        self
    }

    fn with_cmd_for_command(self, command: &Command) -> Self {
        self.with_cmd(
            match command {
                Command::ReadRegister {..} => CommandFieldCmd::ReadRegister,
            }
        )
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

fn crc5_usb(bytes: &[u8]) -> u8 {
    const POLYNOMIAL: u8 = 0x05;
    const WIDTH: usize = 5;
    const INITIAL: u8 = 0x1f;
    const XOR: u8 = 0;
    const REFLECT: bool = false;
    let mut crc5_usb = Crc::<u8>::new(POLYNOMIAL, WIDTH, INITIAL, XOR, REFLECT);

    crc5_usb.update(bytes);
    crc5_usb.finish()
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
            Command::ReadRegister { all: _, address, register } => {
                const LENGTH: u8 = 5;
                dst.put_u8(LENGTH);
                dst.put_u8(address);
                dst.put_u8(register as u8);
            }
        }

        let crc = crc5_usb(&dst[2..]);
        dst.put_u8(crc);

        Ok(())
    }
}

pub enum Response {
    RegisterValue { value: u32, address: u8, register: Register },
    Nonce,
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

        let frame_len = if self.version_rolling {
            ROLLING_FRAME_LEN
        } else {
            NONROLLING_FRAME_LEN
        };

        if src.len() < frame_len {
            return Ok(None);
        }

        let mut prospect = src.clone();  // avoid consuming real buffer as we provisionally parse

        if prospect.get_u8() != PREAMBLE[0] {
            src.advance(1);
            return Ok(None);
        }

        if prospect.get_u8() != PREAMBLE[1] {
            src.advance(1);
            return Ok(None);
        }

        let kind_and_crc = src[frame_len - 1].view_bits::<Lsb0>();
        let kind = kind_and_crc[5..].load::<u8>();
        let crc = kind_and_crc[..4].load::<u8>();

        eprintln!("kind {} crc {}", kind, crc);

        //                        8    16    24    32    40    48    56    64
        let crc5 = crc5_usb(&[0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00]);
        eprintln!("crc 0x{:x} 0b{:b}", crc5, crc5);

        if crc == crc5_usb(&src[PREAMBLE.len()..(frame_len - 2)]) {
            src.advance(frame_len);
        } else {
            src.advance(1);
            eprintln!("BAD CRC");
            return Ok(None);
        }

        const COMMAND_RESPONSE: u8 = 0;
        const JOB_RESPONSE: u8 = 4;

        Ok(match kind {
            COMMAND_RESPONSE => {
                let value = prospect.get_u32();  // TODO: le or be?
                let address = prospect.get_u8();
                let register_address = prospect.get_u8();

                if let Ok(register) = Register::try_from(register_address) {
                    Some(Response::RegisterValue { value, address, register })
                } else {
                    debug!("Unknown register {} in response from chip.", register_address);
                    None
                }
            },
            JOB_RESPONSE => {
                Some(Response::Nonce)
            },
            _ => {
                debug!("Unknown response type {} from chip.", kind);
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc5_usb() {
    }

    fn as_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<String>>()
            .join(" ")
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

    #[test]
    fn command_read_register() {
        assert_frame_eq(
            Command::ReadRegister {
                all: true,
                address: 0,
                register: Register::ChipAddress,
            },
            &[0x55, 0xaa, 0x52, 0x05, 0x00, 0x00, 0x0a],
        );
    }

    fn decode_frame(frame: &[u8]) -> Option<Response> {
        let mut buf = BytesMut::from(frame);
        let mut codec = FrameCodec::default();
        codec.decode(&mut buf).unwrap()
    }

    #[test]
    #[ignore]
    fn response_register_value_wip() { 
        let wire = &[0xaa, 0x55, 0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x06];
        let result = decode_frame(wire);
        assert!(result.is_some());
        
        // If you want to test the specific value inside Some:
        // if let Some(response) = result {
        //     match response {
        //         Response::RegisterValue { value, address, register } => {
        //             assert_eq!(value, 0x78563412); // Assuming little-endian byte order
        //             assert_eq!(address, 0x01);
        //             assert_eq!(register as u8, 0x00);
        //         }
        //     }
        // }
    }
}
