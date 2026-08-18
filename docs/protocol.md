# USBCAN-(2)E-U wire protocol

Everything a host needs to talk to a ZLG USBCAN-E-U (`0x0471:0x1260`) or
USBCAN-2E-U (`0x0471:0x1261`) over USB, written so the adapter can be
reimplemented in another language without reading the Rust.

The reference implementation is [`src/protocol.rs`](../src/protocol.rs), which is
pure functions with no I/O, and its unit tests double as executable examples of
every layout described here.

## Provenance

This description was assembled by studying the vendor's Windows driver stack and
then verifying each conclusion against a real USBCAN-2E-U. Everything below is
observable behaviour: put a USB analyzer between a host and the adapter and you
will see these bytes.

Statements that are inferred rather than measured are marked as such. The most
important one is the controller clock, discussed under [Bit timing](#bit-timing).

Firmware revisions may differ. `usbcan2eu info` prints an identification value
that can be used to tell them apart.

## Device identity

| | |
|---|---|
| USB vendor ID | `0x0471` |
| Product ID | `0x1261` (USBCAN-2E-U, two channels) |
| Product ID | `0x1260` (USBCAN-E-U, one channel) |
| USB version | 1.1, full speed (12 Mbit/s) |
| Configurations | 1 |
| Interfaces | 1 (`bInterfaceNumber` 0) |

## Endpoints

| Address | Type | Direction | Purpose |
|---|---|---|---|
| `0x01` | interrupt | OUT | Shutdown handshake. Unused by this driver. |
| `0x81` | interrupt | IN | Shutdown handshake. Unused by this driver. |
| `0x02` | bulk | OUT | CAN frames to transmit |
| `0x82` | bulk | IN | Transmit acknowledgement |
| `0x03` | isochronous | OUT | **Never open. See below.** |
| `0x83` | isochronous | IN | **Never open. See below.** |
| `0x04` | interrupt | OUT | Control commands |
| `0x84` | interrupt | IN | Control responses |
| `0x05` | bulk | OUT | Unused by this driver |
| `0x85` | bulk | IN | Received CAN frames |

Both CAN channels share one set of endpoints. The channel is carried inside the
packets. Frames for channel 0 and channel 1 arrive interleaved on `0x85`.

### The isochronous endpoints

The adapter declares an isochronous pair that it never uses. On macOS 26,
touching them crashes the kernel — see
[`macos-usb-isoc-kernel-bug.md`](macos-usb-isoc-kernel-bug.md). Any host
implementation should open pipes by address, one at a time, and skip `0x03` and
`0x83`. Interfaces that open every endpoint at once (which is what libusb's
`claim_interface` does on macOS) are not safe with this hardware.

### Opening the other endpoints

The vendor's userspace library opens every non-isochronous endpoint before it
does anything else. This driver does the same, including `0x01`, `0x81` and
`0x05` which it never reads from or writes to. Whether the firmware requires this
has not been isolated; it is cheap insurance against a class of "the device just
sits there" failures.

## Control packets

Fixed 64 bytes in both directions, matching the endpoint's maximum packet size.

```text
offset  size  field
0       1     0x7e   start of frame
1       1     command code
2       1     payload length          (response: bit 7 = failure, bits 0-6 = length)
3       1     0x00   reserved
4       L     payload
4+L     1     XOR of bytes 0 .. 4+L (exclusive)
5+L     ..    zero padding to 64 bytes
```

The checksum covers the header and payload but not itself. Padding is not
included.

A response is valid when byte 0 is `0x7e` and byte 1 echoes the command that was
sent. Bit 7 of byte 2 marks refusal; a refused command has no useful payload.

### Command codes

| Code | Name | Payload | Meaning |
|---|---|---|---|
| `0x01` | Probe | `[0x01]` | Identification. Responds with 2 bytes. |
| `0x02` | Init channel | 7 bytes, below | Prepare a channel |
| `0x23` | Set bit timing | 6 bytes, below | Bit rate and mode |
| `0x24` | Start / stop | 2 bytes, below | Put a channel on or off the bus |
| `0x25` | Set reference | 1+ bytes | Vendor escape hatch, not used here |
| `0x27` | Transmit error recovery | — | Not used here |
| `0x2c` | Get status | — | Board status |

### Command payloads

Channels are numbered from 0 in this document and in the API, but the wire format
uses `channel + 1`.

**`0x02` init channel**

```text
[0x01, channel + 1, 0x01, 0x00, 0x18, 0x00, 0x00]
```

The constants are the values the vendor driver sends; their individual meanings
were not established. It works, and varying them was not attempted on hardware.

**`0x23` set bit timing**

```text
[channel + 1, mode, timing[0], timing[1], timing[2], timing[3]]
```

- `mode` is `0x80` for normal operation, `0x00` otherwise. The firmware appears
  to distinguish only these two cases.
- `timing` is the 32-bit little-endian word described under
  [Bit timing](#bit-timing).

**`0x24` start / stop**

```text
[channel + 1, 0x80]   start
[channel + 1, 0x00]   stop
```

### Startup sequence

```text
0x24 [ch+1, 0x00]   stop
0x02 [...]          init channel
0x23 [...]          set bit timing
0x24 [ch+1, 0x80]   start
```

The leading stop matters. **Channel state survives your process**: a channel
initialized by an earlier run stays initialized until the device is
re-enumerated, and `0x02` then answers with the failure bit set. Sending stop
first clears that. If `0x02` still reports failure, treat it as "already
initialized" and continue — the remaining two commands are applied regardless, so
the channel ends up in the requested configuration either way.

## CAN frames

The same 24-byte record is used for transmit and receive.

```text
offset  size  field
0       1     0x0f
1       1     0x00 for a data frame; 0xE1..0xE6 marks a status record
2       1     send type (0 = normal). Echoed back on receive.
3       1     bit 7    extended identifier
              bit 6    remote request
              bits 5-4 channel
              bits 3-0 DLC
4       4     timestamp, microseconds, little-endian. Zero on transmit.
8       4     identifier, little-endian
12      8     payload
12+DLC  1     XOR of bytes 0 .. 12+DLC (exclusive)
..      ..    zero padding to 24 bytes
```

Note the checksum position moves with the DLC and overwrites a payload byte
position that the DLC says is unused.

Identifiers are masked to 11 bits for standard frames and 29 bits for extended
frames.

**Byte 0 is not a reliable marker on receive.** The vendor driver does not check
it, and this implementation does not either. Dispatch on byte 1.

## Transmitting

A transmit packet is a header followed by frame records, written to `0x02`.

```text
offset  size  field
0       1     0x8e
1       2     frame count, BIG-endian
3       1     1 if the channel is running
4       1     channel
5       1     priority, minimum 1
6       2     0x0000
8       ..    frame records, 24 bytes each
```

The big-endian frame count is the one place in this protocol where a multi-byte
field is not little-endian.

Packets shorter than 80 bytes are padded to 80 with zeros. Multiple frames in one
packet is much faster than one packet per frame.

### Transmit acknowledgement

After each packet, read 5 bytes from `0x82`:

```text
offset  size  field
0       1     0x0e on success
1       1     0x81 for channel 0, 0x91 for channel 1
2       1     DLC echo
3       1     0x00
4       1     status / checksum
```

The acknowledgement means the adapter accepted the frames. It says nothing about
whether any node on the bus received them.

## Receiving

Read from `0x85`. The reply is a flat array of 24-byte records, up to 6144 bytes
per transfer. Byte 1 of each record selects the kind.

| Byte 1 | Record |
|---|---|
| `0x00` | CAN data frame, layout above |
| `0xE1` | A queued transmission was aborted |
| `0xE2` | Receive buffer overflow; frames were lost |
| `0xE3` | Generic CAN controller error |
| `0xE4` | Single-byte status report |
| `0xE5` | Bus error or error-passive |
| `0xE6` | Other vendor status |

Data frames whose channel field is 2 or greater should be discarded; the vendor
driver does the same.

An idle bus produces a timeout rather than an empty transfer. That is the normal
quiet state.

### Reading `0xE5`

A steady stream of `0xE5` records almost always means the controller is
transmitting and nothing is acknowledging it. In practice that is one of:

- the adapter is the only node on the bus,
- the bit rate does not match the rest of the bus,
- bus termination is missing.

`0xE5` is the single most informative record when bringing up a new setup, which
is why `usbcan2eu dump --status` exists.

## Bit timing

`0x23` takes a 32-bit little-endian word:

```text
bits 0-9    BRP, prescaler minus one
bits 16-19  TSEG1
bits 20-22  TSEG2
```

One bit lasts

```text
(BRP + 1) * (TSEG1 + 3 + TSEG2)
```

clocks. The firmware rejects any combination of 35 clocks or fewer.

### The controller clock is 36 MHz

This is the one number in the protocol that had to be established experimentally,
and getting it wrong is expensive: an incorrect clock produces a line rate that is
wrong by a constant factor, so traffic looks plausible on a scope but never
decodes, and every symptom points at the wrong place.

An initial assumption of 16 MHz — a common value for CAN controllers — put the
real line rate 2.25x above nominal.

It was pinned down as follows:

1. Two third-party nodes were put on a bus running known-good 250 kbit/s traffic.
2. The adapter joined as a passive third node.
3. BRP was swept across its range while watching which setting decoded the traffic
   cleanly.
4. Exactly one did: `BRP=8, TSEG1=12, TSEG2=1`, which is 144 clocks per bit.
5. 250 kbit/s × 144 = 36 MHz.

Corroboration: the firmware's own ">35 clocks" check. At 36 MHz, 1 Mbit/s needs
exactly 36 clocks and just passes. At 16 MHz it would need 16 and could never
pass, yet the device does support 1 Mbit/s.

### Table

Every entry divides 36 MHz exactly, so all nine rates are exact rather than
approximated. A unit test enforces this.

| Bit rate | BRP | TSEG1 | TSEG2 | Clocks/bit | tq/bit |
|---|---|---|---|---|---|
| 1000 kbit/s | 1 | 12 | 3 | 36 | 18 |
| 800 kbit/s | 2 | 10 | 2 | 45 | 15 |
| 500 kbit/s | 3 | 12 | 3 | 72 | 18 |
| 250 kbit/s | 8 | 12 | 1 | 144 | 16 |
| 125 kbit/s | 17 | 12 | 1 | 288 | 16 |
| 100 kbit/s | 19 | 12 | 3 | 360 | 18 |
| 50 kbit/s | 44 | 12 | 1 | 720 | 16 |
| 20 kbit/s | 99 | 12 | 3 | 1800 | 18 |
| 10 kbit/s | 199 | 12 | 3 | 3600 | 18 |

The 250 kbit/s row is the measured one. The rest follow from the same clock.

Sample points were not optimized; 16 tq per bit is used wherever it divides
evenly, and 15 or 18 elsewhere. This has been adequate in practice, but a bus with
long propagation delays might want different TSEG1/TSEG2 splits at the same total.

## What is not implemented

The vendor's library exposes more than this driver uses. None of the following
have been characterized:

- Hardware acceptance filters. Filtering here happens on the host.
- Scheduled or periodic transmission driven by the adapter itself.
- Software-controlled bus termination.
- Serial-number read and write.
- Merged-receive mode.
- The `0x01`/`0x81` shutdown handshake. This driver simply closes its pipes.

Contributions covering any of these are welcome, provided they come with hardware
verification.
