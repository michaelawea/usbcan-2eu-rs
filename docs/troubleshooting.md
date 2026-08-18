# Troubleshooting

Run `usbcan2eu info` first. If that fails, nothing further will work.

Adding `-v` (info), `-vv` (debug) or `-vvv` (trace) to any command turns on driver
logging, which is usually enough to see which step failed.

## Nothing is found

**`No USBCAN-(2)E-U adapter found`**

- Confirm macOS sees it at all: `system_profiler SPUSBDataType | grep -i -A 5 0471`.
  No match means the problem is the cable, the port, or the adapter.
- Only vendor `0x0471`, products `0x1260` and `0x1261` are matched. Other models in
  the vendor's range speak different protocols and are not supported.
- USB hubs, particularly unpowered ones, are worth eliminating.

**`the device exposes no USB interface; unplug and reattach it`**

The device is enumerated but IOKit has not published its interface. Unplug it,
wait a couple of seconds, plug it back in. If it persists after a reattach,
another process may be holding the device.

## It opens but nothing arrives

**`Error::Timeout` on every receive**

Timeout means the bus is quiet, which is a statement about the bus rather than an
error. Work through it in this order:

1. Add `--status`. A stream of `BusErrorOrPassive` records means the adapter is
   transmitting and nothing is acknowledging — see below. No records at all means
   nothing is reaching the controller.
2. Check the bit rate. A mismatch produces exactly this symptom. If you do not
   know the bus rate, try each one with `--listen-only`.
3. Confirm something else is actually transmitting.

**A continuous stream of `BusErrorOrPassive` (`0xE5`)**

The controller is transmitting and no other node is acknowledging it. CAN requires
a second node to acknowledge every frame; a lone adapter on a bus will always
report this. Either there is genuinely nothing else on the bus, the bit rate is
wrong, or the physical layer is not carrying the signal.

**Frames arrive but the payloads are wrong**

Suspect the bit timing table. Run `usbcan2eu selftest` first — if the self test
passes, the table is internally consistent and the problem is more likely a bit
rate mismatch with the other side. See
[`protocol.md`](protocol.md#bit-timing).

## Transmit problems

**`device rejected command 0x24`**

The channel was not initialized, or the device is in a state the driver did not
expect. `start_channel` normally handles this by stopping first; if it persists,
unplug and reattach to force re-enumeration, which is the only thing that fully
clears firmware channel state.

**The acknowledgement never arrives**

`Error::Timeout` from `transmit` means the adapter did not answer, which is a USB
problem rather than a bus problem. Try a different port, then reattach.

**The acknowledgement says the frame was rejected**

The adapter refused to queue it. Usually the channel is not started, or it has
gone bus-off after too many errors. Stop and start the channel to recover.

## The machine reboots

If your Mac hard-reboots while you are using a USB CAN adapter, read
[`macos-usb-isoc-kernel-bug.md`](macos-usb-isoc-kernel-bug.md) before anything
else, and unplug the adapter now.

This driver avoids the trigger. Other software on the same machine may not: if
you have libusb-based CAN tooling installed, it is a candidate.

## SLCAN problems

**The client cannot open the device**

The path changes each run — `/dev/ttys004`, `/dev/ttys005` and so on. Use the one
the bridge prints.

**The client connects but sees no traffic**

python-can's `slcan` backend opens the channel itself. If you drive the port by
hand, remember the sequence: `S6\r` to set the bit rate, then `O\r` to open. No
frames flow before `O`.

**Commands come back as `\x07`**

That is SLCAN for "rejected". Common causes: setting the bit rate while the
channel is open (send `C` first), an unsupported rate, or malformed frame text.
Run the bridge with `-vv` to see each command as it arrives.

## Nothing here matches

Open an issue with:

- `usbcan2eu info` output in full,
- `usbcan2eu selftest` output if you have a two-channel unit,
- the failing command with `-vv`,
- your macOS version and machine type.
