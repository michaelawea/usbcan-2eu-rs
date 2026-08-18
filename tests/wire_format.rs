//! Decoder tests driven by bytes captured from real hardware.
//!
//! These run on any platform: the protocol layer performs no I/O. Hardware is
//! only needed for `tests/hardware.rs`.

use usbcan_2eu::protocol::{
    parse_ctrl_resp, parse_rx_chunk, parse_tx_ack, Bitrate, RxStatusKind, FRAME_LEN,
};

/// Read a `.hex` fixture: whitespace-insensitive hex, `#` starts a comment.
fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .flat_map(|line| line.split_whitespace())
        .map(|token| {
            u8::from_str_radix(token, 16)
                .unwrap_or_else(|_| panic!("'{token}' in {name} is not a hex byte"))
        })
        .collect()
}

#[test]
fn captured_probe_response_decodes() {
    let bytes = fixture("ctrl_resp_probe.hex");
    let resp = parse_ctrl_resp(&bytes, 0x01).expect("probe response should decode");
    assert_eq!(resp.cmd_echo, 0x01);
    assert_eq!(resp.bytes(), &[0x71, 0x04]);
}

#[test]
fn captured_probe_response_rejects_wrong_command() {
    let bytes = fixture("ctrl_resp_probe.hex");
    assert!(
        parse_ctrl_resp(&bytes, 0x02).is_err(),
        "a response must not be accepted for a command it does not echo"
    );
}

#[test]
fn captured_transmit_ack_decodes() {
    let ack = parse_tx_ack(&fixture("tx_ack_success.hex")).expect("ack should decode");
    assert!(ack.is_success());
    assert_eq!(ack.channel_marker, 0x81);
    assert_eq!(ack.dlc_echo, 4);
}

#[test]
fn captured_bus_error_status_decodes() {
    let bytes = fixture("rx_status_bus_error.hex");
    assert_eq!(bytes.len(), FRAME_LEN, "a record is always 24 bytes");
    let chunk = parse_rx_chunk(&bytes);
    assert!(chunk.frames.is_empty());
    assert!(chunk.unknown.is_empty());
    assert_eq!(chunk.status.len(), 1);
    assert_eq!(chunk.status[0].kind, RxStatusKind::BusErrorOrPassive);
}

#[test]
fn empty_input_decodes_to_nothing() {
    assert!(parse_rx_chunk(&[]).is_empty());
}

#[test]
fn the_bit_timing_table_is_internally_consistent() {
    // Duplicated from the unit tests on purpose: if this table is wrong, nothing
    // else in the driver works, and it is the value most likely to need revisiting
    // on unfamiliar hardware.
    for rate in Bitrate::ALL {
        assert_eq!(
            rate.clocks_per_bit() * rate.kbps(),
            36_000,
            "{rate} does not divide the controller clock exactly"
        );
    }
}
