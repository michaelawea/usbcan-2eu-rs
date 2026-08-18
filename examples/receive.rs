//! Print every frame arriving on channel 0.
//!
//! ```text
//! cargo run --example receive
//! ```
//!
//! An idle bus produces `Error::Timeout` on every poll. That is the normal
//! quiet state, not a failure, so the loop simply continues.

#[cfg(target_os = "macos")]
fn main() -> Result<(), usbcan_2eu::Error> {
    use usbcan_2eu::{Bitrate, ChannelMode, Device, Error};

    const CHANNEL: u8 = 0;

    let device = Device::open_first()?;
    device.start_channel(CHANNEL, Bitrate::Kbps500, ChannelMode::Normal)?;
    println!("listening on channel {CHANNEL} at 500 kbit/s");

    loop {
        match device.receive(500) {
            Ok(chunk) => {
                for frame in chunk.frames_on(CHANNEL) {
                    println!("{:>10} us  {}", frame.timestamp_us, frame);
                }
                // Status records tell you why a quiet bus is quiet.
                for status in &chunk.status {
                    println!("  bus status: {:?}", status.kind);
                }
            }
            Err(Error::Timeout { .. }) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This example needs macOS; the USB transport is not implemented elsewhere.");
}
