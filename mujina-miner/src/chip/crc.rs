use crc_all::CrcAlgo;

const CRC5_INIT: u8 = 0x1f;

const CRC5_USB: CrcAlgo<u8> = CrcAlgo::<u8>::new(
    0x5,       // polynomial
    5,         // width
    CRC5_INIT, // init
    0,         // xorout
    false,     // reflect
);

/// Calculates a 5-bit CRC using the USB polynomial over a slice of bytes.
///
/// This function implements the CRC-5-USB algorithm which uses polynomial 0x05,
/// an initial value of 0x1f, no output XOR. The algorithm does not use bit reflection.
///
/// Note that while CRCs are conceptually bit-oriented operations, this implementation
/// processes data in byte-sized chunks. The CRC is calculated over the entire sequence
/// of bits in the provided bytes.
pub fn crc5(data: &[u8]) -> u8 {
    let mut crc = CRC5_INIT;
    CRC5_USB.update_crc(&mut crc, data);
    CRC5_USB.finish_crc(&crc)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    // Test that a computed CRC5 matches that of a few frames known to be good, taken from the
    // esp-miner source code. Skip the first two bytes, which are a prefix, and the last byte,
    // which is the expected CRC.
    #[test_case(&[0x55, 0xaa, 0x52, 0x05, 0x00, 0x00, 0x0a]; "read_register_0")]
    #[test_case(&[0x55, 0xaa, 0x51, 0x09, 0x00, 0x28, 0x11, 0x30, 0x02, 0x00, 0x03]; "set_baud")]
    fn golden_frames(frame: &[u8]) {
        let crc = super::crc5(&frame[2..frame.len() - 1]);
        let expect = frame[frame.len() - 1];
        assert_eq!(crc, expect);
    }
}
