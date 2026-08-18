//! USB transport for macOS, built on Apple's IOUSBHost framework.
//!
//! # Why not libusb
//!
//! The adapter exposes an isochronous endpoint pair (`0x03`/`0x83`) that it never
//! uses. libusb's `claim_interface()` walks every endpoint in the interface, and
//! on macOS 26 that walk trips a null-pointer dereference inside
//! `IOUSBHostFamily.kext` and takes the whole machine down via the SoC watchdog.
//!
//! IOUSBHost lets us open pipes one address at a time, so the isochronous pair is
//! simply never touched. Full detail, including the exact trigger conditions, is
//! in `docs/macos-usb-isoc-kernel-bug.md`.
//!
//! **Do not add code that opens endpoint `0x03` or `0x83`.**

use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc;
use std::time::Duration;

use objc2::rc::{Allocated, Retained};
use objc2::AllocAnyThread;
use objc2_core_foundation::{CFDictionary, CFRetained};
use objc2_foundation::{NSError, NSMutableData, NSNumber};
use objc2_io_kit::{
    io_iterator_t, io_service_t, kIOMainPortDefault, IOIteratorNext, IOObjectRelease,
    IOServiceGetMatchingServices,
};
use objc2_io_usb_host::{
    IOUSBHostDevice, IOUSBHostInterface, IOUSBHostObjectInitOptions, IOUSBHostPipe,
};
use tracing::{debug, info, warn};

use crate::error::Error;
use crate::protocol as proto;
use crate::protocol::{Bitrate, CanFrame, ChannelMode, CtrlCmd, CtrlResp, RxChunk, TxAck};

/// Endpoints opened at attach time, besides the five the driver actively uses.
///
/// The vendor's own userspace library opens every non-isochronous endpoint before
/// talking to the device. Doing the same avoids a class of "device stays idle"
/// failures. `0x03`/`0x83` are isochronous and deliberately absent.
const AUX_ENDPOINTS: [u8; 3] = [0x01, 0x81, 0x05];

/// Default timeout for control command round-trips.
const DEFAULT_CTRL_TIMEOUT_MS: u32 = 2000;

/// Largest receive chunk the device will hand back in one transfer.
const RX_BUFFER_LEN: usize = 6144;

/// A device that matched the driver's VID/PID, before it is opened.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// USB vendor ID, always [`proto::VID`].
    pub vid: u16,
    /// USB product ID.
    pub pid: u16,
    /// IOKit service handle.
    pub service: io_service_t,
}

impl DeviceInfo {
    /// Human-readable model name for [`Self::pid`].
    pub fn model(&self) -> &'static str {
        match self.pid {
            proto::PID_2E_U => "USBCAN-2E-U",
            proto::PID_E_U => "USBCAN-E-U",
            _ => "unknown",
        }
    }

    /// Number of CAN channels the model provides.
    pub fn channel_count(&self) -> u8 {
        match self.pid {
            proto::PID_2E_U => 2,
            _ => 1,
        }
    }
}

/// One endpoint descriptor, as reported by the device.
#[derive(Debug, Clone, Copy)]
pub struct EndpointInfo {
    /// `bEndpointAddress`, including the direction bit.
    pub address: u8,
    /// Endpoint number without the direction bit.
    pub number: u8,
    /// `0` for OUT, `1` for IN.
    pub direction: u8,
    /// `0` control, `1` isochronous, `2` bulk, `3` interrupt.
    pub transfer_type: u8,
    /// `wMaxPacketSize`.
    pub max_packet_size: u16,
}

impl EndpointInfo {
    /// Transfer type as a word.
    pub fn transfer_type_name(&self) -> &'static str {
        match self.transfer_type {
            0 => "control",
            1 => "isochronous",
            2 => "bulk",
            3 => "interrupt",
            _ => "unknown",
        }
    }

    /// Direction as a word.
    pub fn direction_name(&self) -> &'static str {
        if self.direction == 1 {
            "IN"
        } else {
            "OUT"
        }
    }
}

/// Releases an `io_service_t` on drop.
struct IoServiceGuard(io_service_t);
impl Drop for IoServiceGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            IOObjectRelease(self.0);
        }
    }
}

/// Releases an `io_iterator_t` on drop.
struct IoIteratorGuard(io_iterator_t);
impl Drop for IoIteratorGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            IOObjectRelease(self.0);
        }
    }
}

