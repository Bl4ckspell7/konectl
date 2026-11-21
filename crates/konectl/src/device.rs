//! Device discovery and the high-level mouse API.

use std::ffi::CString;
use std::fmt;
use std::time::{Duration, Instant};

use hidapi::HidApi;

use crate::color::Rgb;
use crate::error::{Error, Result};
use crate::protocol::{self, Battery, Report, Signal};
use crate::transport::{HidTransport, Transport};

/// How long a single read waits for a report.
const READ_TIMEOUT_MS: i32 = 20;
/// How long a request waits for its answer.
const REPLY_DEADLINE: Duration = Duration::from_millis(500);
/// Silence after the last reply at which [`KoneProAir::raw`] stops collecting.
const RAW_QUIET_PERIOD: Duration = Duration::from_millis(100);
/// Upper bound on replies collected by [`KoneProAir::raw`].
const MAX_RAW_REPLIES: usize = 64;
/// Upper bound on stale reports discarded before a request.
const MAX_STALE_REPORTS: usize = 64;

/// How the mouse is connected to the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// Through the 2.4 GHz USB dongle.
    Wireless,
    /// Through the USB cable.
    Wired,
}

impl ConnectionMode {
    /// USB product ID the mouse uses in this mode.
    pub const fn product_id(self) -> u16 {
        match self {
            Self::Wireless => protocol::PRODUCT_ID_DONGLE,
            Self::Wired => protocol::PRODUCT_ID_WIRED,
        }
    }

    /// USB interface carrying the vendor channel in this mode.
    pub const fn interface(self) -> i32 {
        match self {
            Self::Wireless => protocol::INTERFACE_DONGLE,
            Self::Wired => protocol::INTERFACE_WIRED,
        }
    }

    /// The mode whose vendor channel is `interface` of `product_id`, if any.
    fn from_ids(product_id: u16, interface: i32) -> Option<Self> {
        [Self::Wired, Self::Wireless]
            .into_iter()
            .find(|mode| mode.product_id() == product_id && mode.interface() == interface)
    }
}

impl fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Self::Wireless => "wireless (dongle)",
            Self::Wired => "wired (cable)",
        })
    }
}

/// A connected Kone Pro Air found during discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub mode: ConnectionMode,
    /// Platform path of the vendor channel, e.g. `/dev/hidraw2`.
    pub path: CString,
    pub serial: Option<String>,
}

/// Finds and opens Kone Pro Air devices.
pub struct Manager {
    api: HidApi,
}

impl Manager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: HidApi::new()?,
        })
    }

    /// Re-scans the system for devices.
    pub fn refresh(&mut self) -> Result<()> {
        self.api.refresh_devices()?;
        Ok(())
    }

    /// All connected Kone Pro Air devices, wired ones first.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let mut devices: Vec<DeviceInfo> = self
            .api
            .device_list()
            .filter(|device| device.vendor_id() == protocol::VENDOR_ID)
            .filter_map(|device| {
                let mode =
                    ConnectionMode::from_ids(device.product_id(), device.interface_number())?;
                Some(DeviceInfo {
                    mode,
                    path: device.path().to_owned(),
                    serial: device
                        .serial_number()
                        .filter(|serial| !serial.is_empty())
                        .map(str::to_owned),
                })
            })
            .collect();
        devices.sort_by_key(|device| device.mode != ConnectionMode::Wired);
        devices.dedup_by(|a, b| a.path == b.path);
        devices
    }

    /// Opens and initialises the given device.
    pub fn open(&self, info: &DeviceInfo) -> Result<KoneProAir<HidTransport>> {
        let device = self.api.open_path(&info.path)?;
        KoneProAir::connect(HidTransport::new(device), info.mode)
    }

    /// Opens the first device, preferring a cable connection.
    pub fn open_first(&self) -> Result<KoneProAir<HidTransport>> {
        let info: DeviceInfo = self.devices().into_iter().next().ok_or(Error::NotFound)?;
        self.open(&info)
    }
}

/// An opened ROCCAT Kone Pro Air.
pub struct KoneProAir<T: Transport> {
    transport: T,
    mode: ConnectionMode,
}

