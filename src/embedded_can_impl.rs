//! `embedded-can` trait implementations.
//!
//! Enabled by the `embedded-can` feature. Lets code written against the generic
//! traits drive this adapter without knowing about it.

use embedded_can::{ExtendedId, Frame, Id, StandardId};

use crate::protocol::CanFrame;

impl Frame for CanFrame {
    fn new(id: impl Into<Id>, data: &[u8]) -> Option<Self> {
        if data.len() > 8 {
            return None;
        }
        let (raw, extended) = split_id(id.into());
        Some(CanFrame::new(raw, extended, data))
    }

    fn new_remote(id: impl Into<Id>, dlc: usize) -> Option<Self> {
        if dlc > 8 {
            return None;
        }
        let (raw, extended) = split_id(id.into());
        Some(CanFrame::new_remote(raw, extended, dlc as u8))
    }

    fn is_extended(&self) -> bool {
        self.extended
    }

    fn is_remote_frame(&self) -> bool {
        self.remote
    }

    fn id(&self) -> Id {
        if self.extended {
            Id::Extended(ExtendedId::new(self.id).unwrap_or(ExtendedId::ZERO))
        } else {
            Id::Standard(StandardId::new(self.id as u16).unwrap_or(StandardId::ZERO))
        }
    }

    fn dlc(&self) -> usize {
        self.dlc as usize
    }

    fn data(&self) -> &[u8] {
        self.payload()
    }
}

fn split_id(id: Id) -> (u32, bool) {
    match id {
        Id::Standard(s) => (u32::from(s.as_raw()), false),
        Id::Extended(e) => (e.as_raw(), true),
    }
}

#[cfg(target_os = "macos")]
mod blocking_impl {
    use std::collections::VecDeque;

    use crate::device::Device;
    use crate::error::Error;
    use crate::protocol::CanFrame;

    /// One CAN channel of a [`Device`], as an `embedded_can::blocking::Can`.
    ///
    /// The device delivers frames for both channels in one stream, so this buffers
    /// what it reads and hands out one frame at a time. Frames belonging to the
    /// other channel are discarded.
    ///
    /// ```no_run
    /// use embedded_can::blocking::Can;
    /// use usbcan_2eu::{Bitrate, CanChannel, ChannelMode, Device};
    ///
    /// # fn main() -> Result<(), usbcan_2eu::Error> {
    /// let device = Device::open_first()?;
    /// device.start_channel(0, Bitrate::Kbps500, ChannelMode::Normal)?;
    /// let mut can = CanChannel::new(&device, 0);
    /// let frame = can.receive()?;
    /// # Ok(())
    /// # }
    /// ```
    pub struct CanChannel<'a> {
        device: &'a Device,
        channel: u8,
        buffered: VecDeque<CanFrame>,
        poll_timeout_ms: u32,
    }

    impl<'a> CanChannel<'a> {
        /// Wrap `channel` of `device`.
        pub fn new(device: &'a Device, channel: u8) -> Self {
            Self {
                device,
                channel,
                buffered: VecDeque::new(),
                poll_timeout_ms: 1000,
            }
        }

        /// Set how long a single `receive` waits before returning
        /// [`Error::Timeout`]. Defaults to 1000 ms.
        pub fn set_poll_timeout_ms(&mut self, ms: u32) {
            self.poll_timeout_ms = ms;
        }

        /// The channel index this wraps.
        pub fn channel(&self) -> u8 {
            self.channel
        }
    }

    impl embedded_can::blocking::Can for CanChannel<'_> {
        type Frame = CanFrame;
        type Error = Error;

        fn transmit(&mut self, frame: &Self::Frame) -> Result<(), Self::Error> {
            self.device
                .transmit(self.channel, std::slice::from_ref(frame))?;
            Ok(())
        }

        fn receive(&mut self) -> Result<Self::Frame, Self::Error> {
            if let Some(frame) = self.buffered.pop_front() {
                return Ok(frame);
            }
            let chunk = self.device.receive(self.poll_timeout_ms)?;
            self.buffered.extend(chunk.frames_on(self.channel).copied());
            self.buffered.pop_front().ok_or(Error::Timeout {
                ms: self.poll_timeout_ms,
            })
        }
    }
}

#[cfg(target_os = "macos")]
pub use blocking_impl::CanChannel;

#[cfg(test)]
mod tests {
    use super::*;

    // `CanFrame` has an inherent `new` taking a raw id, which shadows the trait
    // method. Constructors are therefore spelled out through the trait here.
    type F = CanFrame;

    #[test]
    fn standard_frame_roundtrip() {
        let id = StandardId::new(0x123).unwrap();
        let f = <F as Frame>::new(Id::Standard(id), &[1, 2, 3]).unwrap();
        assert!(!f.is_extended());
        assert!(f.is_standard());
        assert_eq!(f.dlc(), 3);
        assert_eq!(f.data(), &[1, 2, 3]);
        assert_eq!(f.id(), Id::Standard(id));
    }

    #[test]
    fn extended_frame_roundtrip() {
        let id = ExtendedId::new(0x1abc_def0).unwrap();
        let f = <F as Frame>::new(Id::Extended(id), &[]).unwrap();
        assert!(f.is_extended());
        assert_eq!(f.dlc(), 0);
        assert_eq!(f.id(), Id::Extended(id));
    }

    #[test]
    fn remote_frame() {
        let id = StandardId::new(0x7ff).unwrap();
        let f = <F as Frame>::new_remote(Id::Standard(id), 4).unwrap();
        assert!(f.is_remote_frame());
        assert!(!f.is_data_frame());
        assert_eq!(f.dlc(), 4);
    }

    #[test]
    fn oversized_payload_is_rejected() {
        assert!(<F as Frame>::new(Id::Standard(StandardId::ZERO), &[0u8; 9]).is_none());
        assert!(<F as Frame>::new_remote(Id::Standard(StandardId::ZERO), 9).is_none());
    }
}