/// An open USBCAN-(2)E-U.
///
/// Dropping the handle releases the USB interface. The device keeps its channel
/// state across process exits — see [`Device::start_channel`].
pub struct Device {
    _host_device: Retained<IOUSBHostDevice>,
    interface: Retained<IOUSBHostInterface>,
    pipe_ctrl_out: Retained<IOUSBHostPipe>,
    pipe_ctrl_in: Retained<IOUSBHostPipe>,
    pipe_data_out: Retained<IOUSBHostPipe>,
    pipe_tx_ack_in: Retained<IOUSBHostPipe>,
    pipe_data_in: Retained<IOUSBHostPipe>,
    _aux_pipes: Vec<Retained<IOUSBHostPipe>>,
    pid: u16,
    identification: Vec<u8>,
    ctrl_timeout_ms: u32,
}

// IOUSBHostPipe performs its work on an internal dispatch queue, so the handle
// itself is safe to move between threads. It is deliberately not `Sync`: two
// threads issuing transfers on one pipe concurrently is not supported.
unsafe impl Send for Device {}

impl Device {
    /// List attached adapters without opening any of them.
    pub fn list() -> Result<Vec<DeviceInfo>, Error> {
        let mut found = Vec::new();
        for &pid in proto::SUPPORTED_PIDS {
            for service in find_services(proto::VID, pid)? {
                found.push(DeviceInfo {
                    vid: proto::VID,
                    pid,
                    service,
                });
            }
        }
        Ok(found)
    }

    /// Open the only attached adapter.
    ///
    /// # Errors
    ///
    /// [`Error::DeviceNotFound`] if none is attached.
    pub fn open_first() -> Result<Self, Error> {
        Self::open(0)
    }

