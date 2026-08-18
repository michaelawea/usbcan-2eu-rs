//! Wire protocol: packet layouts, bit-timing table, pure pack/parse functions.
//!
//! This module performs **no I/O**. Every function here is a pure transformation
//! between CAN-level concepts and the bytes that travel over USB, which makes the
//! whole protocol testable without hardware attached. See [`crate::device`] for
//! the transport that actually moves these bytes.
//!
//! A prose description of the same protocol lives in `docs/protocol.md`.
//!
//! # Endpoint map
//!
//! | Endpoint | Type      | Direction | Purpose                                  |
//! |----------|-----------|-----------|------------------------------------------|
//! | `0x04`   | interrupt | OUT       | Control commands (fixed 64 bytes)        |
//! | `0x84`   | interrupt | IN        | Control responses (fixed 64 bytes)       |
//! | `0x02`   | bulk      | OUT       | CAN frames to transmit (>= 80 bytes)     |
//! | `0x82`   | bulk      | IN        | Transmit acknowledgement (5 bytes)       |
//! | `0x85`   | bulk      | IN        | Received CAN frames (24 bytes per frame) |
//! | `0x01`/`0x81` | interrupt | both | Shutdown handshake, unused by this driver |
//! | `0x03`/`0x83` | **isochronous** | both | **Never open these** — see `docs/macos-usb-isoc-kernel-bug.md` |
//!
//! Both CAN channels share the same endpoints; the channel is carried inside the
//! packets, not by endpoint selection. Received frames for channel 0 and channel 1
//! arrive interleaved on `0x85`.

use crate::error::Error;

// ───────────────────────────── Device identity ─────────────────────────────

/// USB vendor ID used by the USBCAN-(2)E-U family.
pub const VID: u16 = 0x0471;

/// Product ID of the dual-channel USBCAN-2E-U.
pub const PID_2E_U: u16 = 0x1261;

/// Product ID of the single-channel USBCAN-E-U.
///
/// Believed to speak the same protocol with `channel` restricted to 0, but the
/// maintainers have no such unit to verify against. See `docs/hardware-testing.md`.
pub const PID_E_U: u16 = 0x1260;

/// All product IDs this driver will match on.
pub const SUPPORTED_PIDS: &[u16] = &[PID_2E_U, PID_E_U];

// ───────────────────────────── USB endpoints ─────────────────────────────

/// Bulk OUT: CAN frames to be transmitted.
pub const EP_DATA_OUT: u8 = 0x02;
/// Bulk IN: 5-byte transmit acknowledgement.
pub const EP_TX_ACK_IN: u8 = 0x82;
/// Interrupt OUT: 64-byte control commands.
pub const EP_CTRL_OUT: u8 = 0x04;
/// Interrupt IN: 64-byte control responses.
pub const EP_CTRL_IN: u8 = 0x84;
/// Bulk IN: stream of received CAN frames.
pub const EP_DATA_IN: u8 = 0x85;

// ───────────────────────────── Control packets ─────────────────────────────

/// Control packets are always exactly one 64-byte USB packet.
pub const CTRL_PACKET_LEN: usize = 64;

/// First byte of every control packet, in both directions.
pub const CTRL_SOF: u8 = 0x7e;

/// Control command codes.
///
/// These are the commands this driver needs. The device accepts others that are
/// not exercised here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CtrlCmd {
    /// Status / presence probe. Payload: 1 byte.
    Probe = 0x01,
    /// Prepare a channel. Payload: 7 bytes, see [`init_channel_payload`].
    InitChannel = 0x02,
    /// Set bit timing and mode. Payload: 6 bytes, see [`set_bit_timing_payload`].
    SetBitTiming = 0x23,
    /// Start or stop a channel. Payload: 2 bytes, see [`start_payload`] / [`stop_payload`].
    StartStop = 0x24,
    /// Vendor "set reference" escape hatch. Payload: 1 or more bytes.
    SetReference = 0x25,
    /// Transmit-error recovery.
    TxErrRecover = 0x27,
    /// Read board status. Payload: none.
    GetStatus = 0x2c,
}

