//! The Clio Link GATT protocol v1 as a pure codec (docs/bridge-protocol.md).
//!
//! One custom service with five characteristics (PLAN-RESEARCH C2). Every value written or
//! notified on them is one *frame*:
//!
//! ```text
//! [version u8 = 1][type u8][body length N u8][body: N bytes][CRC-16/CCITT-FALSE u16 LE]
//! ```
//!
//! The CRC covers version, type, length and body. Multi-byte fields in bodies are little
//! endian. A frame is at most [`MAX_FRAME`] bytes, so the central must negotiate an ATT MTU of
//! at least `MAX_FRAME + 3` (iOS and macOS negotiate 185 or more on their own); the bridge
//! refuses smaller MTUs instead of fragmenting.
//!
//! Decoding never panics on any input (proptest in `tests/gatt_codec.rs`).

use alloc::string::String;
use alloc::vec::Vec;

use crate::crc::crc16;
use crate::profile::{EcuId, Profile};

/// Protocol version byte of every frame.
pub const PROTOCOL_VERSION: u8 = 1;
/// Version, type and length bytes.
pub const HEADER_LEN: usize = 3;
/// Trailing CRC bytes.
pub const CRC_LEN: usize = 2;
/// Largest body: a whole frame fits [`MAX_FRAME`].
pub const MAX_BODY: usize = MAX_FRAME - HEADER_LEN - CRC_LEN;
/// Largest frame: 180 bytes, below the 182-byte notification payload of a 185-byte ATT MTU
/// (ATT notification header 3 bytes). ours.
pub const MAX_FRAME: usize = 180;
/// Minimum ATT MTU the bridge accepts (a frame must fit one notification). ours.
pub const MIN_ATT_MTU: u16 = MAX_FRAME as u16 + 3;
/// Longest K-line request payload a Command may carry. ours: the longest documented Tier 0/1
/// request is 5 bytes (`31 C7 00 00 00`); 32 leaves room without allowing block transfers.
pub const MAX_REQUEST: usize = 32;
/// Longest identification (`21 80` reply payload) a PREPARE_WRITE may carry. ours: a KWP
/// reply is at most 255 bytes, but every Renault `21 80` of this car is far shorter; 64 keeps
/// the frame within [`MAX_FRAME`].
pub const MAX_IDENTIFICATION: usize = 64;
/// Longest approval code a Confirm may carry (kline-core codes are 6 or 10 characters).
pub const MAX_CODE: usize = 16;

/// 128-bit UUIDs, generated once for this project (2026-11-23).
pub mod uuid {
    /// The Clio Link service.
    pub const SERVICE: &str = "b3340ce9-0893-4302-b20a-d4f96eb056ee";
    /// Live (notify): [`super::LiveSample`].
    pub const LIVE: &str = "b3340cea-0893-4302-b20a-d4f96eb056ee";
    /// Status (read, notify): [`super::Status`].
    pub const STATUS: &str = "b3340ceb-0893-4302-b20a-d4f96eb056ee";
    /// Command (write with response): [`super::Command`].
    pub const COMMAND: &str = "b3340cec-0893-4302-b20a-d4f96eb056ee";
    /// Response (notify, to the writing client only): [`super::Response`].
    pub const RESPONSE: &str = "b3340ced-0893-4302-b20a-d4f96eb056ee";
    /// Raw (notify, off until `RAW_STREAM 1`): [`super::RawFrame`].
    pub const RAW: &str = "b3340cee-0893-4302-b20a-d4f96eb056ee";
}

/// The characteristic a frame travels on; also the frame's type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    Live = 0x01,
    Status = 0x02,
    Command = 0x03,
    Response = 0x04,
    Raw = 0x05,
    /// An extra-frame notification ([`ExtraFrame`]). It travels on the Live characteristic,
    /// only while live extras are on (`LIVE_EXTRAS`), so a v1 app that never asks for them
    /// never receives one.
    Extra = 0x06,
}

impl Channel {
    pub fn from_u8(b: u8) -> Option<Channel> {
        Some(match b {
            0x01 => Channel::Live,
            0x02 => Channel::Status,
            0x03 => Channel::Command,
            0x04 => Channel::Response,
            0x05 => Channel::Raw,
            0x06 => Channel::Extra,
            _ => return None,
        })
    }
}

