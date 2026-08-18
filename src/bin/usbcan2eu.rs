//! Command-line tool for USBCAN-(2)E-U adapters.
//!
//! Run `usbcan2eu selftest` to verify a unit end to end, `usbcan2eu dump` to watch
//! a bus, or `usbcan2eu slcan` to expose the adapter to python-can and friends.

#[cfg(target_os = "macos")]
mod app {
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use clap::{Args, Parser, Subcommand};
    use usbcan_2eu::{Bitrate, CanFrame, ChannelMode, Device, Error};

    #[derive(Parser)]
    #[command(
        name = "usbcan2eu",
        version,
        about = "Unofficial macOS driver for ZLG USBCAN-E-U / USBCAN-2E-U adapters",
        long_about = None,
    )]
    struct Cli {
        /// Index of the adapter to use when more than one is attached.
        #[arg(long, global = true, default_value_t = 0, value_name = "N")]
        device: usize,

        /// Log driver internals. Repeat for more detail.
        #[arg(short, long, global = true, action = clap::ArgAction::Count)]
        verbose: u8,

        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// List attached adapters and describe one in detail.
        Info,

        /// Print frames arriving on a channel.
        Dump(DumpArgs),

        /// Transmit one frame.
        Send(SendArgs),

        /// Verify the adapter end to end by looping channel 0 to channel 1.
        Selftest(SelftestArgs),

        /// Expose the adapter as an SLCAN serial device.
        Slcan(SlcanArgs),
    }

    #[derive(Args)]
    struct DumpArgs {
        /// CAN channel to listen on.
        #[arg(long, default_value_t = 0)]
        channel: u8,

        /// Bit rate, for example 250k, 500k, 1000k.
        #[arg(long, short, default_value = "500k", value_parser = parse_bitrate)]
        bitrate: Bitrate,

        /// Do not transmit or acknowledge on the bus.
        #[arg(long)]
        listen_only: bool,

        /// Also print bus status and error records.
        #[arg(long)]
        status: bool,

        /// Stop after this many frames.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    }

    #[derive(Args)]
    struct SendArgs {
        /// Frame in candump notation: 123#DEADBEEF, 1ABCDEF0#01020304, 123#R4.
        #[arg(value_name = "FRAME")]
        frame: String,

        /// CAN channel to transmit on.
        #[arg(long, default_value_t = 0)]
        channel: u8,

        /// Bit rate, for example 250k, 500k, 1000k.
        #[arg(long, short, default_value = "500k", value_parser = parse_bitrate)]
        bitrate: Bitrate,

        /// Repeat the frame this many times.
        #[arg(long, default_value_t = 1, value_name = "N")]
        count: usize,

        /// Milliseconds to wait between repeats.
        #[arg(long, default_value_t = 100, value_name = "MS")]
        interval: u64,
    }

    #[derive(Args)]
    struct SelftestArgs {
        /// Bit rate to test at.
        #[arg(long, short, default_value = "500k", value_parser = parse_bitrate)]
        bitrate: Bitrate,

        /// Frames to send in each direction.
        #[arg(long, default_value_t = 100, value_name = "N")]
        frames: usize,
    }

    #[derive(Args)]
    struct SlcanArgs {
        /// CAN channel to bridge.
        #[arg(long, default_value_t = 0)]
        channel: u8,

        /// Initial bit rate. A client may override it with an S command before O.
        #[arg(long, short, default_value = "500k", value_parser = parse_bitrate)]
        bitrate: Bitrate,
    }

    pub fn main() -> ExitCode {
        let cli = Cli::parse();
        init_logging(cli.verbose);

        let result = match &cli.command {
            Command::Info => cmd_info(cli.device),
            Command::Dump(args) => cmd_dump(cli.device, args),
            Command::Send(args) => cmd_send(cli.device, args),
            Command::Selftest(args) => cmd_selftest(cli.device, args),
            Command::Slcan(args) => cmd_slcan(cli.device, args),
        };

        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                if let Error::DeviceNotFound { .. } = e {
                    eprintln!("hint: check the adapter is attached with `usbcan2eu info`");
                }
                ExitCode::FAILURE
            }
        }
    }

    fn init_logging(verbosity: u8) {
        let level = match verbosity {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        };
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
            )
            .with_target(false)
            .init();
    }

    // ───────────────────────────── info ─────────────────────────────

    fn cmd_info(index: usize) -> Result<(), Error> {
        let adapters = Device::list()?;
        if adapters.is_empty() {
            println!("No USBCAN-(2)E-U adapter found.");
            println!("Looked for USB vendor 0x0471, products 0x1260 and 0x1261.");
            return Ok(());
        }

        println!("Attached adapters:");
        for (i, a) in adapters.iter().enumerate() {
            println!(
                "  [{i}] {} (VID 0x{:04x} PID 0x{:04x}, {} channel{})",
                a.model(),
                a.vid,
                a.pid,
                a.channel_count(),
                if a.channel_count() == 1 { "" } else { "s" }
            );
        }
        println!();

        let device = Device::open(index)?;
        println!("Adapter {index}");
        println!("  model            {}", device.model());
        println!("  product id       0x{:04x}", device.product_id());
        println!("  channels         {}", device.channel_count());
        let id = device.identification();
        if id.is_empty() {
            println!("  identification   (no response)");
        } else {
            let hex: Vec<String> = id.iter().map(|b| format!("{b:02X}")).collect();
            println!("  identification   {}", hex.join(" "));
        }

        println!("\n  endpoints");
        for ep in device.endpoints() {
            let note = match ep.address {
                0x03 | 0x83 => "  <- isochronous, never opened (see docs)",
                0x02 => "  <- transmit",
                0x82 => "  <- transmit ack",
                0x04 => "  <- control out",
                0x84 => "  <- control in",
                0x85 => "  <- receive",
                _ => "",
            };
            println!(
                "    0x{:02X}  {:<11} {:<3} max {:>3} bytes{}",
                ep.address,
                ep.transfer_type_name(),
                ep.direction_name(),
                ep.max_packet_size,
                note
            );
        }

        println!(
            "\n  bit timing table (controller clock {} MHz)",
            usbcan_2eu::protocol::CAN_CLOCK_HZ / 1_000_000
        );
        for b in Bitrate::ALL {
            let (brp, tseg1, tseg2) = Bitrate::unpack(b.packed());
            println!(
                "    {:>4} kbit/s   brp {:>3}  tseg1 {:>2}  tseg2 {}  ->  {:>4} clocks/bit",
                b.kbps(),
                brp,
                tseg1,
                tseg2,
                b.clocks_per_bit()
            );
        }
        Ok(())
    }

    // ───────────────────────────── dump ─────────────────────────────

    fn cmd_dump(index: usize, args: &DumpArgs) -> Result<(), Error> {
        let device = Device::open(index)?;
        let mode = if args.listen_only {
            ChannelMode::Passive
        } else {
            ChannelMode::Normal
        };
        device.start_channel(args.channel, args.bitrate, mode)?;

        eprintln!(
            "listening on channel {} at {} ({}); press Ctrl-C to stop",
            args.channel,
            args.bitrate,
            if args.listen_only {
                "listen-only"
            } else {
                "normal"
            }
        );

        let running = install_signal_handler();
        let started = Instant::now();
        let mut count = 0usize;

        while running.load(Ordering::Relaxed) {
            match device.receive(200) {
                Ok(chunk) => {
                    for frame in chunk.frames_on(args.channel) {
                        println!(
                            "{:>10.3}  can{}  {}  [{}] {}",
                            started.elapsed().as_secs_f64(),
                            frame.channel,
                            format_id(frame),
                            frame.dlc,
                            format_data(frame)
                        );
                        count += 1;
                        if args.limit.is_some_and(|l| count >= l) {
                            return finish_dump(&device, args.channel, count);
                        }
                    }
                    if args.status {
                        for status in &chunk.status {
                            let bytes: Vec<String> = status
                                .raw
                                .iter()
                                .take(12)
                                .map(|b| format!("{b:02X}"))
                                .collect();
                            eprintln!("  status {:?}: {}", status.kind, bytes.join(" "));
                        }
                    }
                }
                Err(Error::Timeout { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        finish_dump(&device, args.channel, count)
    }

    fn finish_dump(device: &Device, channel: u8, count: usize) -> Result<(), Error> {
        let _ = device.stop_channel(channel);
        eprintln!("\n{count} frame(s) received");
        Ok(())
    }

    // ───────────────────────────── send ─────────────────────────────

    fn cmd_send(index: usize, args: &SendArgs) -> Result<(), Error> {
        let frame = parse_frame(&args.frame).map_err(Error::Init)?;
        let device = Device::open(index)?;
        device.start_channel(args.channel, args.bitrate, ChannelMode::Normal)?;

        for i in 0..args.count {
            let ack = device.transmit(args.channel, &[frame])?;
            if ack.is_success() {
                println!("sent {}", format_frame(&frame));
            } else {
                eprintln!(
                    "adapter did not accept the frame (status 0x{:02X})",
                    ack.status
                );
            }
            if i + 1 < args.count {
                std::thread::sleep(Duration::from_millis(args.interval));
            }
        }
        let _ = device.stop_channel(args.channel);
        Ok(())
    }

    // ───────────────────────────── selftest ─────────────────────────────

    fn cmd_selftest(index: usize, args: &SelftestArgs) -> Result<(), Error> {
        println!("USBCAN-(2)E-U self test");
        println!();
        println!("This transmits on one channel and receives on the other, so wire the");
        println!("two channels together first: CANH to CANH, CANL to CANL, with 120 ohm");
        println!("termination across each end.");
        println!();

        let device = Device::open(index)?;
        println!("  adapter          {} at index {index}", device.model());
        if device.channel_count() < 2 {
            println!("  FAIL             this model has only one channel");
            return Err(Error::Init(
                "the self test needs a two-channel adapter".into(),
            ));
        }

        device.start_channel(0, args.bitrate, ChannelMode::Normal)?;
        device.start_channel(1, args.bitrate, ChannelMode::Normal)?;
        println!("  both channels    started at {}", args.bitrate);
        println!();

        let forward = run_direction(&device, 0, 1, args.frames)?;
        let reverse = run_direction(&device, 1, 0, args.frames)?;

        let _ = device.stop_channel(0);
        let _ = device.stop_channel(1);

        println!();
        let passed = forward.is_pass() && reverse.is_pass();
        if passed {
            println!("PASS");
        } else {
            println!("FAIL");
            println!();
            println!("If nothing arrived at all, the two channels are probably not wired");
            println!("together, or termination is missing. If some frames arrived but were");
            println!("corrupted, suspect the bit timing table in src/protocol.rs.");
        }
        if passed {
            Ok(())
        } else {
            Err(Error::Init("self test failed".into()))
        }
    }

    struct DirectionResult {
        sent: usize,
        received: usize,
        corrupted: usize,
    }

    impl DirectionResult {
        fn is_pass(&self) -> bool {
            self.received == self.sent && self.corrupted == 0
        }
    }

    fn run_direction(
        device: &Device,
        from: u8,
        to: u8,
        count: usize,
    ) -> Result<DirectionResult, Error> {
        print!("  can{from} -> can{to}     ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let mut sent = 0usize;
        let mut received = Vec::with_capacity(count);

        for i in 0..count {
            // Each frame carries its own index, so a frame can be validated on its
            // own without assuming anything about arrival order.
            let frame = CanFrame::new(id_for(i), false, &(i as u32).to_le_bytes());
            device.transmit(from, &[frame])?;
            sent += 1;

            // Drain what has arrived so far. The adapter buffers, so this does not
            // have to keep up frame for frame.
            if let Ok(chunk) = device.receive(5) {
                received.extend(chunk.frames_on(to).copied());
            }
        }

        // Collect stragglers.
        let deadline = Instant::now() + Duration::from_millis(500);
        while received.len() < sent && Instant::now() < deadline {
            match device.receive(100) {
                Ok(chunk) => received.extend(chunk.frames_on(to).copied()),
                Err(Error::Timeout { .. }) => break,
                Err(e) => return Err(e),
            }
        }

        let corrupted = received.iter().filter(|f| !is_intact(f, count)).count();

        let result = DirectionResult {
            sent,
            received: received.len(),
            corrupted,
        };
        if result.is_pass() {
            println!("{}/{} frames, 0 errors", result.received, result.sent);
        } else {
            println!(
                "{}/{} frames, {} corrupted",
                result.received, result.sent, result.corrupted
            );
        }
        Ok(result)
    }

    /// Identifier carried by self-test frame `index`.
    fn id_for(index: usize) -> u32 {
        0x100 + (index as u32 % 0x100)
    }

    /// A self-test frame is intact when its payload names an index in range and
    /// that index produces the identifier the frame arrived with.
    fn is_intact(frame: &CanFrame, count: usize) -> bool {
        let payload = frame.payload();
        if payload.len() != 4 {
            return false;
        }
        let index = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        index < count && frame.id == id_for(index) && !frame.extended && !frame.remote
    }

    // ───────────────────────────── slcan ─────────────────────────────

    fn cmd_slcan(index: usize, args: &SlcanArgs) -> Result<(), Error> {
        let device = Device::open(index)?;
        let mut bridge = usbcan_2eu::slcan::SlcanBridge::new(device, args.channel)?;
        bridge.set_bitrate(args.bitrate);

        println!("SLCAN bridge for channel {} is up.", args.channel);
        println!();
        println!("  device   {}", bridge.path());
        println!(
            "  bitrate  {} (a client may change it with an S command)",
            args.bitrate
        );
        println!();
        println!("python-can:");
        println!(
            "  can.Bus(interface=\"slcan\", channel=\"{}\", bitrate={})",
            bridge.path(),
            args.bitrate.kbps() * 1000
        );
        println!();
        println!("Press Ctrl-C to stop.");

        let running = install_signal_handler();
        bridge.run_until(|| !running.load(Ordering::Relaxed))
    }

    // ───────────────────────────── shared helpers ─────────────────────────────

    fn install_signal_handler() -> Arc<AtomicBool> {
        static STOP: AtomicBool = AtomicBool::new(false);
        extern "C" fn handle(_: libc::c_int) {
            STOP.store(true, Ordering::Relaxed);
        }
        let handler = handle as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        // SAFETY: the handler only stores into a static atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
        // Mirror the static into an Arc so callers get a plain handle.
        let flag = Arc::new(AtomicBool::new(true));
        let mirror = Arc::clone(&flag);
        std::thread::spawn(move || {
            while !STOP.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            mirror.store(false, Ordering::Relaxed);
        });
        flag
    }

    fn parse_bitrate(s: &str) -> Result<Bitrate, String> {
        let trimmed = s.trim().trim_end_matches(['k', 'K']);
        let value: u32 = trimmed
            .parse()
            .map_err(|_| format!("'{s}' is not a bit rate"))?;
        // Accept 500, 500k and 500000 as the same thing.
        let kbps = if s.ends_with('k') || s.ends_with('K') || value <= 1000 {
            value
        } else {
            value / 1000
        };
        Bitrate::from_kbps(kbps).ok_or_else(|| {
            let supported: Vec<String> = Bitrate::ALL
                .iter()
                .map(|b| format!("{}k", b.kbps()))
                .collect();
            format!(
                "{kbps} kbit/s is not supported; try one of {}",
                supported.join(", ")
            )
        })
    }

    /// Parse candump notation: `123#DEADBEEF`, `1ABCDEF0#0102`, `123#R4`.
    fn parse_frame(text: &str) -> Result<CanFrame, String> {
        let (id_text, body) = text
            .split_once('#')
            .ok_or_else(|| format!("'{text}' is missing the '#' separator"))?;
        if id_text.is_empty() || id_text.len() > 8 {
            return Err(format!("'{id_text}' is not a CAN identifier"));
        }
        let extended = id_text.len() > 3;
        let id = u32::from_str_radix(id_text, 16)
            .map_err(|_| format!("'{id_text}' is not hexadecimal"))?;
        let limit = if extended { 0x1fff_ffff } else { 0x7ff };
        if id > limit {
            return Err(format!(
                "identifier 0x{id:X} does not fit in the frame format"
            ));
        }

        if let Some(dlc_text) = body.strip_prefix(['R', 'r']) {
            let dlc: u8 = dlc_text
                .parse()
                .map_err(|_| format!("'{dlc_text}' is not a length"))?;
            if dlc > 8 {
                return Err("remote frame length must be 0 to 8".into());
            }
            return Ok(CanFrame::new_remote(id, extended, dlc));
        }

        if body.len() % 2 != 0 {
            return Err("payload must have an even number of hex digits".into());
        }
        if body.len() > 16 {
            return Err("payload is longer than 8 bytes".into());
        }
        let mut data = Vec::with_capacity(body.len() / 2);
        for pair in body.as_bytes().chunks(2) {
            let s =
                std::str::from_utf8(pair).map_err(|_| "payload is not hexadecimal".to_string())?;
            data.push(u8::from_str_radix(s, 16).map_err(|_| format!("'{s}' is not hexadecimal"))?);
        }
        Ok(CanFrame::new(id, extended, &data))
    }

    fn format_id(frame: &CanFrame) -> String {
        if frame.extended {
            format!("{:08X}", frame.id)
        } else {
            format!("     {:03X}", frame.id)
        }
    }

    fn format_data(frame: &CanFrame) -> String {
        if frame.remote {
            return "remote request".into();
        }
        frame
            .payload()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn format_frame(frame: &CanFrame) -> String {
        format!("{frame}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn selftest_frames_validate_independently_of_arrival_order() {
            let count = 50;
            // Every frame the test sends must validate.
            for i in 0..count {
                let f = CanFrame::new(id_for(i), false, &(i as u32).to_le_bytes());
                assert!(is_intact(&f, count), "frame {i} should validate");
            }
            // A frame whose payload and identifier disagree must not.
            let mismatched = CanFrame::new(id_for(1), false, &7u32.to_le_bytes());
            assert!(!is_intact(&mismatched, count));
            // Nor one with a truncated payload.
            assert!(!is_intact(&CanFrame::new(id_for(0), false, &[0, 0]), count));
            // Nor an index beyond what was sent.
            let stray = CanFrame::new(id_for(999), false, &999u32.to_le_bytes());
            assert!(!is_intact(&stray, count));
        }

        #[test]
        fn bitrate_accepts_all_common_spellings() {
            assert_eq!(parse_bitrate("500k").unwrap(), Bitrate::Kbps500);
            assert_eq!(parse_bitrate("500").unwrap(), Bitrate::Kbps500);
            assert_eq!(parse_bitrate("500000").unwrap(), Bitrate::Kbps500);
            assert_eq!(parse_bitrate("1000k").unwrap(), Bitrate::Kbps1000);
            assert_eq!(parse_bitrate("1000000").unwrap(), Bitrate::Kbps1000);
            assert!(parse_bitrate("333k").is_err());
            assert!(parse_bitrate("nonsense").is_err());
        }

        #[test]
        fn frame_notation_standard() {
            let f = parse_frame("123#DEADBEEF").unwrap();
            assert_eq!(f.id, 0x123);
            assert!(!f.extended);
            assert_eq!(f.payload(), &[0xde, 0xad, 0xbe, 0xef]);
        }

        #[test]
        fn frame_notation_extended() {
            let f = parse_frame("1ABCDEF0#0102").unwrap();
            assert_eq!(f.id, 0x1abc_def0);
            assert!(f.extended);
            assert_eq!(f.payload(), &[1, 2]);
        }

        #[test]
        fn frame_notation_empty_payload() {
            let f = parse_frame("123#").unwrap();
            assert_eq!(f.dlc, 0);
        }

        #[test]
        fn frame_notation_remote() {
            let f = parse_frame("123#R4").unwrap();
            assert!(f.remote);
            assert_eq!(f.dlc, 4);
        }

        #[test]
        fn frame_notation_rejects_bad_input() {
            assert!(parse_frame("123").is_err());
            assert!(
                parse_frame("800#00").is_err(),
                "id too large for a standard frame"
            );
            assert!(parse_frame("123#ABC").is_err(), "odd digit count");
            assert!(
                parse_frame("123#00112233445566778899").is_err(),
                "payload too long"
            );
            assert!(parse_frame("XYZ#00").is_err());
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    app::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("usbcan2eu runs on macOS only: the USB transport is not implemented elsewhere.");
    eprintln!("The protocol layer of the library itself is portable and builds anywhere.");
    std::process::ExitCode::FAILURE
}
