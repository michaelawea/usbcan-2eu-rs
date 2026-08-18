# Capture fixtures

Bytes recorded from a real adapter, used by `tests/wire_format.rs` so the decoders
can be exercised on a machine with no hardware attached.

Each file is hexadecimal, whitespace-insensitive; `#` starts a comment. Add the
capture conditions in the header when you contribute one — a fixture whose origin
is unknown is worth very little.

To record more, run `usbcan2eu dump --status -vv` and copy the byte dumps, or call
`Device::receive_raw` from a small program of your own.
