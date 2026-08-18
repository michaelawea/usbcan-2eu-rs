# Testing with hardware attached

`cargo test` passes with no adapter connected: the protocol layer is pure
functions and the decoders are exercised against captures in
[`tests/fixtures/`](../tests/fixtures). Everything that needs a device is marked
`#[ignore]` and has to be asked for.

## Running the hardware tests

```bash
cargo test --test hardware -- --ignored --test-threads=1
```

`--test-threads=1` is required. The tests open the same adapter, and two of them
holding it at once fails for reasons unrelated to the code.

| Test | Wiring needed |
|---|---|
| `adapter_is_present_and_answers` | Adapter attached |
| `channel_starts_and_stops` | Adapter attached |
| `channel_restart_is_idempotent` | Adapter attached |
| `frames_cross_from_channel_0_to_channel_1` | Channel 0 wired to channel 1 |
| `extended_identifiers_survive_the_round_trip` | Channel 0 wired to channel 1 |
| `a_burst_arrives_complete_and_in_order` | Channel 0 wired to channel 1 |

To wire the two channels together: CANH to CANH, CANL to CANL, 120 Ω across the
pair at each end. Nothing else needs to be on the bus.

## The self test

`usbcan2eu selftest` covers the same ground as the loopback tests and prints a
readable report. It is the right thing to run first, and the right thing to
attach to a bug report.

```bash
usbcan2eu selftest --bitrate 500k --frames 100
```

Run it at more than one bit rate when validating a new unit or a firmware you have
not seen before — a timing table that is wrong for one rate can be right for
another.

## Testing on a live bus

`--listen-only` starts a channel without driving the bus:

```bash
usbcan2eu dump --bitrate 250k --listen-only --status
```

This is the safe way to check that the bit rate matches an existing bus before
transmitting on it. Note that listen-only has not been confirmed to be true
electrical silence with an analyzer; treat it as a strong convention, not a
guarantee, on equipment where an unexpected frame would matter.

## Reporting an untested model

Only the USBCAN-2E-U has been verified. If you have anything else in the family —
particularly the single-channel USBCAN-E-U (`0x1260`) — a report is valuable
whether or not it works.

Please include:

1. `usbcan2eu info` output in full, especially the identification value and the
   endpoint table.
2. `usbcan2eu selftest` output, if the unit has two channels.
3. `usbcan2eu dump --bitrate <rate> --status -vv` against a bus you know is
   running, if you have one.
4. Your macOS version and whether the machine is Apple Silicon or Intel.

An endpoint table that differs from the one in
[`protocol.md`](protocol.md#endpoints) is the most likely place a new model
diverges, and it is the first thing worth comparing.

## Contributing a capture

Fixtures under [`tests/fixtures/`](../tests/fixtures) are hexadecimal text with
`#` comments. Recording a new one is worthwhile whenever you see bytes the
decoders mishandle:

```bash
usbcan2eu dump --status -vv 2>&1 | tee capture.log
```

or, for raw undecoded bytes, call `Device::receive_raw` from a short program.

Say in the file header what was on the bus and what the adapter was doing. A
capture with no context is hard to use.