impl CtrlCmd {
    /// The raw command byte.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Build a 64-byte control packet.
///
/// ```text
/// byte 0         0x7e  start-of-frame
/// byte 1         command code
/// byte 2         payload length
/// byte 3         0x00  reserved
/// byte 4..4+L    payload
/// byte 4+L       XOR of bytes 0..(4+L)
/// remainder      zero padding to 64 bytes
/// ```
///
/// # Panics
///
/// Panics if `payload` cannot fit alongside the header and checksum.
pub fn pack_ctrl(cmd: u8, payload: &[u8]) -> [u8; CTRL_PACKET_LEN] {
    assert!(
        payload.len() <= CTRL_PACKET_LEN - 5,
        "control payload too large: {} bytes",
        payload.len()
    );
    let mut pkt = [0u8; CTRL_PACKET_LEN];
    pkt[0] = CTRL_SOF;
    pkt[1] = cmd;
    pkt[2] = payload.len() as u8;
    pkt[3] = 0x00;
    pkt[4..4 + payload.len()].copy_from_slice(payload);
    let mut xor = 0u8;
    for &b in &pkt[..4 + payload.len()] {
        xor ^= b;
    }
    pkt[4 + payload.len()] = xor;
    pkt
}

/// A decoded control response.
#[derive(Debug, Clone, Copy)]
pub struct CtrlResp {
    /// Echo of the command byte that was sent.
    pub cmd_echo: u8,
    /// Number of valid payload bytes.
    pub length: u8,
    /// Response payload, zero-padded.
    pub payload: [u8; 32],
}

impl CtrlResp {
    /// The valid part of [`Self::payload`].
    pub fn bytes(&self) -> &[u8] {
        &self.payload[..(self.length as usize).min(32)]
    }
}

/// Parse a control response and verify it belongs to `expected_cmd`.
///
/// The device signals failure by setting bit 7 of byte 2; the low 7 bits are the
/// payload length.
pub fn parse_ctrl_resp(buf: &[u8], expected_cmd: u8) -> Result<CtrlResp, Error> {
    if buf.len() < 4 {
        return Err(Error::Protocol("control response shorter than 4 bytes"));
    }
    if buf[0] != CTRL_SOF {
        return Err(Error::Protocol(
            "control response missing 0x7e start-of-frame",
        ));
    }
    if buf[1] != expected_cmd {
        return Err(Error::Protocol(
            "control response echoed a different command",
        ));
    }
    if buf[2] & 0x80 != 0 {
        return Err(Error::Rejected {
            cmd: expected_cmd,
            status: buf[2],
        });
    }
    let length = buf[2] & 0x7f;
    let mut payload = [0u8; 32];
    let copy_len = (length as usize).min(32).min(buf.len().saturating_sub(4));
    payload[..copy_len].copy_from_slice(&buf[4..4 + copy_len]);
    Ok(CtrlResp {
        cmd_echo: buf[1],
        length,
        payload,
    })
}

// ───────────────────────────── CAN frame ─────────────────────────────

/// Transmit packets are padded to at least this length.
pub const TX_PACKET_MIN_LEN: usize = 80;

/// First byte of a transmit packet header.
pub const TX_HEADER_MAGIC: u8 = 0x8e;

/// Length of the transmit packet header.
pub const TX_HEADER_LEN: usize = 8;

/// On-wire size of a single CAN frame, in both directions.
pub const FRAME_LEN: usize = 24;

/// Maximum frames the driver will place in one transmit packet.
pub const MAX_FRAMES_PER_PACKET: usize = 127;

/// A CAN 2.0A/2.0B frame, used for both transmit and receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CanFrame {
    /// Identifier. 11 bits when [`Self::extended`] is false, else 29 bits.
    pub id: u32,
    /// True for a 29-bit extended identifier.
    pub extended: bool,
    /// True for a remote-transmission-request frame (no payload).
    pub remote: bool,
    /// Data length code, `0..=8`.
    pub dlc: u8,
    /// Payload. Only the first [`Self::dlc`] bytes are meaningful.
    pub data: [u8; 8],
    /// Channel this frame belongs to, `0` or `1`.
    pub channel: u8,
    /// Device timestamp in microseconds. Filled on receive, ignored on transmit.
    pub timestamp_us: u32,
    /// Vendor "send type" byte. `0` is normal transmission.
    pub send_type: u8,
}

impl CanFrame {
    /// Build a data frame. `data` is truncated to 8 bytes.
    ///
    /// With the `embedded-can` feature on, this inherent constructor shadows
    /// `embedded_can::Frame::new`. Reach the trait version with
    /// `<CanFrame as Frame>::new(..)` when you need it.
    pub fn new(id: u32, extended: bool, data: &[u8]) -> Self {
        let dlc = data.len().min(8);
        let mut buf = [0u8; 8];
        buf[..dlc].copy_from_slice(&data[..dlc]);
        Self {
            id,
            extended,
            dlc: dlc as u8,
            data: buf,
            ..Default::default()
        }
    }