    /// Open the adapter at `index` within [`Device::list`].
    pub fn open(index: usize) -> Result<Self, Error> {
        let mut devices = Self::list()?;
        if index >= devices.len() {
            for d in devices {
                IOObjectRelease(d.service);
            }
            return Err(Error::DeviceNotFound {
                vid: proto::VID,
                pid: proto::PID_2E_U,
            });
        }
        let target = devices.remove(index);
        for d in devices {
            IOObjectRelease(d.service);
        }
        let target_guard = IoServiceGuard(target.service);

        info!(model = target.model(), "opening adapter via IOUSBHost");

        let host_device = open_host_device(target_guard.0)?;

        // Force SET_CONFIGURATION so IOKit publishes the interface node. This is
        // idempotent: an already-configured device is left alone. `matchInterfaces`
        // must be true, otherwise the interface is registered in a state that
        // subsequent opens cannot find.
        if let Err(e) = unsafe { host_device.configureWithValue_matchInterfaces_error(1, true) } {
            debug!(error = %nserror_to_error(&e, "configureWithValue"), "configure returned an error (usually already configured)");
        }

        // Walk the IORegistry for the interface child rather than using a matching
        // dictionary: the child may not be in `registered` state yet, and the device
        // node can have unrelated siblings such as a WebUSB user client.
        let mut intf_service: io_service_t = 0;
        for attempt in 0..20u32 {
            if let Some(s) = find_interface_child(target_guard.0, 0) {
                intf_service = s;
                debug!(attempt, "found IOUSBHostInterface");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if intf_service == 0 {
            intf_service = find_interface_service(proto::VID, target.pid, 0).map_err(|_| {
                Error::Init("the device exposes no USB interface; unplug and reattach it".into())
            })?;
        }
        let _intf_guard = IoServiceGuard(intf_service);

        let interface = open_host_interface(intf_service)?;

        let pipe_ctrl_out = open_pipe(&interface, proto::EP_CTRL_OUT)?;
        let pipe_ctrl_in = open_pipe(&interface, proto::EP_CTRL_IN)?;
        let pipe_data_out = open_pipe(&interface, proto::EP_DATA_OUT)?;
        let pipe_tx_ack_in = open_pipe(&interface, proto::EP_TX_ACK_IN)?;
        let pipe_data_in = open_pipe(&interface, proto::EP_DATA_IN)?;

        let mut aux_pipes = Vec::new();
        for addr in AUX_ENDPOINTS {
            match open_pipe(&interface, addr) {
                Ok(p) => aux_pipes.push(p),
                Err(e) => {
                    warn!(endpoint = format_args!("0x{addr:02X}"), error = %e, "auxiliary pipe unavailable")
                }
            }
        }
        debug!(
            aux = aux_pipes.len(),
            "5 data pipes plus {} auxiliary pipes open; isochronous endpoints skipped",
            aux_pipes.len()
        );

        let mut dev = Device {
            _host_device: host_device,
            interface,
            pipe_ctrl_out,
            pipe_ctrl_in,
            pipe_data_out,
            pipe_tx_ack_in,
            pipe_data_in,
            _aux_pipes: aux_pipes,
            pid: target.pid,
            identification: Vec::new(),
            ctrl_timeout_ms: DEFAULT_CTRL_TIMEOUT_MS,
        };

        // Identification is informational; a device that does not answer the probe
        // still works, so a failure here is not fatal.
        match dev.send_ctrl(CtrlCmd::Probe.code(), &[0x01]) {
            Ok(resp) => {
                dev.identification = resp.bytes().to_vec();
                info!(identification = ?dev.identification, "adapter ready");
            }
            Err(e) => warn!(error = %e, "adapter did not answer the identification probe"),
        }

        Ok(dev)
    }

    /// USB product ID of this adapter.
    pub fn product_id(&self) -> u16 {
        self.pid
    }

    /// Model name.
    pub fn model(&self) -> &'static str {
        match self.pid {
            proto::PID_2E_U => "USBCAN-2E-U",
            proto::PID_E_U => "USBCAN-E-U",
            _ => "unknown",
        }
    }

    /// Number of CAN channels this model provides.
    pub fn channel_count(&self) -> u8 {
        match self.pid {
            proto::PID_2E_U => 2,
            _ => 1,
        }
    }

    /// Raw payload returned by the identification probe.
    ///
    /// Two bytes on the units tested. The meaning is not documented; treat it as
    /// an opaque firmware fingerprint and report it when filing issues.
    pub fn identification(&self) -> &[u8] {
        &self.identification
    }

    /// Current control-command timeout in milliseconds.
    pub fn ctrl_timeout_ms(&self) -> u32 {
        self.ctrl_timeout_ms
    }

    /// Change the control-command timeout.
    pub fn set_ctrl_timeout_ms(&mut self, ms: u32) {
        self.ctrl_timeout_ms = ms;
    }

    /// Descriptors for every endpoint on the interface.
    ///
    /// Reads descriptors only; no pipe is opened, so this is safe to call at any
    /// time and is the recommended way to inspect an unfamiliar unit.
    pub fn endpoints(&self) -> Vec<EndpointInfo> {
        use objc2_io_kit::IOUSBDescriptorHeader;
        use objc2_io_usb_host::{
            IOUSBGetEndpointAddress, IOUSBGetEndpointDirection, IOUSBGetEndpointMaxPacketSize,
            IOUSBGetEndpointNumber, IOUSBGetEndpointType, IOUSBGetNextEndpointDescriptor,
        };
        let cfg = unsafe { self.interface.configurationDescriptor() };
        let iface = unsafe { self.interface.interfaceDescriptor() };
        let mut out = Vec::new();
        let mut current: *const IOUSBDescriptorHeader = ptr::null();
        loop {
            let next =
                unsafe { IOUSBGetNextEndpointDescriptor(cfg.as_ptr(), iface.as_ptr(), current) };
            if next.is_null() {
                break;
            }
            out.push(EndpointInfo {
                address: unsafe { IOUSBGetEndpointAddress(next) },
                number: unsafe { IOUSBGetEndpointNumber(next) },
                direction: unsafe { IOUSBGetEndpointDirection(next) },
                transfer_type: unsafe { IOUSBGetEndpointType(next) },
                // Speed 1 = full speed; this family is USB 1.1.
                max_packet_size: unsafe { IOUSBGetEndpointMaxPacketSize(1, next) },
            });
            current = next as *const _;
        }
        out
    }

    // ───────────────────────── Channel control ─────────────────────────

    /// Configure and start a CAN channel.
    ///
    /// Runs the full sequence the device expects: stop, initialize, set bit timing,
    /// start. It is safe to call on an already-running channel.
    ///
    /// # Firmware state survives your process
    ///
    /// A channel initialized by an earlier run stays initialized until the device
    /// is re-enumerated, and the initialize step then reports failure. That case is
    /// detected and treated as success — bit timing and start are applied either
    /// way, so a restart still lands on the requested configuration.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidChannel`] if `channel` is not a channel this model has.
    pub fn start_channel(
        &self,
        channel: u8,
        bitrate: Bitrate,
        mode: ChannelMode,
    ) -> Result<(), Error> {
        self.check_channel(channel)?;

        // Stop first. A channel left running by a previous process rejects the
        // initialize command, and this clears that.
        if let Err(e) = self.send_ctrl(CtrlCmd::StartStop.code(), &proto::stop_payload(channel)) {
            debug!(error = %e, "pre-start stop failed, continuing");
        }

        match self.send_ctrl(
            CtrlCmd::InitChannel.code(),
            &proto::init_channel_payload(channel),
        ) {
            Ok(_) => debug!(channel, "channel initialized"),
            Err(Error::Rejected { .. }) => {
                debug!(channel, "channel was already initialized in firmware");
            }
            Err(e) => return Err(e),
        }

        self.send_ctrl(
            CtrlCmd::SetBitTiming.code(),
            &proto::set_bit_timing_payload(channel, mode.flag(), bitrate.packed()),
        )?;
        self.send_ctrl(CtrlCmd::StartStop.code(), &proto::start_payload(channel))?;
        info!(channel, %bitrate, ?mode, "channel started");
        Ok(())
    }

    /// Stop a CAN channel and take it off the bus.
    pub fn stop_channel(&self, channel: u8) -> Result<(), Error> {
        self.check_channel(channel)?;
        self.send_ctrl(CtrlCmd::StartStop.code(), &proto::stop_payload(channel))?;
        info!(channel, "channel stopped");
        Ok(())
    }

    // ───────────────────────── Data path ─────────────────────────

    /// Transmit up to [`proto::MAX_FRAMES_PER_PACKET`] frames on one channel and
    /// wait for the device's acknowledgement.
    ///
    /// The acknowledgement confirms the adapter accepted the frames, not that any
    /// node on the bus received them.
    ///
    /// [`CanFrame::channel`] is ignored; `channel` decides.
    ///
    /// # Errors
    ///
    /// [`Error::Protocol`] if `frames` is empty, [`Error::Timeout`] if the device
    /// does not acknowledge.
    pub fn transmit(&self, channel: u8, frames: &[CanFrame]) -> Result<TxAck, Error> {
        self.check_channel(channel)?;
        if frames.is_empty() {
            return Err(Error::Protocol("transmit called with no frames"));
        }
        if frames.len() > proto::MAX_FRAMES_PER_PACKET {
            return Err(Error::Protocol("too many frames for one transmit packet"));
        }
        let packet = proto::pack_tx_packet(frames, channel, true);
        bulk_out(&self.pipe_data_out, &packet, self.ctrl_timeout_ms)?;

        let mut ack = [0u8; 64];
        let n = bulk_in(&self.pipe_tx_ack_in, &mut ack, self.ctrl_timeout_ms)?;
        if n == 0 {
            return Err(Error::Timeout {
                ms: self.ctrl_timeout_ms,
            });
        }
        proto::parse_tx_ack(&ack[..n])
    }

    /// Read one batch of received frames and status records.
    ///
    /// Both channels share one stream; use [`RxChunk::frames_on`] to filter. An
    /// idle bus produces [`Error::Timeout`], which is normal and not an error
    /// condition for a polling loop.
    pub fn receive(&self, timeout_ms: u32) -> Result<RxChunk, Error> {
        let mut buf = [0u8; RX_BUFFER_LEN];
        let n = bulk_in(&self.pipe_data_in, &mut buf, timeout_ms)?;
        Ok(proto::parse_rx_chunk(&buf[..n]))
    }

    /// Read the receive stream without decoding it.
    ///
    /// For protocol work and bug reports: [`Device::receive`] discards records it
    /// does not recognize, this does not.
    pub fn receive_raw(&self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
        bulk_in(&self.pipe_data_in, buf, timeout_ms)
    }

    // ───────────────────────── Control ─────────────────────────

    /// Send a control command and read its response.
    ///
    /// The higher-level methods cover normal use. This is exposed for commands the
    /// driver does not model.
    pub fn send_ctrl(&self, cmd: u8, payload: &[u8]) -> Result<CtrlResp, Error> {
        let packet = proto::pack_ctrl(cmd, payload);
        interrupt_out(&self.pipe_ctrl_out, &packet)?;

        let mut buf = [0u8; proto::CTRL_PACKET_LEN];
        let n = interrupt_in_timeout(&self.pipe_ctrl_in, &mut buf, self.ctrl_timeout_ms)?;
        if n == 0 {
            return Err(Error::Timeout {
                ms: self.ctrl_timeout_ms,
            });
        }
        proto::parse_ctrl_resp(&buf[..n], cmd)
    }

    fn check_channel(&self, channel: u8) -> Result<(), Error> {
        if channel >= self.channel_count() {
            return Err(Error::InvalidChannel(channel));
        }
        Ok(())
    }
}

// ───────────────────────── Transfer helpers ─────────────────────────

fn bulk_out(pipe: &IOUSBHostPipe, data: &[u8], timeout_ms: u32) -> Result<(), Error> {
    let nsdata = nsdata_from_slice(data);
    let mut transferred: usize = 0;
    unsafe {
        pipe.sendIORequestWithData_bytesTransferred_completionTimeout_error(
            Some(&nsdata),
            &mut transferred,
            f64::from(timeout_ms) / 1000.0,
        )
    }
    .map_err(|e| {
        if is_timeout(&e) {
            Error::Timeout { ms: timeout_ms }
        } else {
            nserror_to_error(&e, "bulk OUT")
        }
    })?;
    if transferred != data.len() {
        warn!(sent = transferred, expected = data.len(), "short bulk OUT");
    }
    Ok(())
}

fn bulk_in(pipe: &IOUSBHostPipe, buf: &mut [u8], timeout_ms: u32) -> Result<usize, Error> {
    let nsdata = nsdata_with_capacity(buf.len());
    let mut transferred: usize = 0;
    match unsafe {
        pipe.sendIORequestWithData_bytesTransferred_completionTimeout_error(
            Some(&nsdata),
            &mut transferred,
            f64::from(timeout_ms) / 1000.0,
        )
    } {
        Ok(()) => Ok(copy_out(&nsdata, transferred, buf)),
        Err(e) if is_timeout(&e) => Err(Error::Timeout { ms: timeout_ms }),
        Err(e) => Err(nserror_to_error(&e, "bulk IN")),
    }
}

/// IOUSBHost requires `completionTimeout == 0` on interrupt pipes; a non-zero value
/// is rejected outright with `kIOReturnBadArgument`.
fn interrupt_out(pipe: &IOUSBHostPipe, data: &[u8]) -> Result<(), Error> {
    let nsdata = nsdata_from_slice(data);
    let mut transferred: usize = 0;
    unsafe {
        pipe.sendIORequestWithData_bytesTransferred_completionTimeout_error(
            Some(&nsdata),
            &mut transferred,
            0.0,
        )
    }
    .map_err(|e| nserror_to_error(&e, "interrupt OUT"))?;
    Ok(())
}

/// Timed read from an interrupt pipe.
///
/// Since interrupt transfers cannot carry a timeout (see [`interrupt_out`]), the
/// transfer is issued blocking and a helper thread aborts the pipe once the
/// deadline passes. The helper is cancelled as soon as the transfer returns.
fn interrupt_in_timeout(
    pipe: &IOUSBHostPipe,
    buf: &mut [u8],
    timeout_ms: u32,
) -> Result<usize, Error> {
    let nsdata = nsdata_with_capacity(buf.len());
    let mut transferred: usize = 0;

    let pipe_addr = (pipe as *const IOUSBHostPipe) as usize;
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if cancel_rx
            .recv_timeout(Duration::from_millis(u64::from(timeout_ms)))
            .is_err()
        {
            // SAFETY: the transfer below holds `pipe` borrowed for the whole
            // lifetime of this thread, which is joined before returning.
            let pipe = unsafe { &*(pipe_addr as *const IOUSBHostPipe) };
            let _ = unsafe { pipe.abortWithError() };
        }
    });

    let result = unsafe {
        pipe.sendIORequestWithData_bytesTransferred_completionTimeout_error(
            Some(&nsdata),
            &mut transferred,
            0.0,
        )
    };
    let _ = cancel_tx.send(());
    let _ = watchdog.join();

    match result {
        Ok(()) => Ok(copy_out(&nsdata, transferred, buf)),
        Err(e) if is_aborted(&e) => Err(Error::Timeout { ms: timeout_ms }),
        Err(e) => Err(nserror_to_error(&e, "interrupt IN")),
    }
}

