//! SLCAN (Lawicel ASCII) bridge over a pseudo-terminal.
//!
//! macOS has no SocketCAN, so tools that expect a CAN interface cannot see this
//! adapter directly. SLCAN is the lowest common denominator those tools do speak:
//! an ASCII line protocol over a serial port. This module allocates a pseudo-
//! terminal, speaks SLCAN on it, and forwards traffic to and from the adapter.
//!
//! ```text
//! python-can / cantools / SavvyCAN
//!            |  SLCAN over /dev/ttys004
//!       SlcanBridge
//!            |  USB
//!      USBCAN-2E-U
//! ```
//!
//! Start it with `usbcan2eu slcan --bitrate 500k`, note the printed device path,
//! then point a client at it:
//!
//! ```python
//! import can
//! bus = can.Bus(interface="slcan", channel="/dev/ttys004", bitrate=500000)
//! ```
//!
//! # Scope
//!
//! One bridge serves one CAN channel. Run a second bridge for the second channel.
//!
//! Supported commands: `S0`-`S8`, `O`, `L`, `C`, `t`, `T`, `r`, `R`, `V`, `v`,
//! `N`, `F`, `Z0`, `Z1`, `M`, `m`. `s` (raw BTR registers) is rejected, because
//! this hardware does not use SJA1000-style BTR values. Filter commands are
//! accepted and ignored: filtering happens on the host.