    /// Build a remote-request frame.
    pub fn new_remote(id: u32, extended: bool, dlc: u8) -> Self {
        Self {
            id,
            extended,
            remote: true,
            dlc: dlc.min(8),
            ..Default::default()
        }
    }

    /// The meaningful part of [`Self::data`].
    pub fn payload(&self) -> &[u8] {
        &self.data[..(self.dlc as usize).min(8)]
    }
}

impl core::fmt::Display for CanFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.extended {
            write!(f, "{:08X}", self.id)?;
        } else {
            write!(f, "{:03X}", self.id)?;
        }
        if self.remote {
            return write!(f, "#R{}", self.dlc);
        }
        write!(f, "#")?;
        for b in self.payload() {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// Build the 8-byte transmit packet header.
///
/// Note that `frame_count` is **big-endian** here, unlike every other multi-byte
/// field in this protocol.
pub fn pack_tx_header(
    frame_count: u16,
    channel: u8,
    started: bool,
    priority: u8,
) -> [u8; TX_HEADER_LEN] {
    let mut h = [0u8; TX_HEADER_LEN];
    h[0] = TX_HEADER_MAGIC;
    h[1] = (frame_count >> 8) as u8;
    h[2] = (frame_count & 0xff) as u8;
    h[3] = u8::from(started);
    h[4] = channel;
    h[5] = priority.max(1);
    h
}

/// Serialize one CAN frame into its 24-byte on-wire form.
///
/// ```text
/// byte 0          0x0f
/// byte 1          0x00 for a data frame
/// byte 2          send type
/// byte 3          bit7 extended | bit6 remote | bits5-4 channel | bits3-0 DLC
/// byte 4..8       timestamp, microseconds, little-endian (zero on transmit)
/// byte 8..12      identifier, little-endian
/// byte 12..20     payload
/// byte 12+DLC     XOR of bytes 0..(12+DLC)
/// remainder       zero padding to 24 bytes
/// ```
pub fn pack_frame(frame: &CanFrame) -> [u8; FRAME_LEN] {
    let mut f = [0u8; FRAME_LEN];
    f[0] = 0x0f;
    f[1] = 0x00;
    f[2] = frame.send_type;
    let dlc = (frame.dlc & 0x0f).min(8);
    f[3] = (u8::from(frame.extended) << 7)
        | (u8::from(frame.remote) << 6)
        | ((frame.channel & 0x03) << 4)
        | dlc;
    f[4..8].copy_from_slice(&frame.timestamp_us.to_le_bytes());
    let id = if frame.extended {
        frame.id & 0x1fff_ffff
    } else {
        frame.id & 0x7ff
    };
    f[8..12].copy_from_slice(&id.to_le_bytes());
    f[12..12 + dlc as usize].copy_from_slice(&frame.data[..dlc as usize]);
    let mut xor = 0u8;
    for &b in &f[..12 + dlc as usize] {
        xor ^= b;
    }
    f[12 + dlc as usize] = xor;
    f
}

/// Build a complete transmit packet: header, frames, padding.
///
/// A packet carries frames for exactly one channel, so `channel` overrides
/// [`CanFrame::channel`] on every frame. It appears in both the header and each
/// frame's flag byte; the device is sent both, matching the vendor driver.
///
/// # Panics
///
/// Panics if `frames` is empty or longer than [`MAX_FRAMES_PER_PACKET`].
pub fn pack_tx_packet(frames: &[CanFrame], channel: u8, started: bool) -> Vec<u8> {
    assert!(
        !frames.is_empty() && frames.len() <= MAX_FRAMES_PER_PACKET,
        "frame count must be 1..={MAX_FRAMES_PER_PACKET}"
    );
    let raw_len = TX_HEADER_LEN + frames.len() * FRAME_LEN;
    let mut buf = vec![0u8; raw_len.max(TX_PACKET_MIN_LEN)];
    buf[..TX_HEADER_LEN].copy_from_slice(&pack_tx_header(frames.len() as u16, channel, started, 1));
    for (i, frame) in frames.iter().enumerate() {
        let off = TX_HEADER_LEN + i * FRAME_LEN;
        let mut frame = *frame;
        frame.channel = channel;
        buf[off..off + FRAME_LEN].copy_from_slice(&pack_frame(&frame));
    }
    buf
}

// ───────────────────────────── Transmit acknowledgement ─────────────────────────────

/// The 5-byte reply read from [`EP_TX_ACK_IN`] after a transmit packet.
#[derive(Debug, Clone, Copy)]
pub struct TxAck {
    /// `0x0e` means the frames were accepted.
    pub status: u8,
    /// `0x81` for channel 0, `0x91` for channel 1.
    pub channel_marker: u8,
    /// Echo of the last frame's DLC.
    pub dlc_echo: u8,
}

impl TxAck {
    /// Whether the device accepted the transmission.
    pub fn is_success(&self) -> bool {
        self.status == 0x0e
    }
}

/// Parse a transmit acknowledgement.
pub fn parse_tx_ack(buf: &[u8]) -> Result<TxAck, Error> {
    if buf.len() < 5 {
        return Err(Error::Protocol("transmit ack shorter than 5 bytes"));
    }
    Ok(TxAck {
        status: buf[0],
        channel_marker: buf[1],
        dlc_echo: buf[2],
    })
}

// ───────────────────────────── Receive ─────────────────────────────

/// Everything decoded out of one read from [`EP_DATA_IN`].
#[derive(Debug, Default, Clone)]
pub struct RxChunk {
    /// Received CAN data frames, in arrival order, both channels interleaved.
    pub frames: Vec<CanFrame>,
    /// Bus status and error reports.
    pub status: Vec<RxStatus>,
    /// Records whose type byte was not recognized, kept verbatim for diagnosis.
    pub unknown: Vec<[u8; FRAME_LEN]>,
}

impl RxChunk {
    /// Whether nothing at all was decoded.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.status.is_empty() && self.unknown.is_empty()
    }

    /// Iterate only over frames belonging to `channel`.
    pub fn frames_on(&self, channel: u8) -> impl Iterator<Item = &CanFrame> {
        self.frames.iter().filter(move |f| f.channel == channel)
    }
}

