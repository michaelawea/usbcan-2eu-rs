//! Send on channel 0, receive on channel 1.
//!
//! Wire the two channels together first: CANH to CANH, CANL to CANL, 120 ohm
//! across each end. This is the same check `usbcan2eu selftest` performs, written
//! out longhand so it is clear what the driver is doing.
//!
//! ```text
//! cargo run --example loopback
//! ```

#[cfg(target_os = "macos")]
fn main() -> Result<(), usbcan_2eu::Error> {
    use std::time::{Duration, Instant};
    use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device, Error};

    const BITRATE: Bitrate = Bitrate::Kbps500;
    const COUNT: usize = 20;

    let device = Device::open_first()?;
    if device.channel_count() < 2 {
        println!("This example needs a two-channel adapter.");
        return Ok(());
    }

    device.start_channel(0, BITRATE, ChannelMode::Normal)?;
    device.start_channel(1, BITRATE, ChannelMode::Normal)?;

    for i in 0..COUNT {
        let frame = CanFrame::new(0x200 + i as u32, false, &(i as u32).to_le_bytes());
        device.transmit(0, &[frame])?;
    }

    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while received.len() < COUNT && Instant::now() < deadline {
        match device.receive(200) {
            Ok(chunk) => received.extend(chunk.frames_on(1).copied()),
            Err(Error::Timeout { .. }) => continue,
            Err(e) => return Err(e),
        }
    }

    println!(
        "{}/{COUNT} frames made it from channel 0 to channel 1",
        received.len()
    );
    for frame in &received {
        println!("  {frame}");
    }

    device.stop_channel(0)?;
    device.stop_channel(1)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This example needs macOS; the USB transport is not implemented elsewhere.");
}
