# Contributing

Thanks for looking. This is a small driver for hardware most people do not have,
so almost any contribution is welcome — including ones that report that something
does not work.

## Contributing without the hardware

Most of this project can be worked on with no adapter attached:

- The [`protocol`](src/protocol.rs) module is pure functions. `cargo test` runs
  its full suite anywhere, including on Linux and Windows.
- The [SLCAN](src/slcan.rs) command parser and frame formatter are likewise
  testable in isolation.
- Documentation, error messages, and the CLI's output all benefit from someone
  who is reading them for the first time.

A port to another platform would live behind the same `protocol` module: it is
deliberately free of platform code so a Linux or Windows transport can reuse it
unchanged.

## Contributing with the hardware

The most useful thing is a **device report** from a model other than the
USBCAN-2E-U. See [`docs/hardware-testing.md`](docs/hardware-testing.md) for what
to include. Reports that the device does *not* work are just as useful as ones
that say it does.

Before submitting a change that touches the transport or protocol:

```bash
cargo test --all-features
cargo test --test hardware -- --ignored --test-threads=1
usbcan2eu selftest
```

Paste the self-test output into the pull request. A change to the protocol layer
that has not been run against hardware should say so plainly rather than imply
otherwise.

## Standards for protocol changes

This driver talks to hardware whose protocol is not documented by its vendor. That
puts a particular weight on how claims are recorded:

- **State what was observed, not what was assumed.** If you measured it, say what
  you measured and how. If you inferred it, mark it as inferred.
- **Add a fixture.** New decoding behaviour should come with bytes from a real
  device under [`tests/fixtures/`](tests/fixtures), including a header describing
  what was on the bus at the time.
- **Do not add vendor material.** No vendor binaries, headers, firmware,
  documentation, or decompiler output belongs in this repository, in any form,
  including inside comments. Describe behaviour instead.
- **Never open endpoints `0x03` or `0x83`.** See
  [`docs/macos-usb-isoc-kernel-bug.md`](docs/macos-usb-isoc-kernel-bug.md). This
  is not a style preference; it crashes the machine.

## Before opening a pull request

```bash
cargo fmt
cargo clippy --all-features --all-targets -- -D warnings
cargo test --all-features
```

CI runs the same three commands on macOS and Linux.

Commits are easier to review when each does one thing. There is no required
commit message format.

## Reporting a bug

Include:

- `usbcan2eu info` output in full,
- the failing command with `-vv`,
- `usbcan2eu selftest` output if you have a two-channel unit,
- your macOS version and machine type.

[`docs/troubleshooting.md`](docs/troubleshooting.md) covers the common cases and
may save you the trip.

## License

Contributions are dual-licensed under Apache-2.0 and MIT, matching the project.
By submitting a contribution you agree to license it that way, with no additional
terms.