/// Classification of a status record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RxStatusKind {
    /// `0xE1` — a queued transmission was aborted.
    TxAbort,
    /// `0xE2` — receive buffer overflowed; frames were lost.
    Overflow,
    /// `0xE3` — generic CAN controller error.
    CanError,
    /// `0xE4` — single-byte status report.
    Status,
    /// `0xE5` — bus error or error-passive.
    ///
    /// A steady stream of these normally means the controller is transmitting
    /// with nobody acknowledging: a lone node on the bus, a bit-rate mismatch,
    /// or missing bus termination.
    BusErrorOrPassive,
    /// `0xE6` — other vendor-defined status.
    Other,
}

/// A bus status or error record.
#[derive(Debug, Clone, Copy)]
pub struct RxStatus {
    /// What the record reports.
    pub kind: RxStatusKind,
    /// The full 24-byte record.
    pub raw: [u8; FRAME_LEN],
}

/// Split a receive chunk into frames and status records.
///
/// Records are a flat array of 24-byte entries. Byte 1 selects the kind: `0x00`
/// is a data frame, `0xE1..=0xE6` are status records. Byte 0 is not checked —
/// the vendor driver does not check it either, and data frames have been observed
/// with values other than `0x0f`.
///
/// Data frames whose channel field is `>= 2` are dropped, matching the vendor
/// driver's behaviour.
pub fn parse_rx_chunk(buf: &[u8]) -> RxChunk {
    let mut chunk = RxChunk::default();
    for i in 0..buf.len() / FRAME_LEN {
        let f = &buf[i * FRAME_LEN..(i + 1) * FRAME_LEN];
        match f[1] {
            0x00 => {
                let channel = (f[3] >> 4) & 0x03;
                if channel >= 2 {
                    continue;
                }
                let mut data = [0u8; 8];
                data.copy_from_slice(&f[12..20]);
                chunk.frames.push(CanFrame {
                    id: u32::from_le_bytes([f[8], f[9], f[10], f[11]]),
                    extended: f[3] & 0x80 != 0,
                    remote: f[3] & 0x40 != 0,
                    dlc: f[3] & 0x0f,
                    data,
                    channel,
                    timestamp_us: u32::from_le_bytes([f[4], f[5], f[6], f[7]]),
                    send_type: f[2],
                });
            }
            0xe1..=0xe6 => {
                let kind = match f[1] {
                    0xe1 => RxStatusKind::TxAbort,
                    0xe2 => RxStatusKind::Overflow,
                    0xe3 => RxStatusKind::CanError,
                    0xe4 => RxStatusKind::Status,
                    0xe5 => RxStatusKind::BusErrorOrPassive,
                    _ => RxStatusKind::Other,
                };
                let mut raw = [0u8; FRAME_LEN];
                raw.copy_from_slice(f);
                chunk.status.push(RxStatus { kind, raw });
            }
            _ => {
                let mut raw = [0u8; FRAME_LEN];
                raw.copy_from_slice(f);
                chunk.unknown.push(raw);
            }
        }
    }
    chunk
}

