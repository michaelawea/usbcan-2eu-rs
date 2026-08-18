//! Enumerate attached adapters and print their endpoint tables.
//!
//! Nothing here transmits, so it is safe to run with the adapter connected to a
//! live bus.
//!
//! ```text
//! cargo run --example list_devices
//! ```

#[cfg(target_os = "macos")]
fn main() -> Result<(), usbcan_2eu::Error> {
    use usbcan_2eu::Device;

    let adapters = Device::list()?;
    if adapters.is_empty() {
        println!("No adapter found. Is it plugged in?");
        return Ok(());
    }

    for (index, info) in adapters.iter().enumerate() {
        println!(
            "[{index}] {} - VID 0x{:04x}, PID 0x{:04x}, {} channel(s)",
            info.model(),
            info.vid,
            info.pid,
            info.channel_count()
        );
    }

    // Opening is what lets us read descriptors and identify the firmware.
    let device = Device::open(0)?;
    println!(
        "\nAdapter 0 identification: {:02X?}",
        device.identification()
    );
    println!("Endpoints:");
    for ep in device.endpoints() {
        println!(
            "  0x{:02X}  {:<11} {:<3}  max packet {} bytes",
            ep.address,
            ep.transfer_type_name(),
            ep.direction_name(),
            ep.max_packet_size
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This example needs macOS; the USB transport is not implemented elsewhere.");
}