/// Why bytes are not a valid frame or message. Nothing is executed for an invalid frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("frame shorter than header and CRC")]
    Truncated,
    #[error("unsupported protocol version {0}")]
    BadVersion(u8),
    #[error("unknown frame type {0:#04x}")]
    UnknownType(u8),
    #[error("length byte does not match the frame size")]
    LengthMismatch,
    #[error("body longer than {MAX_BODY} bytes")]
    BodyTooLong,
    #[error("CRC mismatch")]
    BadCrc,
    #[error("frame type {got:?} where {want:?} was expected")]
    WrongType { want: Channel, got: Channel },
    #[error("unknown command opcode {0:#04x}")]
    BadOpcode(u8),
    #[error("bad arguments for this message")]
    BadArgs,
}

/// Frame `body` as a `channel` frame.
pub fn encode_frame(channel: Channel, body: &[u8]) -> Result<Vec<u8>, CodecError> {
    if body.len() > MAX_BODY {
        return Err(CodecError::BodyTooLong);
    }
    let mut out = Vec::with_capacity(HEADER_LEN + body.len() + CRC_LEN);
    out.push(PROTOCOL_VERSION);
    out.push(channel as u8);
    out.push(body.len() as u8);
    out.extend_from_slice(body);
    let crc = crc16(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Check a frame and return its channel and body.
pub fn decode_frame(bytes: &[u8]) -> Result<(Channel, &[u8]), CodecError> {
    if bytes.len() < HEADER_LEN + CRC_LEN {
        return Err(CodecError::Truncated);
    }
    if bytes.len() > MAX_FRAME {
        return Err(CodecError::BodyTooLong);
    }
    if bytes[0] != PROTOCOL_VERSION {
        return Err(CodecError::BadVersion(bytes[0]));
    }
    let n = bytes[2] as usize;
    if HEADER_LEN + n + CRC_LEN != bytes.len() {
        return Err(CodecError::LengthMismatch);
    }
    let (data, crc) = bytes.split_at(HEADER_LEN + n);
    if crc16(data) != u16::from_le_bytes([crc[0], crc[1]]) {
        return Err(CodecError::BadCrc);
    }
    let channel = Channel::from_u8(bytes[1]).ok_or(CodecError::UnknownType(bytes[1]))?;
    Ok((channel, &data[HEADER_LEN..]))
}

fn expect(bytes: &[u8], want: Channel) -> Result<&[u8], CodecError> {
    let (got, body) = decode_frame(bytes)?;
    if got != want {
        return Err(CodecError::WrongType { want, got });
    }
    Ok(body)
}

/// Little-endian reader over a body that fails with [`CodecError::BadArgs`].
struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.0.len() < n {
            return Err(CodecError::BadArgs);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn rest(&mut self) -> &'a [u8] {
        let r = self.0;
        self.0 = &[];
        r
    }
    fn end(&self) -> Result<(), CodecError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(CodecError::BadArgs)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Live

/// Flag bits of [`LiveSample::flags`].
pub mod live_flags {
    /// `21AA` decoded at least once in this session (rpm, injection, fuel are valid).
    pub const AA_VALID: u8 = 0x01;
    /// `21A9` decoded at least once in this session (speed, coolant, IAT, MAP, battery valid).
    pub const A9_VALID: u8 = 0x02;
    /// Engine running (rpm > 0 in the latest frame).
    pub const ENGINE_RUNNING: u8 = 0x04;
    /// Lock-when-moving is armed.
    pub const LWM_ARMED: u8 = 0x08;
    /// Lock-when-moving locked the doors and has not unlocked them yet.
    pub const LWM_LOCKED: u8 = 0x10;
    /// Live extras are on (`LIVE_EXTRAS` with a non-zero interval): Extra frames follow.
    pub const EXTRAS_ON: u8 = 0x20;
}

/// The packed v1 live struct (PLAN-RESEARCH C2), 20 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveSample {
    pub seq: u16,
    /// Bridge uptime in ms when the newest frame was decoded.
    pub t_ms: u32,
    pub rpm: u16,
    pub speed_kmh: u8,
    /// Injection time in microseconds.
    pub inj_us: u16,
    /// Instantaneous fuel consumption in mL/h (Sirius CONSLH).
    pub fuel_mlh: u16,
    pub coolant_c: i8,
    pub iat_c: i8,
    pub map_mbar: u16,
    /// Battery voltage in centivolts (Sirius measurement).
    pub batt_cv: u16,
    pub flags: u8,
}

impl LiveSample {
    pub const LEN: usize = 20;

    pub fn body(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(Self::LEN);
        b.extend_from_slice(&self.seq.to_le_bytes());
        b.extend_from_slice(&self.t_ms.to_le_bytes());
        b.extend_from_slice(&self.rpm.to_le_bytes());
        b.push(self.speed_kmh);
        b.extend_from_slice(&self.inj_us.to_le_bytes());
        b.extend_from_slice(&self.fuel_mlh.to_le_bytes());
        b.push(self.coolant_c as u8);
        b.push(self.iat_c as u8);
        b.extend_from_slice(&self.map_mbar.to_le_bytes());
        b.extend_from_slice(&self.batt_cv.to_le_bytes());
        b.push(self.flags);
        b
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_frame(Channel::Live, &self.body()).expect("20-byte body fits")
    }

    pub fn decode(frame: &[u8]) -> Result<LiveSample, CodecError> {
        let body = expect(frame, Channel::Live)?;
        let mut r = Rd(body);
        let s = LiveSample {
            seq: r.u16()?,
            t_ms: r.u32()?,
            rpm: r.u16()?,
            speed_kmh: r.u8()?,
            inj_us: r.u16()?,
            fuel_mlh: r.u16()?,
            coolant_c: r.u8()? as i8,
            iat_c: r.u8()? as i8,
            map_mbar: r.u16()?,
            batt_cv: r.u16()?,
            flags: r.u8()?,
        };
        r.end()?;
        Ok(s)
    }
}

// ---------------------------------------------------------------------------------------------
// Status

/// What the bridge is doing, as reported in [`Status::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BridgeState {
    /// No K-line session, transceiver off, awake.
    Idle = 0,
    /// A 5-baud init is running.
    Init = 1,
    /// A diagnostic session is open.
    Session = 2,
    /// Waiting before the next init attempt.
    Backoff = 3,
    /// The K line was released for another tester (`YIELD_KLINE`).
    Yielded = 4,
    /// Low-power sleep (only reported on the way down).
    Sleep = 5,
}

