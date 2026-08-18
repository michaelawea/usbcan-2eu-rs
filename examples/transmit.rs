//! Send one standard frame and one extended frame on channel 0.
//!
//! ```text
//! cargo run --example transmit
//! ```
//!
//! This puts real traffic on whatever bus the adapter is wired to. The
//! acknowledgement only confirms the adapter accepted the frame; it says nothing
//! about any other node receiving it.

#[cfg(target_os = "macos")]
fn main() -> Result<(), usbcan_2eu::Error> {
    use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device};

    const CHANNEL: u8 = 0;

    let device = Device::open_first()?;
    device.start_channel(CHANNEL, Bitrate::Kbps500, ChannelMode::Normal)?;

    let frames = [
        CanFrame::new(0x123, false, &[0xde, 0xad, 0xbe, 0xef]),
        CanFrame::new(0x1abc_def0, true, &[1, 2, 3, 4, 5, 6, 7, 8]),
    ];

    for frame in &frames {
        let ack = device.transmit(CHANNEL, std::slice::from_ref(frame))?;
        println!(
            "{frame}  ->  {}",
            if ack.is_success() {
                "accepted".to_string()
            } else {
                format!("rejected, status 0x{:02X}", ack.status)
            }
        );
    }

    // Several frames also go out in one USB transfer, which is much faster than
    // one call per frame when you have a burst to send.
    device.transmit(CHANNEL, &frames)?;

    device.stop_channel(CHANNEL)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This example needs macOS; the USB transport is not implemented elsewhere.");
}
