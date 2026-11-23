//! `--transport ble`: reads through the Clio Link bridge instead of a KKL cable
//! (docs/bridge-protocol.md "Host transport").
//!
//! The bridge is the only K-line master in the car; the host never sees break, echo or
//! frames, only request payloads and reply payloads over the GATT protocol of
//! `kline_bridge::gatt`. So `--transport ble` is not a [`crate::transport::Transport`]: it
//! replaces the whole link (init, session, keep-alive happen on the bridge).
//!
//! Policy, twice: this client refuses anything that is not a Tier 0 read before writing it,
//! and the bridge refuses anything but Tier 0 from the Mac's bond, which has the agent role
//! (MASTER-PLAN-v2 M12). The trait [`BleLink`] is the radio: a btleplug backend is not in this
//! build (docs/bridge-protocol.md "Host transport" says why); tests use a link to a bridge
//! running against kline-sim in the same process.

use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kline_bridge::gatt::{self, Channel, Command, Op, Response, ResultCode};
use kline_bridge::profile::EcuId;
use kline_core::safety::{self, Tier};

/// The radio side: one bonded, encrypted connection to the bridge's Clio Link service.
pub trait BleLink {
    /// Write one frame to the Command characteristic (write with response).
    fn write_command(&mut self, frame: &[u8]) -> io::Result<()>;
    /// The next notification (characteristic, frame), or `None` after `timeout_ms`.
    fn next_notification(&mut self, timeout_ms: u32) -> io::Result<Option<(Channel, Vec<u8>)>>;
}

/// Why a request over BLE failed.
#[derive(Debug)]
pub enum BleError {
    Io(io::Error),
    /// Refused here before anything was written (not a Tier 0 read).
    NotRead {
        tier: Option<Tier>,
    },
    /// The bridge answered with this result (data: the ECU's negative reply, if any).
    Bridge {
        code: ResultCode,
        data: Vec<u8>,
    },
    /// No final response within the timeout.
    Timeout,
}

impl From<io::Error> for BleError {
    fn from(e: io::Error) -> Self {
        BleError::Io(e)
    }
}

impl std::fmt::Display for BleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BleError::Io(e) => write!(f, "BLE link: {e}"),
            BleError::NotRead { tier: Some(t) } => write!(
                f,
                "not a read ({t:?}): over BLE the Mac link is Tier 0 only; use the phone app \
                 or `kline exec` on the cable"
            ),
            BleError::NotRead { tier: None } => {
                write!(f, "forbidden by the safety policy in every tier")
            }
            BleError::Bridge { code, data } => write!(f, "bridge answered {code:?} {data:02X?}"),
            BleError::Timeout => write!(f, "no answer from the bridge"),
        }
    }
}

/// Talks to one bridge.
pub struct BridgeClient {
    link: Box<dyn BleLink>,
    counter: u32,
    /// Final response wait. ours: a request may need a 5-baud init (2.5 s) plus a profile
    /// switch (another init) first.
    pub timeout_ms: u32,
}

impl BridgeClient {
    /// The counter starts at the Unix time in seconds, so it is above the last one this bond
    /// used in an earlier run (the bridge keeps the last accepted value per bond in NVS).
    pub fn new(link: Box<dyn BleLink>) -> BridgeClient {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(1);
        BridgeClient::with_counter(link, secs)
    }

    pub fn with_counter(link: Box<dyn BleLink>, counter: u32) -> BridgeClient {
        BridgeClient {
            link,
            counter,
            timeout_ms: 15_000,
        }
    }

    /// Send `op` and wait for its final response (not `Accepted`). One retry with a higher
    /// counter after `StaleCounter` (another run in the same second).
    pub fn call(&mut self, op: Op) -> Result<Response, BleError> {
        for _ in 0..2 {
            self.counter = self.counter.wrapping_add(1);
            let counter = self.counter;
            let frame = Command {
                counter,
                op: op.clone(),
            }
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            self.link.write_command(&frame)?;
            let r = self.wait_final(counter)?;
            if r.result == ResultCode::StaleCounter {
                self.counter = self.counter.wrapping_add(1 << 16);
                continue;
            }
            return Ok(r);
        }
        Err(BleError::Bridge {
            code: ResultCode::StaleCounter,
            data: Vec::new(),
        })
    }

    fn wait_final(&mut self, counter: u32) -> Result<Response, BleError> {
        // Wall-clock deadline, and a bound on silent polls for links with their own clock.
        let deadline = Instant::now() + Duration::from_millis(u64::from(self.timeout_ms));
        let step = 100;
        let mut silent_left = self.timeout_ms / step;
        while Instant::now() < deadline && silent_left > 0 {
            match self.link.next_notification(step)? {
                Some((Channel::Response, f)) => {
                    if let Ok(r) = Response::decode(&f) {
                        if r.counter == counter && r.result != ResultCode::Accepted {
                            return Ok(r);
                        }
                    }
                }
                Some(_) => {}
                None => silent_left -= 1,
            }
        }
        Err(BleError::Timeout)
    }

    /// One Tier 0 read on `ecu`; the positive reply payload (from the response SID).
    pub fn read(&mut self, ecu: EcuId, payload: &[u8]) -> Result<Vec<u8>, BleError> {
        let target = kline_bridge::gate::target(ecu);
        match safety::tier_of_in(&kline_bridge::gate::guard(ecu), &target, payload) {
            Ok(Tier::Read) => {}
            Ok(t) => return Err(BleError::NotRead { tier: Some(t) }),
            Err(_) => return Err(BleError::NotRead { tier: None }),
        }
        if payload.is_empty() || payload.len() > gatt::MAX_REQUEST {
            return Err(BleError::NotRead { tier: None });
        }
        let r = self.call(Op::Request {
            ecu,
            payload: payload.to_vec(),
        })?;
        match r.result {
            ResultCode::Ok => Ok(r.data),
            code => Err(BleError::Bridge { code, data: r.data }),
        }
    }

    /// Ask the bridge to stop its session and release the K line (`YIELD_KLINE`), so a KKL
    /// cable can be used (one tool on the cable).
    pub fn yield_kline(&mut self) -> Result<(), BleError> {
        match self.call(Op::YieldKline)?.result {
            ResultCode::Ok => Ok(()),
            code => Err(BleError::Bridge {
                code,
                data: Vec::new(),
            }),
        }
    }

    pub fn resume_kline(&mut self) -> Result<(), BleError> {
        match self.call(Op::ResumeKline)?.result {
            ResultCode::Ok => Ok(()),
            code => Err(BleError::Bridge {
                code,
                data: Vec::new(),
            }),
        }
    }

    /// The bridge's status record.
    pub fn status(&mut self) -> Result<gatt::Status, BleError> {
        let r = self.call(Op::GetStatus)?;
        gatt::Status::decode(&r.data)
            .map_err(|e| BleError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))
    }

    /// Hand back the link (tests).
    pub fn into_link(self) -> Box<dyn BleLink> {
        self.link
    }
}