impl BridgeState {
    pub fn from_u8(b: u8) -> Option<BridgeState> {
        Some(match b {
            0 => BridgeState::Idle,
            1 => BridgeState::Init,
            2 => BridgeState::Session,
            3 => BridgeState::Backoff,
            4 => BridgeState::Yielded,
            5 => BridgeState::Sleep,
            _ => return None,
        })
    }
}

/// The door lock state the BMT reports (`21 A3` data byte 0 bit 7, DCODCOND).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum LockState {
    #[default]
    Unknown = 0,
    Locked = 1,
    Unlocked = 2,
}

impl LockState {
    pub fn from_u8(b: u8) -> Option<LockState> {
        Some(match b {
            0 => LockState::Unknown,
            1 => LockState::Locked,
            2 => LockState::Unlocked,
            _ => return None,
        })
    }
}

/// Flag bits of [`Status::flags`].
pub mod status_flags {
    pub const LWM_ARMED: u8 = 0x01;
    pub const LWM_LOCKED: u8 = 0x02;
    pub const ENGINE_RUNNING: u8 = 0x04;
    /// This build can accept Tier 1 approvals from the owner (feature `approvals`).
    pub const APPROVALS: u8 = 0x08;
    /// A front door is open (BMT `21 A0` data byte 2 bit 0x02, NOT VERIFIED).
    pub const DOOR_FRONT_OPEN: u8 = 0x10;
    /// A rear door is open (BMT `21 A0` data byte 2 bit 0x04, NOT VERIFIED).
    pub const DOOR_REAR_OPEN: u8 = 0x20;
    /// Battery below the write limit (12.0 V, MASTER-PLAN-v2 P6): no Tier 1 and no LOCK/UNLOCK.
    pub const LOW_BATTERY: u8 = 0x40;
    /// Lock-when-moving gave up an unlock (the doors may still be locked) and disarmed itself;
    /// cleared only when the owner arms again or acknowledges it (LWM_ACK).
    pub const LWM_GAVE_UP: u8 = 0x80;
}