fn copy_out(nsdata: &NSMutableData, transferred: usize, buf: &mut [u8]) -> usize {
    let n = transferred.min(buf.len());
    let bytes = unsafe { nsdata.as_bytes_unchecked() };
    buf[..n].copy_from_slice(&bytes[..n]);
    n
}

// ───────────────────────── IOKit plumbing ─────────────────────────

fn find_services(vid: u16, pid: u16) -> Result<Vec<io_service_t>, Error> {
    let vid_num = NSNumber::new_u16(vid);
    let pid_num = NSNumber::new_u16(pid);

    let matching = unsafe {
        IOUSBHostDevice::createMatchingDictionaryWithVendorID_productID_bcdDevice_deviceClass_deviceSubclass_deviceProtocol_speed_productIDArray(
            Some(&vid_num), Some(&pid_num), None, None, None, None, None, None,
        )
    };
    let dict_ptr = Retained::into_raw(matching) as *mut CFDictionary;
    let dict = unsafe { CFRetained::from_raw(ptr::NonNull::new(dict_ptr).unwrap()) };

    let mut iter: io_iterator_t = 0;
    let rc = unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, Some(dict), &mut iter) };
    if rc != 0 {
        return Err(Error::Init(format!(
            "IOServiceGetMatchingServices failed (0x{rc:x})"
        )));
    }
    let _guard = IoIteratorGuard(iter);

    let mut services = Vec::new();
    loop {
        let s = IOIteratorNext(iter);
        if s == 0 {
            break;
        }
        services.push(s);
    }
    Ok(services)
}