// ───────────────────────────── Command payload builders ─────────────────────────────

/// Payload for [`CtrlCmd::InitChannel`].
pub fn init_channel_payload(channel: u8) -> [u8; 7] {
    [0x01, channel + 1, 0x01, 0x00, 0x18, 0x00, 0x00]
}

/// Payload for [`CtrlCmd::SetBitTiming`].
///
/// `mode_flag` comes from [`ChannelMode::flag`]; `timing` from [`Bitrate::packed`].
pub fn set_bit_timing_payload(channel: u8, mode_flag: u8, timing: u32) -> [u8; 6] {
    let mut p = [0u8; 6];
    p[0] = channel + 1;
    p[1] = mode_flag;
    p[2..6].copy_from_slice(&timing.to_le_bytes());
    p
}

/// Payload for [`CtrlCmd::StartStop`] that starts a channel.
pub fn start_payload(channel: u8) -> [u8; 2] {
    [channel + 1, 0x80]
}

/// Payload for [`CtrlCmd::StartStop`] that stops a channel.
pub fn stop_payload(channel: u8) -> [u8; 2] {
    [channel + 1, 0x00]
}

// ───────────────────────────── Bit timing ─────────────────────────────

/// The CAN controller's time-quantum clock, in hertz.
///
/// This value is **empirically calibrated**, not taken from a datasheet. See the
/// note on [`Bitrate::packed`] for how it was established and why it matters.
pub const CAN_CLOCK_HZ: u32 = 36_000_000;

/// Supported CAN bit rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bitrate {
    /// 10 kbit/s.
    Kbps10,
    /// 20 kbit/s.
    Kbps20,
    /// 50 kbit/s.
    Kbps50,
    /// 100 kbit/s.
    Kbps100,
    /// 125 kbit/s.
    Kbps125,
    /// 250 kbit/s. The rate the timing table was calibrated against.
    Kbps250,
    /// 500 kbit/s.
    Kbps500,
    /// 800 kbit/s.
    Kbps800,
    /// 1 Mbit/s.
    Kbps1000,
}

impl Bitrate {
    /// Every supported rate, slowest first.
    pub const ALL: [Bitrate; 9] = [
        Bitrate::Kbps10,
        Bitrate::Kbps20,
        Bitrate::Kbps50,
        Bitrate::Kbps100,
        Bitrate::Kbps125,
        Bitrate::Kbps250,
        Bitrate::Kbps500,
        Bitrate::Kbps800,
        Bitrate::Kbps1000,
    ];

    /// The nominal rate in kbit/s.
    pub fn kbps(self) -> u32 {
        match self {
            Self::Kbps10 => 10,
            Self::Kbps20 => 20,
            Self::Kbps50 => 50,
            Self::Kbps100 => 100,
            Self::Kbps125 => 125,
            Self::Kbps250 => 250,
            Self::Kbps500 => 500,
            Self::Kbps800 => 800,
            Self::Kbps1000 => 1000,
        }
    }