/// Bridge status, 14 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub fw_major: u8,
    pub fw_minor: u8,
    pub state: BridgeState,
    /// Address of the ECU of the open session or running init, 0 for none.
    pub ecu: u8,
    pub profile: Profile,
    /// Battery voltage from the bridge's own divider, centivolts (0 = not measured).
    pub batt_cv: u16,
    pub lock: LockState,
    pub flags: u8,
    /// Last error ([`ResultCode`] value), 0 for none.
    pub last_error: u8,
    pub uptime_s: u32,
    /// Battery state of charge estimated by the bridge (`crate::soc`), percent, or
    /// [`SOC_UNKNOWN`].
    pub soc_pct: u8,
    /// Confidence of `soc_pct`: 0 none, 1 approximate, 2 rested (`crate::soc::Confidence`).
    pub soc_conf: u8,
}

/// `Status::soc_pct` when no estimate exists.
pub const SOC_UNKNOWN: u8 = 0xFF;

impl Status {
    pub const LEN: usize = 16;
    /// The 14-byte Status of firmware before the battery estimate; still decoded (estimate
    /// unknown).
    pub const LEN_V1_0: usize = 14;

    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(Self::LEN);
        b.push(self.fw_major);
        b.push(self.fw_minor);
        b.push(self.state as u8);
        b.push(self.ecu);
        b.push(self.profile as u8);
        b.extend_from_slice(&self.batt_cv.to_le_bytes());
        b.push(self.lock as u8);
        b.push(self.flags);
        b.push(self.last_error);
        b.extend_from_slice(&self.uptime_s.to_le_bytes());
        b.push(self.soc_pct);
        b.push(self.soc_conf);
        encode_frame(Channel::Status, &b).expect("16-byte body fits")
    }

    pub fn decode(frame: &[u8]) -> Result<Status, CodecError> {
        let body = expect(frame, Channel::Status)?;
        let mut r = Rd(body);
        let s = Status {
            fw_major: r.u8()?,
            fw_minor: r.u8()?,
            state: BridgeState::from_u8(r.u8()?).ok_or(CodecError::BadArgs)?,
            ecu: r.u8()?,
            profile: Profile::from_u8(r.u8()?).ok_or(CodecError::BadArgs)?,
            batt_cv: r.u16()?,
            lock: LockState::from_u8(r.u8()?).ok_or(CodecError::BadArgs)?,
            flags: r.u8()?,
            last_error: r.u8()?,
            uptime_s: r.u32()?,
            soc_pct: SOC_UNKNOWN,
            soc_conf: 0,
        };
        let mut s = s;
        if !r.0.is_empty() {
            s.soc_pct = r.u8()?;
            s.soc_conf = r.u8()?;
            if s.soc_conf > 2 || (s.soc_pct > 100 && s.soc_pct != SOC_UNKNOWN) {
                return Err(CodecError::BadArgs);
            }
        }
        r.end()?;
        Ok(s)
    }
}

// ---------------------------------------------------------------------------------------------
// Command

/// Command opcodes (docs/bridge-protocol.md "Command").
pub mod opcode {
    pub const START_LIVE: u8 = 0x01;
    pub const STOP_LIVE: u8 = 0x02;
    pub const REQUEST: u8 = 0x03;
    pub const PREPARE: u8 = 0x04;
    pub const CONFIRM: u8 = 0x05;
    pub const LOCK: u8 = 0x06;
    pub const UNLOCK: u8 = 0x07;
    pub const YIELD_KLINE: u8 = 0x08;
    pub const RESUME_KLINE: u8 = 0x09;
    pub const SET_TIME: u8 = 0x0A;
    pub const LWM_ARM: u8 = 0x0B;
    pub const LWM_DISARM: u8 = 0x0C;
    pub const RAW_STREAM: u8 = 0x0D;
    pub const GET_STATUS: u8 = 0x0E;
    pub const PREPARE_WRITE: u8 = 0x0F;
    pub const LWM_ACK: u8 = 0x10;
    pub const LIVE_EXTRAS: u8 = 0x11;
    pub const READ_BODY_ONCE: u8 = 0x12;
}

/// Largest interval of `LIVE_EXTRAS` (drive cycles between two extra reads). ours: 16 cycles
/// is about 3 s per extra read at the measured rate, so each of the extra frames still comes
/// about every 20 s.
pub const MAX_EXTRA_EVERY: u8 = 16;

