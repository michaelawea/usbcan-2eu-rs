# usbcan-2eu

A userspace driver for **ZLG USBCAN-E-U and USBCAN-2E-U** CAN interfaces on
**macOS**, written in Rust.

The vendor ships drivers for Windows and Linux but nothing for macOS. This crate
talks to the adapter directly over USB through Apple's IOUSBHost framework, so
there is no kernel extension to install, no vendor library to link against, and
nothing to code-sign.

[中文说明](README.zh-CN.md)

---

> **Unofficial.** This project is not affiliated with, authorized by, endorsed by,
> or supported by Guangzhou ZHIYUAN Electronics (ZLG). "ZLG" and "USBCAN" are
> trademarks of their respective owners, used here only to identify the hardware
> this driver works with. No vendor software, driver, firmware, or documentation
> is included or redistributed. You need to own the hardware.

---

## Quick start

```bash
git clone https://github.com/michaelawea/usbcan-2eu-rs
cd usbcan-2eu-rs
cargo build --release

# Is the adapter there, and what does it look like?
./target/release/usbcan2eu info

# Watch a bus.
./target/release/usbcan2eu dump --bitrate 250k

# Put a frame on it.
./target/release/usbcan2eu send 123#DEADBEEF --bitrate 250k
```

Requires macOS 11 or newer and Rust 1.85 or newer. Apple Silicon and Intel are
both fine. No `sudo`, no driver installation, no reboot.

## Does it actually work?

Wire channel 0 to channel 1 — CANH to CANH, CANL to CANL, 120 Ω across each end —
and ask it:

```bash
usbcan2eu selftest --bitrate 500k
```

```text
  adapter          USBCAN-2E-U at index 0
  both channels    started at 500 kbit/s

  can0 -> can1     100/100 frames, 0 errors
  can1 -> can0     100/100 frames, 0 errors

PASS
```

That exercises the whole stack — bit timing, packet framing, transmit
acknowledgement, receive decoding — against real silicon, with no other equipment
involved. It is also the first thing to run before filing a bug.

## Using it from other tools

### SLCAN, for python-can and everything else

macOS has no SocketCAN, so most CAN tooling cannot see this adapter directly.
The bridge exposes it as an SLCAN serial device instead:

```bash
usbcan2eu slcan --bitrate 500k
# SLCAN bridge for channel 0 is up.
#   device   /dev/ttys004
```

```python
import can

bus = can.Bus(interface="slcan", channel="/dev/ttys004", bitrate=500000)
for msg in bus:
    print(msg)
```

Anything that speaks SLCAN over a serial port works the same way: `cantools`,
`SavvyCAN`, your own script.

### embedded-can

With the `embedded-can` feature, [`CanFrame`] implements `embedded_can::Frame` and
[`CanChannel`] implements `embedded_can::blocking::Can`, so code written against
the generic traits runs unmodified:

```toml
[dependencies]
usbcan-2eu = { version = "0.1", default-features = false, features = ["embedded-can"] }
```

### As a library

```toml
[dependencies]
usbcan-2eu = { version = "0.1", default-features = false }
```

```rust,no_run
use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device, Error};

let device = Device::open_first()?;
device.start_channel(0, Bitrate::Kbps500, ChannelMode::Normal)?;

device.transmit(0, &[CanFrame::new(0x123, false, &[0xde, 0xad])])?;

loop {
    match device.receive(200) {
        Ok(chunk) => chunk.frames_on(0).for_each(|f| println!("{f}")),
        Err(Error::Timeout { .. }) => continue,   // an idle bus, not a failure
        Err(e) => return Err(e),
    }
}
# Ok::<(), Error>(())
```

`cargo run --example receive`, `--example transmit`, `--example loopback`, and
`--example list_devices` are short, self-contained versions of the above.

### Feature flags

| Feature | Default | What it adds |
|---|---|---|
| `cli` | yes | The `usbcan2eu` binary. Disable it for library-only use so `clap` is not pulled in. |
| `slcan` | via `cli` | The SLCAN bridge. |
| `embedded-can` | no | `embedded_can` trait implementations. |
| `serde` | no | `Serialize` on the public data types. |

## Hardware support

| Model | Product ID | Channels | Status |
|---|---|---|---|
| USBCAN-2E-U | `0x1261` | 2 | Verified against real hardware |
| USBCAN-E-U | `0x1260` | 1 | Should work, **never tested — please report** |

Reverse-engineered protocols drift between firmware revisions. `usbcan2eu info`
prints an identification value; include it when reporting anything.

If you have a USBCAN-E-U, running `usbcan2eu info` and opening an issue with the
output — working or not — is a genuinely useful contribution. See
[`docs/hardware-testing.md`](docs/hardware-testing.md).

Supported bit rates: 10, 20, 50, 100, 125, 250, 500, 800 and 1000 kbit/s. Every
one is exact rather than approximated; see
[`docs/protocol.md`](docs/protocol.md#bit-timing).

## How the protocol was determined

The adapter speaks an undocumented USB protocol. This implementation was built by
studying the vendor's Windows driver stack and then verifying every conclusion
against a real device — packet layouts, checksums, command sequences and bit
timing were all confirmed by observing what the hardware actually does.

The documentation describes the device's observable behaviour, which is what the
implementation depends on and what anyone with a USB analyzer can check
independently. This repository contains no vendor code, binaries, headers, or
decompiler output, and none were copied into its source.

Where a conclusion is inferred rather than measured, the code and documentation
say so. The controller clock frequency in
[`docs/protocol.md`](docs/protocol.md#bit-timing) is the most consequential
example, and the note there explains exactly how it was pinned down.

## Documentation

| Document | Contents |
|---|---|
| [`docs/protocol.md`](docs/protocol.md) | The wire protocol, for anyone reimplementing it in another language |
| [`docs/macos-usb-isoc-kernel-bug.md`](docs/macos-usb-isoc-kernel-bug.md) | Why this driver does not use libusb, and the macOS kernel bug behind that |
| [`docs/hardware-testing.md`](docs/hardware-testing.md) | Running the tests that need a device attached |
| [`docs/troubleshooting.md`](docs/troubleshooting.md) | Symptom-to-cause table |
| [API documentation](https://docs.rs/usbcan-2eu) | Generated from the source |

## Safety

This tool transmits on a real CAN bus. On vehicles, battery systems, and
industrial equipment, CAN frames cause things to move, switch, and change state.

- Do not use it on a vehicle in motion.
- Do not point it at equipment you are not prepared to disturb.
- `--listen-only` keeps a channel from driving the bus, though this has not been
  confirmed to be true electrical silence with an analyzer.

The software is provided without warranty of any kind. See the licenses.

## Contributing

Issues and pull requests are welcome, particularly device reports from models
other than the USBCAN-2E-U. See [`CONTRIBUTING.md`](CONTRIBUTING.md) — including
how to contribute without owning the hardware.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. Unless you state otherwise, any contribution you intentionally
submit for inclusion shall be dual-licensed as above, with no additional terms.

[`CanFrame`]: https://docs.rs/usbcan-2eu/latest/usbcan_2eu/protocol/struct.CanFrame.html
[`CanChannel`]: https://docs.rs/usbcan-2eu/latest/usbcan_2eu/struct.CanChannel.html