impl<T: Transport> KoneProAir<T> {
    /// Wraps an opened vendor channel and sends the init command.
    pub fn connect(transport: T, mode: ConnectionMode) -> Result<Self> {
        let mut mouse = Self { transport, mode };
        mouse.discard_pending(0)?;
        mouse.transport.write_report(&protocol::init())?;
        // Consume the init answer so it cannot be mistaken for the reply to a
        // later request. Its content is not used and a missing answer is not fatal.
        mouse.await_reply(|reply| protocol::is_init_reply(reply).then_some(()))?;
        Ok(mouse)
    }

    pub fn mode(&self) -> ConnectionMode {
        self.mode
    }

    /// Battery state.
    pub fn battery(&mut self) -> Result<Battery> {
        self.request(
            &protocol::query_battery(),
            "battery query",
            protocol::parse_battery,
        )
    }

    /// Radio signal between mouse and dongle.
    pub fn signal(&mut self) -> Result<Signal> {
        self.request(
            &protocol::query_signal(),
            "signal query",
            protocol::parse_signal,
        )
    }

    /// Sets both LED zones. Not saved: the mouse goes back to its stored
    /// lighting after it wakes from sleep.
    pub fn set_leds(&mut self, left: Rgb, right: Rgb) -> Result<()> {
        self.transport
            .write_report(&protocol::set_leds(left, right))
    }

    /// Sends an arbitrary report and returns every reply that follows, until
    /// the channel has been quiet for 100 ms (or for 500 ms if nothing came).
    ///
    /// Debugging aid for protocol work: unknown commands may change or break
    /// the mouse configuration.
    pub fn raw(&mut self, report: &Report) -> Result<Vec<Report>> {
        self.discard_pending(0)?;
        self.transport.write_report(report)?;
        let mut replies: Vec<Report> = Vec::new();
        let mut last_activity: Instant = Instant::now();
        while replies.len() < MAX_RAW_REPLIES {
            let quiet_limit: Duration = if replies.is_empty() {
                REPLY_DEADLINE
            } else {
                RAW_QUIET_PERIOD
            };
            if last_activity.elapsed() >= quiet_limit {
                break;
            }
            if let Some(reply) = self.transport.read_report(READ_TIMEOUT_MS)? {
                replies.push(reply);
                last_activity = Instant::now();
            }
        }
        Ok(replies)
    }

    /// Sends `report` and returns the first reply `parse` accepts.
    fn request<R>(
        &mut self,
        report: &Report,
        what: &'static str,
        parse: impl Fn(&Report) -> Option<R>,
    ) -> Result<R> {
        self.discard_pending(0)?;
        self.transport.write_report(report)?;
        self.await_reply(parse)?.ok_or(Error::Timeout(what))
    }