use std::io::{Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::device::Device;
use crate::error::Error;
use crate::protocol::{Bitrate, CanFrame, ChannelMode, RxStatusKind};

const CR: u8 = b'\r';
const BEL: u8 = 0x07;

/// SLCAN status bits, as defined by the Lawicel CANUSB command set.
mod status_bits {
    pub const ERROR_WARNING: u8 = 0x04;
    pub const DATA_OVERRUN: u8 = 0x08;
    pub const ERROR_PASSIVE: u8 = 0x20;
    pub const BUS_ERROR: u8 = 0x80;
}

/// A running SLCAN endpoint backed by a real adapter.
pub struct SlcanBridge {
    device: Device,
    channel: u8,
    master: std::fs::File,
    /// Held open so the pseudo-terminal survives a client disconnecting.
    _slave: std::fs::File,
    path: String,

    line: Vec<u8>,
    out: Vec<u8>,

    open: bool,
    bitrate: Bitrate,
    mode: ChannelMode,
    timestamps: bool,
    status: u8,
}

impl SlcanBridge {
    /// Allocate a pseudo-terminal and bind it to `channel` of `device`.
    ///
    /// The channel is not started until a client sends `O` or `L`.
    pub fn new(device: Device, channel: u8) -> Result<Self, Error> {
        let (master, slave, path) = open_pty()?;
        info!(path = %path, channel, "SLCAN bridge listening");
        Ok(Self {
            device,
            channel,
            master,
            _slave: slave,
            path,
            line: Vec::with_capacity(64),
            out: Vec::with_capacity(1024),
            open: false,
            bitrate: Bitrate::Kbps500,
            mode: ChannelMode::Normal,
            timestamps: false,
            status: 0,
        })
    }

    /// Path of the pseudo-terminal a client should connect to.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Preset the bit rate, as if the client had sent the matching `S` command.
    pub fn set_bitrate(&mut self, bitrate: Bitrate) {
        self.bitrate = bitrate;
    }

    /// Run until `should_stop` returns true.
    ///
    /// Returns on unrecoverable USB errors. A client disconnecting is not an
    /// error; the bridge keeps running and waits for the next one.
    pub fn run_until(&mut self, should_stop: impl Fn() -> bool) -> Result<(), Error> {
        while !should_stop() {
            self.pump_commands()?;
            if self.open {
                self.pump_frames()?;
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if self.open {
            let _ = self.device.stop_channel(self.channel);
        }
        Ok(())
    }

    /// Read whatever the client has sent and act on complete lines.
    fn pump_commands(&mut self) -> Result<(), Error> {
        let mut buf = [0u8; 512];
        loop {
            match read_nonblocking(&mut self.master, &mut buf)? {
                0 => break,
                n => {
                    for &byte in &buf[..n] {
                        match byte {
                            CR | b'\n' => {
                                let line = std::mem::take(&mut self.line);
                                let ok = self.handle_command(&line);
                                self.out.push(if ok { CR } else { BEL });
                            }
                            // Some clients send a lone BEL to resynchronize.
                            BEL => self.line.clear(),
                            _ => {
                                if self.line.len() < 64 {
                                    self.line.push(byte);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.flush()
    }

    /// Move one batch of received frames to the client.
    fn pump_frames(&mut self) -> Result<(), Error> {
        match self.device.receive(20) {
            Ok(chunk) => {
                for status in &chunk.status {
                    self.status |= match status.kind {
                        RxStatusKind::Overflow => status_bits::DATA_OVERRUN,
                        RxStatusKind::BusErrorOrPassive => {
                            status_bits::ERROR_PASSIVE | status_bits::BUS_ERROR
                        }
                        RxStatusKind::CanError => status_bits::ERROR_WARNING,
                        _ => 0,
                    };
                }
                let frames: Vec<CanFrame> = chunk.frames_on(self.channel).copied().collect();
                for frame in frames {
                    self.emit_frame(&frame);
                }
                self.flush()
            }
            Err(Error::Timeout { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn handle_command(&mut self, line: &[u8]) -> bool {
        if line.is_empty() {
            return true;
        }
        debug!(command = %String::from_utf8_lossy(line), "SLCAN command");
        match line[0] {
            b'S' => self.cmd_set_bitrate(&line[1..]),
            b's' => false, // raw BTR registers do not apply to this hardware
            b'O' => self.cmd_open(ChannelMode::Normal),
            b'L' => self.cmd_open(ChannelMode::Passive),
            b'C' => self.cmd_close(),
            b't' | b'T' | b'r' | b'R' => self.cmd_transmit(line),
            b'V' => self.reply(b"V1010"),
            b'v' => self.reply(b"v1010"),
            b'N' => self.cmd_serial(),
            b'F' => self.cmd_status(),
            b'Z' => self.cmd_timestamps(&line[1..]),
            // Acceptance filters are a host-side concern here.
            b'M' | b'm' => true,
            _ => false,
        }
    }

    fn cmd_set_bitrate(&mut self, arg: &[u8]) -> bool {
        if self.open {
            return false;
        }
        let Some(&digit) = arg.first() else {
            return false;
        };
        let bitrate = match digit {
            b'0' => Bitrate::Kbps10,
            b'1' => Bitrate::Kbps20,
            b'2' => Bitrate::Kbps50,
            b'3' => Bitrate::Kbps100,
            b'4' => Bitrate::Kbps125,
            b'5' => Bitrate::Kbps250,
            b'6' => Bitrate::Kbps500,
            b'7' => Bitrate::Kbps800,
            b'8' => Bitrate::Kbps1000,
            _ => return false,
        };
        self.bitrate = bitrate;
        true
    }

    fn cmd_open(&mut self, mode: ChannelMode) -> bool {
        if self.open {
            return false;
        }
        match self.device.start_channel(self.channel, self.bitrate, mode) {
            Ok(()) => {
                self.open = true;
                self.mode = mode;
                info!(channel = self.channel, bitrate = %self.bitrate, ?mode, "SLCAN channel open");
                true
            }
            Err(e) => {
                warn!(error = %e, "SLCAN open failed");
                false
            }
        }
    }

    fn cmd_close(&mut self) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        match self.device.stop_channel(self.channel) {
            Ok(()) => true,
            Err(e) => {
                warn!(error = %e, "SLCAN close failed");
                false
            }
        }
    }

    fn cmd_transmit(&mut self, line: &[u8]) -> bool {
        if !self.open {
            return false;
        }
        let Some(frame) = parse_slcan_frame(line) else {
            return false;
        };
        match self.device.transmit(self.channel, &[frame]) {
            Ok(ack) if ack.is_success() => true,
            Ok(_) => {
                self.status |= status_bits::BUS_ERROR;
                false
            }
            Err(e) => {
                warn!(error = %e, "SLCAN transmit failed");
                self.status |= status_bits::BUS_ERROR;
                false
            }
        }
    }

    fn cmd_serial(&mut self) -> bool {
        let id = self.device.identification();
        let mut serial = [b'0'; 4];
        for (i, byte) in id.iter().take(2).enumerate() {
            serial[i * 2] = hex_digit(byte >> 4);
            serial[i * 2 + 1] = hex_digit(byte & 0x0f);
        }
        self.out.push(b'N');
        self.out.extend_from_slice(&serial);
        true
    }

    fn cmd_status(&mut self) -> bool {
        let flags = std::mem::take(&mut self.status);
        self.out.push(b'F');
        self.out.push(hex_digit(flags >> 4));
        self.out.push(hex_digit(flags & 0x0f));
        true
    }

    fn cmd_timestamps(&mut self, arg: &[u8]) -> bool {
        match arg.first() {
            Some(b'0') => {
                self.timestamps = false;
                true
            }
            Some(b'1') => {
                self.timestamps = true;
                true
            }
            _ => false,
        }
    }

    fn reply(&mut self, bytes: &[u8]) -> bool {
        self.out.extend_from_slice(bytes);
        true
    }

    fn emit_frame(&mut self, frame: &CanFrame) {
        let tag = match (frame.extended, frame.remote) {
            (false, false) => b't',
            (true, false) => b'T',
            (false, true) => b'r',
            (true, true) => b'R',
        };
        self.out.push(tag);
        if frame.extended {
            push_hex(&mut self.out, frame.id, 8);
        } else {
            push_hex(&mut self.out, frame.id, 3);
        }
        self.out.push(hex_digit(frame.dlc.min(8)));
        if !frame.remote {
            for byte in frame.payload() {
                self.out.push(hex_digit(byte >> 4));
                self.out.push(hex_digit(byte & 0x0f));
            }
        }
        if self.timestamps {
            // SLCAN timestamps are milliseconds modulo 60000.
            push_hex(&mut self.out, (frame.timestamp_us / 1000) % 60_000, 4);
        }
        self.out.push(CR);
    }

    fn flush(&mut self) -> Result<(), Error> {
        if self.out.is_empty() {
            return Ok(());
        }
        // A client that has gone away must not take the bridge down with it.
        match self.master.write_all(&self.out) {
            Ok(()) => {}
            Err(e) if is_disconnected(&e) => debug!("SLCAN client not connected, output dropped"),
            Err(e) => return Err(Error::Io(e)),
        }
        self.out.clear();
        Ok(())
    }
}

// ───────────────────────── Frame text format ─────────────────────────

/// Parse `t`/`T`/`r`/`R` into a frame. Returns `None` on any malformed input.
fn parse_slcan_frame(line: &[u8]) -> Option<CanFrame> {
    let (extended, remote) = match line[0] {
        b't' => (false, false),
        b'T' => (true, false),
        b'r' => (false, true),
        b'R' => (true, true),
        _ => return None,
    };
    let id_len = if extended { 8 } else { 3 };
    if line.len() < 1 + id_len + 1 {
        return None;
    }
    let id = parse_hex(&line[1..1 + id_len])?;
    let dlc = hex_value(line[1 + id_len])?;
    if dlc > 8 {
        return None;
    }
    if remote {
        return Some(CanFrame::new_remote(id, extended, dlc));
    }
    let body = &line[2 + id_len..];
    if body.len() < dlc as usize * 2 {
        return None;
    }
    let mut data = [0u8; 8];
    for i in 0..dlc as usize {
        data[i] = (hex_value(body[i * 2])? << 4) | hex_value(body[i * 2 + 1])?;
    }
    Some(CanFrame::new(id, extended, &data[..dlc as usize]))
}

fn parse_hex(bytes: &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for &b in bytes {
        value = (value << 4) | u32::from(hex_value(b)?);
    }
    Some(value)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(nibble: u8) -> u8 {
    match nibble & 0x0f {
        n @ 0..=9 => b'0' + n,
        n => b'A' + n - 10,
    }
}

fn push_hex(out: &mut Vec<u8>, value: u32, digits: u32) {
    for shift in (0..digits).rev() {
        out.push(hex_digit(((value >> (shift * 4)) & 0xf) as u8));
    }
}

// ───────────────────────── Pseudo-terminal ─────────────────────────

/// Allocate a pseudo-terminal pair in raw mode.
///
/// The slave end is returned so the caller can hold it open: without it, macOS
/// reports EIO on the master as soon as the last client closes.
fn open_pty() -> Result<(std::fs::File, std::fs::File, String), Error> {
    // SAFETY: standard POSIX pseudo-terminal allocation. Each call is checked and
    // the descriptors are handed straight to File, which owns them from then on.
    unsafe {
        let master: RawFd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if master < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let master_file = std::fs::File::from_raw_fd(master);

        if libc::grantpt(master) < 0 || libc::unlockpt(master) < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }

        let name_ptr = libc::ptsname(master);
        if name_ptr.is_null() {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let path = std::ffi::CStr::from_ptr(name_ptr)
            .to_string_lossy()
            .into_owned();

        let c_path = std::ffi::CString::new(path.clone())
            .map_err(|_| Error::Init("pseudo-terminal path contains a NUL byte".into()))?;
        let slave: RawFd = libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if slave < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        let slave_file = std::fs::File::from_raw_fd(slave);

        // Raw mode: no echo, no line editing, no CR/LF translation. Without this
        // every byte written to the client comes straight back at us.
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut termios) == 0 {
            libc::cfmakeraw(&mut termios);
            if libc::tcsetattr(slave, libc::TCSANOW, &termios) < 0 {
                warn!("could not put the pseudo-terminal into raw mode");
            }
        }

        // Non-blocking master so the command pump never stalls the receive loop.
        let flags = libc::fcntl(master, libc::F_GETFL, 0);
        if flags < 0 || libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }

        Ok((master_file, slave_file, path))
    }
}

/// Read what is available, treating "nothing yet" and "no client" as zero bytes.
fn read_nonblocking(file: &mut std::fs::File, buf: &mut [u8]) -> Result<usize, Error> {
    match file.read(buf) {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
        Err(e) if is_disconnected(&e) => Ok(0),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Whether an error just means "no client is attached to the pseudo-terminal".
fn is_disconnected(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
    ) || e.raw_os_error() == Some(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_data_frame() {
        let f = parse_slcan_frame(b"t1238DEADBEEF01020304").unwrap();
        assert_eq!(f.id, 0x123);
        assert!(!f.extended);
        assert_eq!(f.dlc, 8);
        assert_eq!(
            f.payload(),
            &[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04]
        );
    }

    #[test]
    fn emitted_text_parses_back_to_the_same_frame() {
        for original in [
            CanFrame::new(0x123, false, &[0xde, 0xad]),
            CanFrame::new(0x1abc_def0, true, &[1, 2, 3, 4, 5, 6, 7, 8]),
            CanFrame::new(0x000, false, &[]),
            CanFrame::new_remote(0x7ff, false, 4),
            CanFrame::new_remote(0x1fff_ffff, true, 0),
        ] {
            let mut out = Vec::new();
            let tag = match (original.extended, original.remote) {
                (false, false) => b't',
                (true, false) => b'T',
                (false, true) => b'r',
                (true, true) => b'R',
            };
            out.push(tag);
            push_hex(&mut out, original.id, if original.extended { 8 } else { 3 });
            out.push(hex_digit(original.dlc));
            if !original.remote {
                for byte in original.payload() {
                    out.push(hex_digit(byte >> 4));
                    out.push(hex_digit(byte & 0x0f));
                }
            }
            let parsed = parse_slcan_frame(&out).expect("round trip");
            assert_eq!(parsed.id, original.id);
            assert_eq!(parsed.extended, original.extended);
            assert_eq!(parsed.remote, original.remote);
            assert_eq!(parsed.dlc, original.dlc);
            assert_eq!(parsed.payload(), original.payload());
        }
    }

    #[test]
    fn parse_standard_frame_with_exact_payload() {
        let f = parse_slcan_frame(b"t1232AABB").unwrap();
        assert_eq!(f.id, 0x123);
        assert_eq!(f.payload(), &[0xaa, 0xbb]);
    }

    #[test]
    fn parse_extended_frame() {
        let f = parse_slcan_frame(b"T1ABCDEF0100").unwrap();
        assert_eq!(f.id, 0x1abc_def0);
        assert!(f.extended);
        assert_eq!(f.dlc, 1);
        assert_eq!(f.payload(), &[0x00]);
    }

    #[test]
    fn parse_remote_frames() {
        let f = parse_slcan_frame(b"r1234").unwrap();
        assert!(f.remote);
        assert_eq!(f.dlc, 4);
        let f = parse_slcan_frame(b"R1FFFFFFF8").unwrap();
        assert!(f.remote && f.extended);
        assert_eq!(f.dlc, 8);
    }

    #[test]
    fn reject_malformed_input() {
        assert!(parse_slcan_frame(b"t12").is_none()); // truncated
        assert!(parse_slcan_frame(b"t1239").is_none()); // dlc 9
        assert!(parse_slcan_frame(b"t1232AA").is_none()); // payload short
        assert!(parse_slcan_frame(b"tZZZ0").is_none()); // bad hex
        assert!(parse_slcan_frame(b"x1230").is_none()); // unknown tag
    }

    #[test]
    fn hex_helpers() {
        let mut out = Vec::new();
        push_hex(&mut out, 0x1ab, 3);
        assert_eq!(out, b"1AB");
        out.clear();
        push_hex(&mut out, 0x1abc_def0, 8);
        assert_eq!(out, b"1ABCDEF0");
    }
}
