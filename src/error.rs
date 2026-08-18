//! Error type for all driver operations.

use thiserror::Error;

/// Everything that can go wrong talking to a USBCAN-(2)E-U.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The underlying USB transport reported a failure.
    #[error("USB error ({code}): {message}")]
    Usb {
        /// `IOReturn` value reported by the kernel.
        code: i32,
        /// Human-readable description, including which operation failed.
        message: String,
    },

    /// No device with a matching VID/PID is attached.
    #[error("device not found (VID=0x{vid:04x} PID=0x{pid:04x})")]
    DeviceNotFound {
        /// USB vendor ID that was searched for.
        vid: u16,
        /// USB product ID that was searched for.
        pid: u16,
    },

    /// The device is present but does not expose an endpoint we need.
    #[error("required endpoint 0x{addr:02x} not present on device")]
    MissingEndpoint {
        /// The endpoint address that could not be opened.
        addr: u8,
    },

    /// Channel index outside `0..=1`.
    #[error("invalid channel index {0} (must be 0 or 1)")]
    InvalidChannel(u8),

    /// Requested bitrate is not in the supported table.
    #[error("unsupported bitrate: {0} kbit/s")]
    UnsupportedBitrate(u32),

    /// A transfer did not complete within the requested time.
    #[error("operation timed out after {ms} ms")]
    Timeout {
        /// How long the operation waited, in milliseconds.
        ms: u32,
    },

    /// Device setup / enumeration failed.
    #[error("initialization failure: {0}")]
    Init(String),

    /// The device replied, but the reply did not follow the wire protocol.
    #[error("protocol error: {0}")]
    Protocol(&'static str),

    /// The device explicitly rejected a command.
    #[error("device rejected command 0x{cmd:02x} (status byte 0x{status:02x})")]
    Rejected {
        /// The command the device refused.
        cmd: u8,
        /// The status byte it replied with; bit 7 marks the refusal.
        status: u8,
    },

    /// An operating-system call failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[cfg(feature = "embedded-can")]
impl embedded_can::Error for Error {
    fn kind(&self) -> embedded_can::ErrorKind {
        embedded_can::ErrorKind::Other
    }
}
