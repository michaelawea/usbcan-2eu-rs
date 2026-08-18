//! Userspace driver for the ZLG USBCAN-E-U and USBCAN-2E-U CAN interfaces on macOS.
//!
//! The vendor ships Windows and Linux drivers but nothing for macOS. This crate
//! speaks the adapter's USB protocol directly through Apple's IOUSBHost framework,
//! so no kernel extension, no vendor library, and no code signing are involved.
//!
//! This is an independent, unofficial implementation. It is not affiliated with,
//! authorized by, or supported by the hardware vendor.
//!
//! # Layout
//!
//! - [`protocol`] — packet layouts and bit timing as pure functions. No I/O, and
//!   available on every platform, so the wire format can be tested anywhere.
//! - [`device`] — the USB transport. macOS only.
//! - [`slcan`] — a Lawicel/SLCAN bridge on a pseudo-terminal, which makes the
//!   adapter usable from python-can and other tools that speak SLCAN.
//!
//! # Getting started
//!
//! ```no_run
//! # // The transport is macOS-only, so this example is compiled only there.
//! # #[cfg(target_os = "macos")]
//! # fn example() -> Result<(), usbcan_2eu::Error> {
//! use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device};
//!
//! let device = Device::open_first()?;
//! device.start_channel(0, Bitrate::Kbps500, ChannelMode::Normal)?;
//!
//! device.transmit(0, &[CanFrame::new(0x123, false, &[0xde, 0xad])])?;
//!
//! loop {
//!     match device.receive(200) {
//!         Ok(chunk) => {
//!             for frame in &chunk.frames {
//!                 println!("{frame}");
//!             }
//!         }
//!         // An idle bus simply produces no data.
//!         Err(usbcan_2eu::Error::Timeout { .. }) => continue,
//!         Err(e) => return Err(e),
//!     }
//! }
//! # }
//! ```
//!
//! # Command-line tool
//!
//! The crate also builds a `usbcan2eu` binary. `usbcan2eu selftest` is the quickest
//! way to confirm a unit works end to end.
//!
//! # Safety on a live bus
//!
//! Transmitting puts real frames on a real bus. Do not point this at a vehicle,
//! battery system, or any other equipment you are not prepared to disturb.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod error;
pub mod protocol;

#[cfg(target_os = "macos")]
#[cfg_attr(docsrs, doc(cfg(target_os = "macos")))]
pub mod device;

#[cfg(all(target_os = "macos", feature = "slcan"))]
#[cfg_attr(docsrs, doc(cfg(all(target_os = "macos", feature = "slcan"))))]
pub mod slcan;

#[cfg(feature = "embedded-can")]
mod embedded_can_impl;

#[cfg(all(target_os = "macos", feature = "embedded-can"))]
#[cfg_attr(docsrs, doc(cfg(feature = "embedded-can")))]
pub use embedded_can_impl::CanChannel;

pub use error::Error;
pub use protocol::{
    Bitrate, CanFrame, ChannelMode, CtrlCmd, CtrlResp, RxChunk, RxStatus, RxStatusKind, TxAck,
};

#[cfg(target_os = "macos")]
pub use device::{Device, DeviceInfo, EndpointInfo};

/// Convenience alias for results carrying this crate's [`Error`].
pub type Result<T> = core::result::Result<T, Error>;