/// What a Command asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Poll a profile continuously: [`Profile::Drive`] or [`Profile::Parked`].
    StartLive(Profile),
    /// Stop polling (the session ends when nothing else needs it).
    StopLive,
    /// One request; it must be Tier 0 (any role).
    Request { ecu: EcuId, payload: Vec<u8> },
    /// Ask for an approval challenge for one Tier 1 or Tier 3 request (owner only, feature
    /// `approvals`).
    Prepare { ecu: EcuId, payload: Vec<u8> },
    /// Ask for an approval challenge for one Tier 2 config write, with the exact `21 80` reply
    /// the app chose its ECU definition from (kline-core compares it with the session's latest
    /// `21 80` reply, like the CLI).
    PrepareWrite {
        ecu: EcuId,
        identification: Vec<u8>,
        payload: Vec<u8>,
    },
    /// Answer a challenge with the code the owner typed (owner only).
    Confirm { challenge: u32, code: String },
    /// Lock the parked car with the fob output (owner only).
    Lock,
    /// Unlock the parked car with the fob output (owner only).
    Unlock,
    /// End the session and release the K line for another tester.
    YieldKline,
    /// Take the K line back after `YieldKline`.
    ResumeKline,
    /// Wall clock, Unix seconds (for logs only; nothing depends on it).
    SetTime(u32),
    /// Arm lock-when-moving (owner only, feature `standing-lock`).
    LwmArm,
    /// Disarm lock-when-moving (owner only).
    LwmDisarm,
    /// The owner acknowledges a lock-when-moving give-up (status flag `LWM_GAVE_UP`).
    LwmAck,
    /// Raw frame notifications on or off.
    RawStream(bool),
    /// Live extras (the "advanced" drive profile): 0 off, `n` = one extra Sirius read
    /// (`profile::EXTRA_SCHEDULE`, in turn) after every `n` drive cycles, plus the raw
    /// `21 A9` and one `21 AA` per cycle, all as [`ExtraFrame`]s. Reads only (Tier 0).
    LiveExtras(u8),
    /// Answer with one Status frame on Response.
    GetStatus,
    /// One body read at a standstill with the engine running (owner decision 2026-11-25): the
    /// bridge leaves the Sirius, reads BMT `21 A0` and `21 A3`, publishes both as Extra frames
    /// (ECU `26`) and goes back. Reads only; at most once per stop.
    ReadBodyOnce,
}

/// A command frame: a per-client counter that must strictly increase, and the operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub counter: u32,
    pub op: Op,
}

