//! Tests that need an adapter attached.
//!
//! They are `#[ignore]` so `cargo test` stays green without hardware. Run them
//! deliberately:
//!
//! ```text
//! cargo test --test hardware -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: these open the same adapter, and two tests holding
//! it at once will fail for reasons that have nothing to do with the code.
//!
//! See `docs/hardware-testing.md` for the wiring each test expects.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device, Error};

const BITRATE: Bitrate = Bitrate::Kbps500;

/// No wiring required.
#[test]
#[ignore = "needs an adapter attached"]
fn adapter_is_present_and_answers() {
    let adapters = Device::list().expect("enumeration should not fail");
    assert!(!adapters.is_empty(), "no adapter found");

    let device = Device::open_first().expect("adapter should open");
    assert!(device.channel_count() >= 1);

    let endpoints = device.endpoints();
    assert!(
        endpoints.iter().any(|e| e.address == 0x85),
        "the receive endpoint is missing; this may not be a supported model"
    );
    assert!(
        endpoints.iter().any(|e| e.transfer_type == 1),
        "expected the unused isochronous endpoints to be present but unopened"
    );
}

/// No wiring required. Starting a channel with nothing attached is harmless.
#[test]
#[ignore = "needs an adapter attached"]
fn channel_starts_and_stops() {
    let device = Device::open_first().expect("adapter should open");
    device
        .start_channel(0, BITRATE, ChannelMode::Normal)
        .expect("channel 0 should start");
    device.stop_channel(0).expect("channel 0 should stop");
}

/// Starting twice in a row must work: firmware keeps channel state across runs.
#[test]
#[ignore = "needs an adapter attached"]
fn channel_restart_is_idempotent() {
    let device = Device::open_first().expect("adapter should open");
    for attempt in 0..3 {
        device
            .start_channel(0, BITRATE, ChannelMode::Normal)
            .unwrap_or_else(|e| panic!("start attempt {attempt} failed: {e}"));
    }
    let _ = device.stop_channel(0);
}

/// Needs channel 0 wired to channel 1 with termination at both ends.
#[test]
#[ignore = "needs channel 0 wired to channel 1"]
fn frames_cross_from_channel_0_to_channel_1() {
    let device = Device::open_first().expect("adapter should open");
    if device.channel_count() < 2 {
        eprintln!("single-channel adapter, skipping");
        return;
    }
    device
        .start_channel(0, BITRATE, ChannelMode::Normal)
        .unwrap();
    device
        .start_channel(1, BITRATE, ChannelMode::Normal)
        .unwrap();

    let sent = CanFrame::new(0x321, false, &[0xca, 0xfe, 0xba, 0xbe]);
    device
        .transmit(0, &[sent])
        .expect("transmit should be accepted");

    let received = drain_until(&device, 1, 1, Duration::from_secs(2));
    assert_eq!(received.len(), 1, "the frame did not arrive on channel 1");
    assert_eq!(received[0].id, sent.id);
    assert_eq!(received[0].payload(), sent.payload());
    assert!(!received[0].extended);

    let _ = device.stop_channel(0);
    let _ = device.stop_channel(1);
}

/// Extended identifiers survive the round trip. Same wiring as above.
#[test]
#[ignore = "needs channel 0 wired to channel 1"]
fn extended_identifiers_survive_the_round_trip() {
    let device = Device::open_first().expect("adapter should open");
    if device.channel_count() < 2 {
        return;
    }
    device
        .start_channel(0, BITRATE, ChannelMode::Normal)
        .unwrap();
    device
        .start_channel(1, BITRATE, ChannelMode::Normal)
        .unwrap();

    let sent = CanFrame::new(0x1abc_def0, true, &[1, 2, 3, 4, 5, 6, 7, 8]);
    device.transmit(0, &[sent]).unwrap();

    let received = drain_until(&device, 1, 1, Duration::from_secs(2));
    assert_eq!(received.len(), 1);
    assert!(received[0].extended);
    assert_eq!(received[0].id, 0x1abc_def0);
    assert_eq!(received[0].payload(), &[1, 2, 3, 4, 5, 6, 7, 8]);

    let _ = device.stop_channel(0);
    let _ = device.stop_channel(1);
}

/// A burst in one USB transfer must arrive complete and in order.
#[test]
#[ignore = "needs channel 0 wired to channel 1"]
fn a_burst_arrives_complete_and_in_order() {
    const COUNT: usize = 32;

    let device = Device::open_first().expect("adapter should open");
    if device.channel_count() < 2 {
        return;
    }
    device
        .start_channel(0, BITRATE, ChannelMode::Normal)
        .unwrap();
    device
        .start_channel(1, BITRATE, ChannelMode::Normal)
        .unwrap();

    for i in 0..COUNT {
        device
            .transmit(0, &[CanFrame::new(0x400 + i as u32, false, &[i as u8])])
            .unwrap();
    }

    let received = drain_until(&device, 1, COUNT, Duration::from_secs(5));
    assert_eq!(received.len(), COUNT, "frames were lost");
    for (i, frame) in received.iter().enumerate() {
        assert_eq!(frame.id, 0x400 + i as u32, "frame {i} arrived out of order");
        assert_eq!(frame.payload(), &[i as u8]);
    }

    let _ = device.stop_channel(0);
    let _ = device.stop_channel(1);
}

fn drain_until(device: &Device, channel: u8, want: usize, budget: Duration) -> Vec<CanFrame> {
    let deadline = Instant::now() + budget;
    let mut out = Vec::with_capacity(want);
    while out.len() < want && Instant::now() < deadline {
        match device.receive(200) {
            Ok(chunk) => out.extend(chunk.frames_on(channel).copied()),
            Err(Error::Timeout { .. }) => continue,
            Err(e) => panic!("receive failed: {e}"),
        }
    }
    out
}