    /// Reads until `parse` accepts a report, skipping all others.
    /// Returns `Ok(None)` when nothing matched within [`REPLY_DEADLINE`].
    fn await_reply<R>(&mut self, parse: impl Fn(&Report) -> Option<R>) -> Result<Option<R>> {
        let deadline: Instant = Instant::now() + REPLY_DEADLINE;
        while Instant::now() < deadline {
            if let Some(reply) = self.transport.read_report(READ_TIMEOUT_MS)?
                && let Some(value) = parse(&reply)
            {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Drops queued input reports so a request is not answered by an old reply.
    /// Waits up to `first_timeout_ms` for the first one.
    fn discard_pending(&mut self, first_timeout_ms: i32) -> Result<()> {
        let mut timeout_ms: i32 = first_timeout_ms;
        for _ in 0..MAX_STALE_REPORTS {
            if self.transport.read_report(timeout_ms)?.is_none() {
                break;
            }
            timeout_ms = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::protocol::report_with_prefix;

    /// Replays canned replies: each write releases the next batch of replies.
    struct MockTransport {
        written: Vec<Report>,
        batches: VecDeque<Vec<Report>>,
        pending: VecDeque<Report>,
    }

    impl MockTransport {
        fn new(batches: Vec<Vec<Report>>) -> Self {
            Self {
                written: Vec::new(),
                batches: batches.into(),
                pending: VecDeque::new(),
            }
        }
    }

    impl Transport for MockTransport {
        fn write_report(&mut self, report: &Report) -> Result<()> {
            self.written.push(*report);
            if let Some(batch) = self.batches.pop_front() {
                self.pending.extend(batch);
            }
            Ok(())
        }

        fn read_report(&mut self, _timeout_ms: i32) -> Result<Option<Report>> {
            Ok(self.pending.pop_front())
        }
    }

    fn init_reply() -> Report {
        report_with_prefix(&[0x90, 0x00, 0x03, 0x00, 0xec, 0xfa, 0x03])
    }

    fn battery_reply(raw: u8) -> Report {
        let mut reply = report_with_prefix(&[0x90, 0x0a]);
        reply[10] = raw;
        reply
    }

    fn foreign_report() -> Report {
        report_with_prefix(&[0x10, 0x10, 0x03])
    }

    /// Connects a mock whose init is answered; `batches` answer the later writes.
    fn connect(batches: Vec<Vec<Report>>) -> KoneProAir<MockTransport> {
        let mut all_batches: Vec<Vec<Report>> = vec![vec![init_reply()]];
        all_batches.extend(batches);
        KoneProAir::connect(MockTransport::new(all_batches), ConnectionMode::Wireless)
            .expect("mock connect")
    }

    #[test]
    fn connect_sends_init_and_consumes_its_reply() {
        let mouse = connect(vec![]);
        assert_eq!(mouse.transport.written, vec![protocol::init()]);
        assert!(mouse.transport.pending.is_empty());
    }

    #[test]
    fn connect_skips_reports_before_init_reply() {
        let transport = MockTransport::new(vec![vec![foreign_report(), init_reply()]]);
        let mouse = KoneProAir::connect(transport, ConnectionMode::Wireless).expect("mock connect");
        assert!(mouse.transport.pending.is_empty());
    }

    #[test]
    fn battery_skips_foreign_replies() {
        let mut mouse = connect(vec![vec![foreign_report(), battery_reply(57)]]);
        assert_eq!(mouse.battery().expect("battery").percent(), Some(57));
        assert_eq!(mouse.transport.written[1], protocol::query_battery());
    }

    #[test]
    fn many_foreign_reports_do_not_exhaust_the_request() {
        let mut batch: Vec<Report> = vec![foreign_report(); 100];
        batch.push(battery_reply(42));
        let mut mouse = connect(vec![batch]);
        assert_eq!(mouse.battery().expect("battery").percent(), Some(42));
    }

    #[test]
    fn stale_replies_are_not_used() {
        let mut mouse = connect(vec![vec![battery_reply(80)]]);
        mouse.transport.pending.push_back(battery_reply(10));
        assert_eq!(mouse.battery().expect("battery").percent(), Some(80));
    }

    #[test]
    fn missing_reply_times_out() {
        let mut mouse = connect(vec![]);
        assert!(matches!(
            mouse.signal(),
            Err(Error::Timeout("signal query"))
        ));
    }

    #[test]
    fn signal_is_parsed() {
        let reply = report_with_prefix(&[0x90, 0x70, 0, 0, 40]);
        let mut mouse = connect(vec![vec![reply]]);
        assert_eq!(mouse.signal().expect("signal").raw, 40);
    }

    #[test]
    fn set_leds_writes_led_report() {
        let mut mouse = connect(vec![]);
        let (left, right) = (Rgb::new(255, 0, 0), Rgb::new(0, 0, 255));
        mouse.set_leds(left, right).expect("set leds");
        assert_eq!(mouse.transport.written[1], protocol::set_leds(left, right));
    }

    #[test]
    fn raw_collects_all_replies() {
        let replies = vec![report_with_prefix(&[1]), report_with_prefix(&[2])];
        let mut mouse = connect(vec![replies.clone()]);
        let request = report_with_prefix(&[0xaa]);
        assert_eq!(mouse.raw(&request).expect("raw"), replies);
        assert_eq!(mouse.transport.written[1], request);
    }

    #[test]
    fn modes_match_usb_ids() {
        assert_eq!(
            ConnectionMode::from_ids(0x2c8e, 2),
            Some(ConnectionMode::Wireless)
        );
        assert_eq!(
            ConnectionMode::from_ids(0x2c92, 1),
            Some(ConnectionMode::Wired)
        );
        assert_eq!(ConnectionMode::from_ids(0x2c8e, 1), None);
        assert_eq!(ConnectionMode::from_ids(0x2c88, 2), None);
    }
}