    /// Look up a rate by its nominal kbit/s value.
    pub fn from_kbps(kbps: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.kbps() == kbps)
    }

    /// Bit-timing register value for [`CtrlCmd::SetBitTiming`].
    ///
    /// The 32-bit word packs three fields:
    ///
    /// ```text
    /// bits 0..9    BRP    prescaler minus one
    /// bits 16..19  TSEG1
    /// bits 20..22  TSEG2
    /// ```
    ///
    /// One bit lasts `(BRP + 1) * (TSEG1 + 3 + TSEG2)` clocks, and the firmware
    /// rejects any combination yielding 35 clocks or fewer.
    ///
    /// # On the 36 MHz figure
    ///
    /// The controller clock is not documented anywhere the maintainers could find,
    /// and an initial guess of 16 MHz produced a line rate 2.25x higher than
    /// nominal — traffic looked plausible but never decoded.
    ///
    /// It was pinned down by putting the adapter on a bus with known-good 250 kbit/s
    /// traffic between two third-party nodes, sweeping BRP in listen-only mode, and
    /// keeping the setting that decoded cleanly: `BRP=8, TSEG1=12, TSEG2=1`, i.e.
    /// 144 clocks per bit. 250 kbit/s x 144 = 36 MHz.
    ///
    /// This is corroborated by the firmware's own ">35 clocks" check: at 36 MHz,
    /// 1 Mbit/s needs exactly 36 clocks and just passes. At 16 MHz it would need 16
    /// and could never pass, yet the device does support 1 Mbit/s.
    ///
    /// Every entry in the table below divides 36 MHz exactly, so all nine rates are
    /// exact rather than approximated. This is enforced by a unit test.
    pub fn packed(self) -> u32 {
        // (BRP, TSEG1, TSEG2) chosen so that (BRP+1) * (TSEG1+3+TSEG2) == 36_000_000 / bps.
        // 16 tq/bit (TSEG1=12, TSEG2=1) is preferred; 18 or 15 tq are used where 16
        // does not divide evenly.
        match self {
            Self::Kbps1000 => pack(1, 12, 3), //   2 * 18 =   36 clocks
            Self::Kbps800 => pack(2, 10, 2),  //   3 * 15 =   45 clocks
            Self::Kbps500 => pack(3, 12, 3),  //   4 * 18 =   72 clocks
            Self::Kbps250 => pack(8, 12, 1),  //   9 * 16 =  144 clocks
            Self::Kbps125 => pack(17, 12, 1), //  18 * 16 =  288 clocks
            Self::Kbps100 => pack(19, 12, 3), //  20 * 18 =  360 clocks
            Self::Kbps50 => pack(44, 12, 1),  //  45 * 16 =  720 clocks
            Self::Kbps20 => pack(99, 12, 3),  // 100 * 18 = 1800 clocks
            Self::Kbps10 => pack(199, 12, 3), // 200 * 18 = 3600 clocks
        }
    }

    /// Decode a packed timing word back into `(BRP, TSEG1, TSEG2)`.
    pub fn unpack(word: u32) -> (u32, u32, u32) {
        (word & 0x3ff, (word >> 16) & 0xf, (word >> 20) & 0x7)
    }

    /// Clocks per bit implied by [`Self::packed`].
    pub fn clocks_per_bit(self) -> u32 {
        let (brp, tseg1, tseg2) = Self::unpack(self.packed());
        (brp + 1) * (tseg1 + 3 + tseg2)
    }
}

impl core::fmt::Display for Bitrate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} kbit/s", self.kbps())
    }
}

const fn pack(brp: u32, tseg1: u32, tseg2: u32) -> u32 {
    (brp & 0x3ff) | ((tseg1 & 0xf) << 16) | ((tseg2 & 0x7) << 20)
}

/// How a channel participates on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    /// Transmit and acknowledge normally.
    #[default]
    Normal,
    /// Do not drive the bus.
    ///
    /// The device distinguishes only "normal" from "not normal"; whether this
    /// yields true listen-only silence has not been confirmed with a bus analyzer.
    Passive,
}

impl ChannelMode {
    /// Mode byte for [`set_bit_timing_payload`].
    pub fn flag(self) -> u8 {
        match self {
            Self::Normal => 0x80,
            Self::Passive => 0x00,
        }
    }
}