/// Find the `IOUSBHostInterface` child of a device node.
///
/// Broader than a matching dictionary, which only sees services in `registered`
/// state, and stricter than "take the first child", which can hand back an
/// unrelated user client.
fn find_interface_child(device_service: io_service_t, want_iface_num: u8) -> Option<io_service_t> {
    use objc2_core_foundation::CFString;

    let plane_ptr = objc2_io_kit::kIOServicePlane.as_ptr() as *mut [std::os::raw::c_char; 128];
    let mut iter: io_iterator_t = 0;
    let rc = unsafe {
        objc2_io_kit::IORegistryEntryGetChildIterator(device_service, plane_ptr, &mut iter)
    };
    if rc != 0 {
        warn!(rc, "IORegistryEntryGetChildIterator failed");
        return None;
    }
    let _guard = IoIteratorGuard(iter);

    let class_name = b"IOUSBHostInterface\0";
    let class_name_ptr = class_name.as_ptr() as *mut [std::os::raw::c_char; 128];
    let bif_key = CFString::from_str("bInterfaceNumber");

    loop {
        let child = IOIteratorNext(iter);
        if child == 0 {
            break;
        }
        if !unsafe { objc2_io_kit::IOObjectConformsTo(child, class_name_ptr) } {
            IOObjectRelease(child);
            continue;
        }
        let prop = unsafe {
            objc2_io_kit::IORegistryEntryCreateCFProperty(child, Some(&bif_key), None, 0)
        };
        let iface_num = match prop {
            // CFNumber is toll-free bridged to NSNumber.
            Some(cf) => unsafe {
                (*(CFRetained::as_ptr(&cf).as_ptr() as *const NSNumber)).unsignedCharValue()
            },
            None => {
                IOObjectRelease(child);
                continue;
            }
        };
        if iface_num == want_iface_num {
            return Some(child);
        }
        IOObjectRelease(child);
    }
    None
}