impl Command {
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.counter.to_le_bytes());
        match &self.op {
            Op::StartLive(p) => {
                b.push(opcode::START_LIVE);
                b.push(*p as u8);
            }
            Op::StopLive => b.push(opcode::STOP_LIVE),
            Op::Request { ecu, payload } | Op::Prepare { ecu, payload } => {
                if payload.is_empty() || payload.len() > MAX_REQUEST {
                    return Err(CodecError::BadArgs);
                }
                b.push(if matches!(self.op, Op::Request { .. }) {
                    opcode::REQUEST
                } else {
                    opcode::PREPARE
                });
                b.push(ecu.address());
                b.extend_from_slice(payload);
            }
            Op::PrepareWrite {
                ecu,
                identification,
                payload,
            } => {
                if payload.is_empty()
                    || payload.len() > MAX_REQUEST
                    || identification.is_empty()
                    || identification.len() > MAX_IDENTIFICATION
                {
                    return Err(CodecError::BadArgs);
                }
                b.push(opcode::PREPARE_WRITE);
                b.push(ecu.address());
                b.push(identification.len() as u8);
                b.extend_from_slice(identification);
                b.extend_from_slice(payload);
            }
            Op::Confirm { challenge, code } => {
                if !valid_code(code.as_bytes()) {
                    return Err(CodecError::BadArgs);
                }
                b.push(opcode::CONFIRM);
                b.extend_from_slice(&challenge.to_le_bytes());
                b.extend_from_slice(code.as_bytes());
            }
            Op::Lock => b.push(opcode::LOCK),
            Op::Unlock => b.push(opcode::UNLOCK),
            Op::YieldKline => b.push(opcode::YIELD_KLINE),
            Op::ResumeKline => b.push(opcode::RESUME_KLINE),
            Op::SetTime(t) => {
                b.push(opcode::SET_TIME);
                b.extend_from_slice(&t.to_le_bytes());
            }
            Op::LwmArm => b.push(opcode::LWM_ARM),
            Op::LwmDisarm => b.push(opcode::LWM_DISARM),
            Op::LwmAck => b.push(opcode::LWM_ACK),
            Op::RawStream(on) => {
                b.push(opcode::RAW_STREAM);
                b.push(u8::from(*on));
            }
            Op::GetStatus => b.push(opcode::GET_STATUS),
            Op::LiveExtras(every) => {
                if *every > MAX_EXTRA_EVERY {
                    return Err(CodecError::BadArgs);
                }
                b.push(opcode::LIVE_EXTRAS);
                b.push(*every);
            }
            Op::ReadBodyOnce => b.push(opcode::READ_BODY_ONCE),
        }
        encode_frame(Channel::Command, &b)
    }

    pub fn decode(frame: &[u8]) -> Result<Command, CodecError> {
        let body = expect(frame, Channel::Command)?;
        let mut r = Rd(body);
        let counter = r.u32()?;
        let code = r.u8()?;
        let op = match code {
            opcode::START_LIVE => {
                let p = Profile::from_u8(r.u8()?).ok_or(CodecError::BadArgs)?;
                if !matches!(p, Profile::Drive | Profile::Parked) {
                    return Err(CodecError::BadArgs);
                }
                Op::StartLive(p)
            }
            opcode::STOP_LIVE => Op::StopLive,
            opcode::REQUEST | opcode::PREPARE => {
                let ecu = EcuId::from_address(r.u8()?).ok_or(CodecError::BadArgs)?;
                let payload = r.rest().to_vec();
                if payload.is_empty() || payload.len() > MAX_REQUEST {
                    return Err(CodecError::BadArgs);
                }
                if code == opcode::REQUEST {
                    Op::Request { ecu, payload }
                } else {
                    Op::Prepare { ecu, payload }
                }
            }
            opcode::PREPARE_WRITE => {
                let ecu = EcuId::from_address(r.u8()?).ok_or(CodecError::BadArgs)?;
                let n = r.u8()? as usize;
                if n == 0 || n > MAX_IDENTIFICATION {
                    return Err(CodecError::BadArgs);
                }
                let identification = r.take(n)?.to_vec();
                let payload = r.rest().to_vec();
                if payload.is_empty() || payload.len() > MAX_REQUEST {
                    return Err(CodecError::BadArgs);
                }
                Op::PrepareWrite {
                    ecu,
                    identification,
                    payload,
                }
            }
            opcode::CONFIRM => {
                let challenge = r.u32()?;
                let c = r.rest();
                if !valid_code(c) {
                    return Err(CodecError::BadArgs);
                }
                Op::Confirm {
                    challenge,
                    code: c.iter().map(|&b| b as char).collect(),
                }
            }
            opcode::LOCK => Op::Lock,
            opcode::UNLOCK => Op::Unlock,
            opcode::YIELD_KLINE => Op::YieldKline,
            opcode::RESUME_KLINE => Op::ResumeKline,
            opcode::SET_TIME => Op::SetTime(r.u32()?),
            opcode::LWM_ARM => Op::LwmArm,
            opcode::LWM_DISARM => Op::LwmDisarm,
            opcode::LWM_ACK => Op::LwmAck,
            opcode::RAW_STREAM => match r.u8()? {
                0 => Op::RawStream(false),
                1 => Op::RawStream(true),
                _ => return Err(CodecError::BadArgs),
            },
            opcode::GET_STATUS => Op::GetStatus,
            opcode::LIVE_EXTRAS => {
                let every = r.u8()?;
                if every > MAX_EXTRA_EVERY {
                    return Err(CodecError::BadArgs);
                }
                Op::LiveExtras(every)
            }
            opcode::READ_BODY_ONCE => Op::ReadBodyOnce,
            other => return Err(CodecError::BadOpcode(other)),
        };
        r.end()?;
        Ok(Command { counter, op })
    }
}

/// 1..=[`MAX_CODE`] printable ASCII characters.
fn valid_code(c: &[u8]) -> bool {
    !c.is_empty() && c.len() <= MAX_CODE && c.iter().all(|b| b.is_ascii_graphic())
}