// ───────────────────────────── Tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_packet_layout_and_checksum() {
        let pkt = pack_ctrl(0x01, &[0x01]);
        assert_eq!(pkt.len(), CTRL_PACKET_LEN);
        assert_eq!(&pkt[..5], &[0x7e, 0x01, 0x01, 0x00, 0x01]);
        // 0x7e ^ 0x01 ^ 0x01 ^ 0x00 ^ 0x01
        assert_eq!(pkt[5], 0x7f);
        assert_eq!(&pkt[6..], &[0u8; 58]);
    }

    #[test]
    fn ctrl_packet_init_channel() {
        let pkt = pack_ctrl(CtrlCmd::InitChannel.code(), &init_channel_payload(0));
        assert_eq!(
            &pkt[..11],
            &[0x7e, 0x02, 0x07, 0x00, 0x01, 0x01, 0x01, 0x00, 0x18, 0x00, 0x00]
        );
        let xor = pkt[..11].iter().fold(0u8, |a, b| a ^ b);
        assert_eq!(pkt[11], xor);
    }

    #[test]
    fn ctrl_response_accepted() {
        let mut buf = [0u8; CTRL_PACKET_LEN];
        buf[..7].copy_from_slice(&[0x7e, 0x01, 0x02, 0x00, 0x71, 0x04, 0x08]);
        let resp = parse_ctrl_resp(&buf, 0x01).unwrap();
        assert_eq!(resp.cmd_echo, 0x01);
        assert_eq!(resp.bytes(), &[0x71, 0x04]);
    }

    #[test]
    fn ctrl_response_rejected() {
        let buf = [0x7e, 0x02, 0x80, 0, 0, 0, 0, 0];
        assert!(matches!(
            parse_ctrl_resp(&buf, 0x02),
            Err(Error::Rejected { cmd: 0x02, .. })
        ));
    }

    #[test]
    fn ctrl_response_wrong_command_echo() {
        let buf = [0x7e, 0x24, 0x00, 0, 0, 0, 0, 0];
        assert!(parse_ctrl_resp(&buf, 0x02).is_err());
    }

    #[test]
    fn tx_header_frame_count_is_big_endian() {
        assert_eq!(
            pack_tx_header(1, 0, true, 1),
            [0x8e, 0x00, 0x01, 0x01, 0x00, 0x01, 0x00, 0x00]
        );
        let h = pack_tx_header(0x0123, 1, true, 1);
        assert_eq!((h[1], h[2]), (0x01, 0x23));
        assert_eq!(h[4], 1);
    }

    #[test]
    fn frame_pack_standard_id() {
        let f = pack_frame(&CanFrame::new(0x123, false, &[0xaa, 0xbb, 0xcc, 0xdd]));
        assert_eq!(&f[..4], &[0x0f, 0x00, 0x00, 0x04]);
        assert_eq!(&f[8..12], &[0x23, 0x01, 0x00, 0x00]);
        assert_eq!(&f[12..16], &[0xaa, 0xbb, 0xcc, 0xdd]);
        let xor = f[..16].iter().fold(0u8, |a, b| a ^ b);
        assert_eq!(f[16], xor);
        assert_eq!(f[16], 0x29);
    }

    #[test]
    fn frame_pack_extended_id() {
        let f = pack_frame(&CanFrame::new(0x1234_5678, true, &[0xa5; 8]));
        assert_eq!(f[3], 0x88);
        assert_eq!(&f[8..12], &[0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn frame_pack_remote() {
        let f = pack_frame(&CanFrame::new_remote(0x100, false, 8));
        assert_eq!(f[3], 0x48);
    }

    #[test]
    fn tx_packet_is_padded_to_minimum() {
        let pkt = pack_tx_packet(&[CanFrame::new(0x123, false, &[1, 2, 3, 4])], 0, true);
        assert_eq!(pkt.len(), TX_PACKET_MIN_LEN);
        assert_eq!(pkt[0], TX_HEADER_MAGIC);
        assert_eq!(&pkt[32..], &[0u8; 48]);
    }

    #[test]
    fn tx_packet_forces_the_channel_onto_every_frame() {
        let frames = [
            CanFrame::new(0x100, false, &[1]),
            CanFrame::new(0x200, false, &[2]),
        ];
        let pkt = pack_tx_packet(&frames, 1, true);
        assert_eq!(pkt[4], 1, "header channel");
        for i in 0..2 {
            let flags = pkt[TX_HEADER_LEN + i * FRAME_LEN + 3];
            assert_eq!((flags >> 4) & 0x03, 1, "frame {i} channel");
        }
    }

    #[test]
    fn tx_packet_grows_past_minimum() {
        let frames = vec![CanFrame::new(1, false, &[0]); 4];
        let pkt = pack_tx_packet(&frames, 0, true);
        assert_eq!(pkt.len(), TX_HEADER_LEN + 4 * FRAME_LEN);
    }

    #[test]
    fn tx_ack_success() {
        let ack = parse_tx_ack(&[0x0e, 0x81, 0x04, 0x00, 0x8b]).unwrap();
        assert!(ack.is_success());
        assert_eq!(ack.channel_marker, 0x81);
        assert_eq!(ack.dlc_echo, 0x04);
    }

    #[test]
    fn tx_ack_too_short() {
        assert!(parse_tx_ack(&[0x0e, 0x81]).is_err());
    }

    #[test]
    fn rx_chunk_decodes_status_record() {
        let raw: [u8; FRAME_LEN] = [
            0x0f, 0xe5, 0x03, 0x00, 0x86, 0xfe, 0xc1, 0x02, 0xa2, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let chunk = parse_rx_chunk(&raw);
        assert!(chunk.frames.is_empty());
        assert_eq!(chunk.status.len(), 1);
        assert_eq!(chunk.status[0].kind, RxStatusKind::BusErrorOrPassive);
    }

    #[test]
    fn rx_chunk_decodes_data_frame() {
        let mut f = [0u8; FRAME_LEN];
        f[0] = 0x0f;
        f[3] = 0x04;
        f[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        f[8..12].copy_from_slice(&0x123u32.to_le_bytes());
        f[12..16].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let chunk = parse_rx_chunk(&f);
        assert_eq!(chunk.frames.len(), 1);
        let frame = chunk.frames[0];
        assert_eq!(frame.id, 0x123);
        assert!(!frame.extended);
        assert_eq!(frame.payload(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(frame.timestamp_us, 0x1234_5678);
        assert_eq!(frame.channel, 0);
    }

    #[test]
    fn rx_chunk_ignores_byte0_and_drops_bad_channel() {
        let mut f = [0u8; FRAME_LEN * 2];
        // Record 0: data frame with an unexpected byte 0 - must still decode.
        f[0] = 0x00;
        f[3] = 0x11; // channel 1, dlc 1
                     // Record 1: channel 2 - must be dropped.
        f[FRAME_LEN] = 0x0f;
        f[FRAME_LEN + 3] = 0x21;
        let chunk = parse_rx_chunk(&f);
        assert_eq!(chunk.frames.len(), 1);
        assert_eq!(chunk.frames[0].channel, 1);
    }

    #[test]
    fn rx_chunk_keeps_unknown_records() {
        let mut f = [0u8; FRAME_LEN];
        f[1] = 0x77;
        let chunk = parse_rx_chunk(&f);
        assert_eq!(chunk.unknown.len(), 1);
    }

    #[test]
    fn rx_chunk_ignores_trailing_partial_record() {
        let chunk = parse_rx_chunk(&[0u8; FRAME_LEN + 5]);
        assert_eq!(chunk.frames.len(), 1);
    }

    #[test]
    fn frame_roundtrips_through_the_wire_format() {
        for frame in [
            CanFrame::new(0x7ff, false, &[1, 2, 3, 4, 5, 6, 7, 8]),
            CanFrame::new(0x1fff_ffff, true, &[0xaa]),
            CanFrame::new(0, false, &[]),
        ] {
            let decoded = parse_rx_chunk(&pack_frame(&frame)).frames[0];
            assert_eq!(decoded.id, frame.id);
            assert_eq!(decoded.extended, frame.extended);
            assert_eq!(decoded.dlc, frame.dlc);
            assert_eq!(decoded.payload(), frame.payload());
        }
    }

    #[test]
    fn every_bitrate_is_exact_at_36mhz_and_passes_firmware_check() {
        for b in Bitrate::ALL {
            let clocks = b.clocks_per_bit();
            assert_eq!(
                clocks * b.kbps(),
                CAN_CLOCK_HZ / 1000,
                "{b} is not exact: {clocks} clocks/bit"
            );
            assert!(clocks > 35, "{b} would be rejected by the firmware");
        }
    }

    #[test]
    fn bitrate_250k_matches_the_calibration_measurement() {
        assert_eq!(Bitrate::Kbps250.clocks_per_bit(), 144);
        assert_eq!(Bitrate::unpack(Bitrate::Kbps250.packed()), (8, 12, 1));
    }

    #[test]
    fn bitrate_lookup() {
        assert_eq!(Bitrate::from_kbps(500), Some(Bitrate::Kbps500));
        assert_eq!(Bitrate::from_kbps(333), None);
    }

    #[test]
    fn frame_display() {
        assert_eq!(
            CanFrame::new(0x123, false, &[0xde, 0xad]).to_string(),
            "123#DEAD"
        );
        assert_eq!(
            CanFrame::new(0x1abcdef0, true, &[]).to_string(),
            "1ABCDEF0#"
        );
        assert_eq!(CanFrame::new_remote(0x7ff, false, 4).to_string(), "7FF#R4");
    }
}
