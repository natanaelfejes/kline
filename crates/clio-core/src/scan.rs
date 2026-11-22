//! Read-only identifier scan (policy v2 Tier 0): `21 xx` or `1A xx` over a range, rate-limited.
//!
//! A [`ScanPlan`] can only ever produce exactly two bytes, `[0x21, id]` or `[0x1A, id]`; there is
//! no way to make it emit a write, routine or output control (no brute force of `31`/`30`/`3B`).
//! One request at a time: after each [`ScanStep::Send`] the caller must [`ScanPlan::report`] the
//! result before the next one. The scan stops after [`MAX_MISSING_REPLIES`] missing replies or
//! at the first security NRC ([`SECURITY_NRCS`]). Each [`ScanRequest`] still goes through the
//! safety policy in `Session::request_scan`. The CLI runs a scan only as an explicit command and
//! logs every request and reply.

use core::ops::RangeInclusive;

use crate::frame::Reply;

/// Smallest allowed spacing between the starts of two scan requests. Coordinator decision
/// 2026-11-22 (capability catalogue 8/9.6: the shared line has no flood protection).
pub const MIN_SCAN_INTERVAL_MS: u32 = 200;
/// Missing replies (no reply, timeout, line error) after which a scan stops. Coordinator
/// decision 2026-11-22.
pub const MAX_MISSING_REPLIES: u8 = 3;
/// NRCs that stop a scan at once: securityAccessDenied 0x33, exceedNumberOfAttempts 0x35,
/// requiredTimeDelayNotExpired 0x36 (KWP2000). Coordinator decision 2026-11-22.
pub const SECURITY_NRCS: [u8; 3] = [0x33, 0x35, 0x36];

/// Which read service to scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanService {
    /// `21 xx` ReadDataByLocalIdentifier.
    LocalIdentifier,
    /// `1A xx` ReadEcuIdentification.
    EcuIdentification,
}

impl ScanService {
    fn sid(self) -> u8 {
        match self {
            ScanService::LocalIdentifier => 0x21,
            ScanService::EcuIdentification => 0x1A,
        }
    }
}

/// One scan read. Only [`ScanPlan`] makes these.
#[derive(Debug, PartialEq, Eq)]
pub struct ScanRequest {
    payload: [u8; 2],
}

impl ScanRequest {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn identifier(&self) -> u8 {
        self.payload[1]
    }
}

/// Why a scan stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStop {
    /// [`MAX_MISSING_REPLIES`] requests got no usable reply.
    MissingReplies,
    /// The ECU answered with this security NRC.
    SecurityNrc(u8),
}

/// What to do next in a scan.
#[derive(Debug, PartialEq, Eq)]
pub enum ScanStep {
    /// Send this read now.
    Send(ScanRequest),
    /// Too early; ask again after this many milliseconds.
    Wait { ms: u32 },
    /// The last request has not been reported yet.
    AwaitingReport,
    /// Every identifier of the range was issued and reported.
    Finished,
    /// The scan was stopped; nothing more is sent.
    Stopped(ScanStop),
}

/// Result of one scan request, for [`ScanPlan::report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanResult<'a> {
    /// The ECU replied (positive, negative or status).
    Reply(&'a Reply),
    /// No reply or the exchange failed.
    Missing,
}

/// Identifiers `range` of `service`, at most one request per `interval_ms` (at least
/// [`MIN_SCAN_INTERVAL_MS`]).
#[derive(Debug)]
pub struct ScanPlan {
    service: ScanService,
    next: u16,
    end: u16,
    interval_ms: u32,
    last_ms: Option<u64>,
    awaiting: bool,
    missing: u8,
    stopped: Option<ScanStop>,
}

impl ScanPlan {
    pub fn new(service: ScanService, range: RangeInclusive<u8>, interval_ms: u32) -> ScanPlan {
        ScanPlan {
            service,
            next: u16::from(*range.start()),
            end: u16::from(*range.end()),
            interval_ms: interval_ms.max(MIN_SCAN_INTERVAL_MS),
            last_ms: None,
            awaiting: false,
            missing: 0,
            stopped: None,
        }
    }

    /// Report the result of the last [`ScanStep::Send`].
    pub fn report(&mut self, result: ScanResult<'_>) {
        self.awaiting = false;
        match result {
            ScanResult::Reply(Reply::Negative { nrc, .. }) if SECURITY_NRCS.contains(nrc) => {
                self.stopped = Some(ScanStop::SecurityNrc(*nrc));
            }
            ScanResult::Reply(_) => {}
            ScanResult::Missing => {
                self.missing = self.missing.saturating_add(1);
                if self.missing >= MAX_MISSING_REPLIES {
                    self.stopped = Some(ScanStop::MissingReplies);
                }
            }
        }
    }

    pub fn interval_ms(&self) -> u32 {
        self.interval_ms
    }

    /// The next step at `now_ms`.
    pub fn next(&mut self, now_ms: u64) -> ScanStep {
        if let Some(stop) = self.stopped {
            return ScanStep::Stopped(stop);
        }
        if self.awaiting {
            return ScanStep::AwaitingReport;
        }
        if self.next > self.end {
            return ScanStep::Finished;
        }
        if let Some(last) = self.last_ms {
            let due = last + u64::from(self.interval_ms);
            if now_ms < due {
                return ScanStep::Wait {
                    ms: u32::try_from(due - now_ms).unwrap_or(u32::MAX),
                };
            }
        }
        let id = self.next as u8;
        self.next += 1;
        self.last_ms = Some(now_ms);
        self.awaiting = true;
        ScanStep::Send(ScanRequest {
            payload: [self.service.sid(), id],
        })
    }
}