/// Fallback lookup that only finds interfaces already in `registered` state.
fn find_interface_service(vid: u16, pid: u16, iface_num: u8) -> Result<io_service_t, Error> {
    let vid_num = NSNumber::new_u16(vid);
    let pid_num = NSNumber::new_u16(pid);
    let if_num = NSNumber::new_u8(iface_num);
    let cfg_val = NSNumber::new_u8(1);

    let matching = unsafe {
        IOUSBHostInterface::createMatchingDictionaryWithVendorID_productID_bcdDevice_interfaceNumber_configurationValue_interfaceClass_interfaceSubclass_interfaceProtocol_speed_productIDArray(
            Some(&vid_num), Some(&pid_num), None, Some(&if_num), Some(&cfg_val),
            None, None, None, None, None,
        )
    };
    let dict_ptr = Retained::into_raw(matching) as *mut CFDictionary;
    let dict = unsafe { CFRetained::from_raw(ptr::NonNull::new(dict_ptr).unwrap()) };

    let mut iter: io_iterator_t = 0;
    let rc = unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, Some(dict), &mut iter) };
    if rc != 0 {
        return Err(Error::Init(format!(
            "IOServiceGetMatchingServices for interface failed (0x{rc:x})"
        )));
    }
    let _guard = IoIteratorGuard(iter);

    let first = IOIteratorNext(iter);
    if first == 0 {
        return Err(Error::Init(format!(
            "no USB interface for VID 0x{vid:04x} PID 0x{pid:04x} interface {iface_num}"
        )));
    }
    Ok(first)
}