// ---------------------------------------------------------------------------------------------
// Response

/// Result of a command (docs/bridge-protocol.md "Result codes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResultCode {
    /// Done. Data: the positive reply payload for REQUEST/CONFIRM, else empty.
    Ok = 0x00,
    /// Queued; a second Response with the same counter follows.
    Accepted = 0x01,
    /// The frame did not decode (CRC, version, length, opcode, arguments).
    BadFrame = 0x02,
    /// Counter not greater than the last accepted one of this client.
    StaleCounter = 0x03,
    /// Too many commands (or LOCK/UNLOCK) in the window.
    RateLimited = 0x04,
    /// This client's role may not do this (agent role: Tier 0 only; unbonded: nothing).
    NotAllowedForRole = 0x05,
    /// Forbidden by kline-core in every tier (immobilizer, protocol frames).
    Forbidden = 0x06,
    /// Not Tier 0: use PREPARE and CONFIRM (owner only).
    ApprovalRequired = 0x07,
    /// Wrong code, unknown or expired challenge, or kline-core refused the approval.
    ApprovalMismatch = 0x08,
    /// Battery below 12.0 V: no writes, no LOCK/UNLOCK (MASTER-PLAN-v2 P6).
    LowBattery = 0x09,
    /// The queue is full or another challenge is pending.
    Busy = 0x0A,
    /// The K line failed (init, echo, timeout). Data: empty.
    LinkError = 0x0B,
    /// The ECU answered negatively. Data: `7F sid nrc`.
    NegativeReply = 0x0C,
    /// The K line is yielded; RESUME_KLINE first.
    Yielded = 0x0D,
    /// The lock state could not be read, or did not change after the pulse.
    LockNotConfirmed = 0x0E,
    /// Not in this build (approvals or standing-lock feature off), or not over BLE (Tier 2/3).
    Unsupported = 0x0F,
    /// A challenge was made. Data: challenge id u32 LE, tier u8, then the code (ASCII).
    Challenge = 0x10,
    /// Ignition on / engine running: the fob path cannot work (the BMT ignores RF then).
    IgnitionOn = 0x11,
    /// A non-read while the car moves (fresh speed above 0, or the Sirius with the engine
    /// running and no fresh speed).
    VehicleMoving = 0x12,
    /// Lock-when-moving needs the Sirius now (armed with the engine running, or an unlock
    /// owed): no profile switch, no other ECU, no yield.
    LwmActive = 0x13,
    /// The ECU's reply does not fit one Response frame; it was received but is not returned.
    ReplyTooLong = 0x14,
    /// The session the challenge was made on has ended; nothing was sent. PREPARE again.
    SessionLost = 0x15,
    /// A Tier 2 config write needs PREPARE_WRITE with the identification.
    IdentificationRequired = 0x16,
}

impl ResultCode {
    pub fn from_u8(b: u8) -> Option<ResultCode> {
        use ResultCode::*;
        Some(match b {
            0x00 => Ok,
            0x01 => Accepted,
            0x02 => BadFrame,
            0x03 => StaleCounter,
            0x04 => RateLimited,
            0x05 => NotAllowedForRole,
            0x06 => Forbidden,
            0x07 => ApprovalRequired,
            0x08 => ApprovalMismatch,
            0x09 => LowBattery,
            0x0A => Busy,
            0x0B => LinkError,
            0x0C => NegativeReply,
            0x0D => Yielded,
            0x0E => LockNotConfirmed,
            0x0F => Unsupported,
            0x10 => Challenge,
            0x11 => IgnitionOn,
            0x12 => VehicleMoving,
            0x13 => LwmActive,
            0x14 => ReplyTooLong,
            0x15 => SessionLost,
            0x16 => IdentificationRequired,
            _ => return None,
        })
    }
}

/// Answer to one Command, sent on Response to the client that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The command's counter (0 when the command did not decode).
    pub counter: u32,
    pub result: ResultCode,
    pub data: Vec<u8>,
}

/// Largest `data` of a [`Response`].
pub const MAX_RESPONSE_DATA: usize = MAX_BODY - 5;
/// Largest `bytes` of a [`RawFrame`].
pub const MAX_RAW_BYTES: usize = MAX_BODY - 6;

