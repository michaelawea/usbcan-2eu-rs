# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-18

First release.

### Added

- USB transport for macOS built on IOUSBHost, avoiding the libusb isochronous
  crash described in `docs/macos-usb-isoc-kernel-bug.md`.
- Wire protocol implementation: control commands, transmit packets, transmit
  acknowledgement, receive decoding, and an empirically calibrated bit timing
  table covering 10 kbit/s to 1 Mbit/s.
- `Device` API: enumerate, open, start and stop channels, transmit, receive,
  read endpoint descriptors.
- `usbcan2eu` command-line tool with `info`, `dump`, `send`, `selftest` and
  `slcan` subcommands.
- SLCAN bridge over a pseudo-terminal, making the adapter usable from python-can
  and other tools that speak the Lawicel ASCII protocol.
- `embedded-can` trait implementations behind a feature flag.
- `serde` serialization behind a feature flag.
- Decoder tests driven by captures from real hardware, runnable without a device;
  hardware tests behind `#[ignore]`.
- Protocol, troubleshooting, hardware-testing and kernel-bug documentation.

### Known limitations

- Only the two-channel USBCAN-2E-U (`0x1261`) has been verified. The
  single-channel USBCAN-E-U (`0x1260`) is matched and believed to work but has
  never been tested.
- Hardware acceptance filters, scheduled transmission, software bus termination,
  serial number access and merged-receive mode are not implemented.
- Listen-only mode has not been confirmed to be true electrical silence.
- macOS only.

[Unreleased]: https://github.com/michaelawea/usbcan-2eu-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/michaelawea/usbcan-2eu-rs/releases/tag/v0.1.0
