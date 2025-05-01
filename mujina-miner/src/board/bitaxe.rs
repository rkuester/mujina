use tokio::io::AsyncWriteExt;
use tokio_serial::{self, SerialPortBuilderExt};

pub const CONTROL_SERIAL: &'static str = "/dev/ttyACM0";
pub const DATA_SERIAL: &'static str = "/dev/ttyACM1";

pub async fn deassert_reset() {
    // TODO: Open serial elsewhere, use a proper codec and high-level messages.

    let mut port = tokio_serial::new(CONTROL_SERIAL, 115200)
        .open_native_async()
        .expect("failed to open control serial port");

    const RSTN_HI: &[u8] = &[0x07, 0x00, 0x00, 0x00, 0x06, 0x00, 0x01];
    port.write_all(&RSTN_HI).await.unwrap();
    port.flush().await.unwrap();
}