impl Response {
    /// The frame, or `BodyTooLong` when `data` exceeds [`MAX_RESPONSE_DATA`] (never truncated).
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let mut b = Vec::with_capacity(5 + self.data.len());
        b.extend_from_slice(&self.counter.to_le_bytes());
        b.push(self.result as u8);
        b.extend_from_slice(&self.data);
        encode_frame(Channel::Response, &b)
    }

    pub fn decode(frame: &[u8]) -> Result<Response, CodecError> {
        let body = expect(frame, Channel::Response)?;
        let mut r = Rd(body);
        Ok(Response {
            counter: r.u32()?,
            result: ResultCode::from_u8(r.u8()?).ok_or(CodecError::BadArgs)?,
            data: r.rest().to_vec(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Raw

/// Direction of a [`RawFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RawDir {
    /// Bytes the bridge wrote on the K line.
    Tx = 0,
    /// Bytes the bridge read (echo included, as the kline JSONL capture does).
    Rx = 1,
}

/// One K-line write or read, for debugging (same meaning as a kline JSONL capture line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub t_ms: u32,
    pub dir: RawDir,
    /// ECU address of the session (0 during none).
    pub ecu: u8,
    pub bytes: Vec<u8>,
}

impl RawFrame {
    /// The frame, or `BodyTooLong` when `bytes` exceeds [`MAX_RAW_BYTES`] (never truncated;
    /// the bridge splits longer reads into several frames).
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let mut b = Vec::with_capacity(6 + self.bytes.len());
        b.extend_from_slice(&self.t_ms.to_le_bytes());
        b.push(self.dir as u8);
        b.push(self.ecu);
        b.extend_from_slice(&self.bytes);
        encode_frame(Channel::Raw, &b)
    }

    pub fn decode(frame: &[u8]) -> Result<RawFrame, CodecError> {
        let body = expect(frame, Channel::Raw)?;
        let mut r = Rd(body);
        Ok(RawFrame {
            t_ms: r.u32()?,
            dir: match r.u8()? {
                0 => RawDir::Tx,
                1 => RawDir::Rx,
                _ => return Err(CodecError::BadArgs),
            },
            ecu: r.u8()?,
            bytes: r.rest().to_vec(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Extra

/// One extra read of the live profile, raw (docs/bridge-protocol.md "Extra"): the app decodes
/// it. Body: `[seq u16][t_ms u32][ecu u8][request length u8][request][reply]`, where the reply
/// is the positive reply payload from the response SID (`61 lid ..`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraFrame {
    /// +1 per Extra notification (its own counter, not the Live `seq`).
    pub seq: u16,
    /// Bridge uptime when the reply was decoded.
    pub t_ms: u32,
    /// ECU address (`7A` Sirius).
    pub ecu: u8,
    /// The request that was sent, e.g. `21 A0`.
    pub request: Vec<u8>,
    /// The positive reply payload.
    pub reply: Vec<u8>,
}

/// Fixed part of an [`ExtraFrame`] body.
pub const EXTRA_HEADER: usize = 8;

impl ExtraFrame {
    /// The frame, or `BodyTooLong` when request and reply do not fit one frame (never
    /// truncated: the bridge then sends nothing for that read).
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        if self.request.is_empty() || self.request.len() > MAX_REQUEST {
            return Err(CodecError::BadArgs);
        }
        let mut b = Vec::with_capacity(EXTRA_HEADER + self.request.len() + self.reply.len());
        b.extend_from_slice(&self.seq.to_le_bytes());
        b.extend_from_slice(&self.t_ms.to_le_bytes());
        b.push(self.ecu);
        b.push(self.request.len() as u8);
        b.extend_from_slice(&self.request);
        b.extend_from_slice(&self.reply);
        encode_frame(Channel::Extra, &b)
    }

    pub fn decode(frame: &[u8]) -> Result<ExtraFrame, CodecError> {
        let body = expect(frame, Channel::Extra)?;
        let mut r = Rd(body);
        let seq = r.u16()?;
        let t_ms = r.u32()?;
        let ecu = r.u8()?;
        let n = r.u8()? as usize;
        if n == 0 || n > MAX_REQUEST {
            return Err(CodecError::BadArgs);
        }
        let request = r.take(n)?.to_vec();
        Ok(ExtraFrame {
            seq,
            t_ms,
            ecu,
            request,
            reply: r.rest().to_vec(),
        })
    }
}