fn open_host_device(service: io_service_t) -> Result<Retained<IOUSBHostDevice>, Error> {
    let alloc: Allocated<IOUSBHostDevice> = IOUSBHostDevice::alloc();
    let mut err: Option<Retained<NSError>> = None;
    unsafe {
        IOUSBHostDevice::initWithIOService_options_queue_error_interestHandler(
            alloc,
            service,
            IOUSBHostObjectInitOptions::None,
            None,
            Some(&mut err),
            ptr::null_mut(),
        )
    }
    .ok_or_else(|| match err {
        Some(e) => nserror_to_error(&e, "open USB device"),
        None => Error::Init("IOUSBHostDevice initialization returned nil".into()),
    })
}

fn open_host_interface(service: io_service_t) -> Result<Retained<IOUSBHostInterface>, Error> {
    let alloc: Allocated<IOUSBHostInterface> = IOUSBHostInterface::alloc();
    let mut err: Option<Retained<NSError>> = None;
    unsafe {
        IOUSBHostInterface::initWithIOService_options_queue_error_interestHandler(
            alloc,
            service,
            IOUSBHostObjectInitOptions::None,
            None,
            Some(&mut err),
            ptr::null_mut(),
        )
    }
    .ok_or_else(|| match err {
        Some(e) => nserror_to_error(&e, "open USB interface"),
        None => Error::Init("IOUSBHostInterface initialization returned nil".into()),
    })
}

/// Open exactly one pipe by endpoint address.
///
/// Opening a pipe is what makes the kernel touch an endpoint, which is why this
/// driver never passes `0x03` or `0x83` here.
fn open_pipe(
    intf: &Retained<IOUSBHostInterface>,
    addr: u8,
) -> Result<Retained<IOUSBHostPipe>, Error> {
    unsafe { intf.copyPipeWithAddress_error(addr as usize) }.map_err(|e| {
        let err = nserror_to_error(&e, &format!("open pipe 0x{addr:02x}"));
        match err {
            // kIOReturnNoDevice / kIOUSBEndpointNotFound
            Error::Usb { code, .. } if code == -536870206 || code == -536870212 => {
                Error::MissingEndpoint { addr }
            }
            other => other,
        }
    })
}

fn error_code(err: &NSError) -> i32 {
    err.code() as i32
}

fn nserror_to_error(err: &NSError, context: &str) -> Error {
    let code = error_code(err);
    Error::Usb {
        code,
        message: format!(
            "{context}: [{} {code}] {}",
            err.domain(),
            err.localizedDescription()
        ),
    }
}

/// `kIOReturnTimeout` (0xE00002D6).
fn is_timeout(err: &NSError) -> bool {
    error_code(err) == -536870186
}

/// `kIOReturnAborted` (0xE00002EB) — what the watchdog thread produces.
fn is_aborted(err: &NSError) -> bool {
    let code = error_code(err);
    code == -536870165 || code == -536870186
}

fn nsdata_from_slice(data: &[u8]) -> Retained<NSMutableData> {
    let nsdata = NSMutableData::new();
    unsafe {
        nsdata.appendBytes_length(
            ptr::NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
            data.len(),
        );
    }
    nsdata
}

fn nsdata_with_capacity(len: usize) -> Retained<NSMutableData> {
    NSMutableData::dataWithLength(len).expect("NSMutableData allocation failed")
}
