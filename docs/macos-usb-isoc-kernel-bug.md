# Why this driver does not use libusb

Opening this adapter with libusb on macOS 26 crashes the machine. Not the
process — the machine. The kernel dereferences a null pointer inside
`IOUSBHostFamily.kext` and the SoC watchdog forces a hard reset a few seconds
later.

This document exists so nobody rediscovers that the hard way, and so the design
choice in [`src/device.rs`](../src/device.rs) is not mistaken for
over-engineering.

## What triggers it

All of these together:

| | |
|---|---|
| Operating system | macOS 26 (observed on 26.3.1 and 26.5) |
| Hardware | Apple Silicon |
| Device speed | Full speed, 12 Mbit/s |
| Device | Declares at least one isochronous endpoint |
| Call | `libusb_claim_interface()`, sometimes `libusb_open()` plus descriptor traversal |

The USBCAN-2E-U matches every row. It declares an isochronous pair at
`0x03`/`0x83` that it never actually uses.

## What it looks like

```text
Kernel data abort at far=0x0000000000000003
in com.apple.iokit.IOUSBHostFamily
```

The null-plus-three offset is the giveaway. No panic report is written, because
the machine does not survive long enough to write one. The only evidence left
behind is a reset counter file:

```bash
ls /Library/Logs/DiagnosticReports/ResetCounter-*.diag
sudo log show --predicate 'eventMessage CONTAINS "wdog"' --last 24h
```

Two things make this worse than a normal crash. First, an attached adapter can
retrigger it while the machine is idle, with no USB code of yours running —
during development this project saw a second watchdog reset minutes after the
first, with nothing running. Second, the recovery itself is disruptive: the run
that produced this document included a 2.1 GB burst of `coresymbolicationd` disk
writes between the two resets.

**If you hit this, unplug the adapter before doing anything else.**

## Upstream

- libusb issue [#1762](https://github.com/libusb/libusb/issues/1762) — open, no
  fix; the defect is on Apple's side.
- Reported independently against other full-speed devices with isochronous
  endpoints, so it is not specific to this adapter.

## The fix

libusb's macOS backend is built on the older `IOUSBLib` interface. Its
`USBInterfaceOpen()` initializes a pipe for **every** endpoint on the interface,
isochronous ones included, which is exactly the code path that faults.

`IOUSBHost`, Apple's modern userspace USB API, separates the two steps:

- `IOUSBHostInterface` claims the interface,
- `copyPipeWithAddress:` opens one pipe, by address, on request.

So this driver opens the pipes it needs by address and never names `0x03` or
`0x83`. The isochronous initialization path is never entered.

**Do not add code that opens those two endpoints,** and do not replace the
IOUSBHost transport with libusb, however much simpler the latter looks.

Rust bindings come from [`objc2-io-usb-host`](https://docs.rs/objc2-io-usb-host).

## Alternatives that were considered

**A DriverKit system extension.** Correct, and considerably more work: it needs an
Apple Developer account, entitlements, and signing. Not warranted while
IOUSBHost works.

**Different hardware.** Adapters that are USB 2.0 high speed, or that do not
declare isochronous endpoints, are unaffected. Not helpful if you already own
this one.

## Consequences for this driver

- The transport is macOS-specific. A Linux or Windows port would be a separate
  transport behind the same [`protocol`](../src/protocol.rs) module, which is
  deliberately platform-independent.
- Interrupt pipes cannot carry a completion timeout under IOUSBHost — passing a
  non-zero value returns `kIOReturnBadArgument`. Timed reads on `0x84` are done by
  issuing a blocking transfer and having a watchdog thread abort the pipe at the
  deadline. That is the reason for the extra machinery in `interrupt_in_timeout`.

## References

- [libusb #1762](https://github.com/libusb/libusb/issues/1762)
- [Apple Developer — IOUSBHostInterface](https://developer.apple.com/documentation/iousbhost/iousbhostinterface)
- [`objc2-io-usb-host`](https://docs.rs/objc2-io-usb-host)
