//! K-line timing and link parameters as plain data.
//!
//! Every constant cites its source (see docs/protocol.md):
//! - `RE: IOTBD32V3 ...`: behaviour of DDT2000's native K-line DLL (reverse engineered).
//! - `RE: KWP2000Diag4 ...`: DDT2000's protocol classes (reverse engineered).
//! - `XML <K>`: the ECU definition file.
//! - `ours`: a choice of this project, where DDT2000 has no equivalent.

/// Named constants. Source tags are in each doc comment.
pub mod consts {
    /// Line speed. RE: IOTBD32V3 Setup (DCB 10400 8N1). DDT2000 effectively stays at 10400
    /// even when the XML says 10417 (RE: IOTBDSetBaudRate is not applied before the worker starts).
    pub const DEFAULT_BAUD: u32 = 10_400;
    /// One 5-baud bit. RE: IOTBD32V3 5-baud routine.
    pub const FIVE_BAUD_BIT_MS: u32 = 200;
    /// Wait after releasing the line for the 5-baud stop bit, before reading. RE: IOTBD32V3.
    pub const FIVE_BAUD_STOP_MS: u32 = 100;
    /// Added to KW2Timeout for the 0x55/KW1/KW2 and ~KW2 reads. RE: IOTBD32V3 5-baud routine.
    pub const KW2_READ_MARGIN_MS: u32 = 100;
    /// Sleep before the fast-init low pulse. RE: IOTBD32V3 fast init.
    pub const FAST_INIT_PRE_MS: u32 = 1;
    /// Fast-init low pulse (TiniL). RE: IOTBD32V3 fast init.
    pub const FAST_INIT_LOW_MS: u32 = 25;
    /// Fast-init high time before StartCommunication. RE: IOTBD32V3 fast init.
    pub const FAST_INIT_HIGH_MS: u32 = 25;
    /// Inter-byte delay forced by Multipoint. RE: KWP2000Diag4 KWP2000Ecu Multipoint init.
    pub const MULTIPOINT_INTER_BYTE_MS: u32 = 7;
    /// XOR applied to the 5-baud address in Multipoint. RE: KWP2000Diag4 KWP2000Ecu Multipoint init.
    pub const MULTIPOINT_ADDRESS_XOR: u8 = 0x80;
    /// Tester (source) address in KWP headers. RE: KWP2000Diag4 KWP2000Ecu default ToolAddress.
    pub const TOOL_ADDRESS: u8 = 0xF1;
    /// Expected first byte after a 5-baud address (ISO 9141 sync pattern). Logged, not enforced
    /// (RE: IOTBD32V3 does not check it).
    pub const SYNC_BYTE: u8 = 0x55;
    /// Idle K line before an init. ours: ISO 9141/14230 W5/Tidle; DDT2000 relies on the 300 ms
    /// tail of its previous worker thread (RE: IOTBD32V3 thread exit).
    pub const IDLE_BEFORE_INIT_MS: u32 = 300;
    /// Read timeout after a 0x78 (responsePending) reply. ours: KWP2000 P2* extended timing;
    /// DDT2000 does not handle 0x78 at all (RE: KWP2000Diag4 parser).
    pub const RESPONSE_PENDING_TIMEOUT_MS: u32 = 5_000;
    /// Maximum consecutive 0x78 replies before giving up. ours.
    pub const MAX_RESPONSE_PENDING: u8 = 10;
}

/// How the link is initialised and framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitKind {
    /// `<K><ISO8>`: ISO 9141 5-baud init, ISO8 framing `[N][data][cs]`, no ~KW2 handshake
    /// (Safir). RE: IOTBD32V3 worker skips the handshake when its ISO8 flag is set.
    Iso8FiveBaud,
    /// `<K><KWP><ISO8>`: KWP2000 after a 5-baud init with the ~KW2 / ~address handshake
    /// (Sirius 32, BMT). `multipoint` is `KWP@Multipoint`.
    KwpFiveBaud { multipoint: bool },
    /// `<K><KWP><FastInit>`: KWP2000 fast init (wake-up pattern + StartCommunication).
    KwpFast { multipoint: bool },
}

impl InitKind {
    /// `KWP@Multipoint="1"`.
    pub fn multipoint(self) -> bool {
        match self {
            InitKind::Iso8FiveBaud => false,
            InitKind::KwpFiveBaud { multipoint } | InitKind::KwpFast { multipoint } => multipoint,
        }
    }

    /// ISO8 framing (as opposed to KWP2000 framing).
    pub fn is_iso8(self) -> bool {
        matches!(self, InitKind::Iso8FiveBaud)
    }
}

/// Timing of one K-line link, all in milliseconds except `baud`.
///
/// `Default` gives the IOTBD32V3 context defaults (RE: IOTBD32V3 allocate). The ECU definition
/// overrides them (`spec` feature, or by hand).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KTiming {
    /// Line speed in bit/s. Default 10400 even when `K@BaudRate` says 10417 (see
    /// [`consts::DEFAULT_BAUD`]); set it to the XML value to test the other speed.
    pub baud: u32,
    pub init: InitKind,
    /// W4: wait after KW2 before sending ~KW2. `KWP/KW2_@Delay`, default 25.
    pub kw2_delay_ms: u32,
    /// Wait after a completed 5-baud init before the first frame. `FirstRequest@Delay`, default 20.
    pub first_request_ms: u32,
    /// Gap between transmitted bytes. `InterByte@Delay`, default 0 (whole frame in one write).
    /// Multipoint forces 7, see [`LinkConfig::effective_timing`].
    pub inter_byte_ms: u32,
    /// P3: minimum gap between a reply and the next request. `ReplyToRequest@Delay`, default 5.
    pub reply_to_request_ms: u32,
    /// Timeout of each read (echo, first reply byte, rest of reply). `RequestToReply@Timeout`,
    /// default 300.
    pub request_to_reply_timeout_ms: u32,
    /// Idle time after which TesterPresent is sent. `TesterPresent@Delay`, default 3000.
    pub tester_present_ms: u32,
    /// Gap after a failed exchange. `OnError@Delay`, default 300.
    pub on_error_ms: u32,
    /// `KW2@Timeout`, default 300. The 5-baud read window is this plus 100 ms.
    pub kw2_timeout_ms: u32,
}

impl Default for KTiming {
    fn default() -> Self {
        KTiming {
            baud: consts::DEFAULT_BAUD,
            init: InitKind::Iso8FiveBaud,
            kw2_delay_ms: 25,
            first_request_ms: 20,
            inter_byte_ms: 0,
            reply_to_request_ms: 5,
            request_to_reply_timeout_ms: 300,
            tester_present_ms: 3000,
            on_error_ms: 300,
            kw2_timeout_ms: 300,
        }
    }
}

impl KTiming {
    /// Read window for 0x55 KW1 KW2 and for [echo, ~address]: KW2Timeout + 100 ms.
    pub fn kw2_read_window_ms(&self) -> u32 {
        self.kw2_timeout_ms
            .saturating_add(consts::KW2_READ_MARGIN_MS)
    }
}

/// How DDT2000 would open this ECU; decides the 5-baud address byte, the inter-byte gap and
/// whether a 5-baud ECU is opened with a fast init instead. See docs/protocol.md "Init variants".
///
/// NOT VERIFIED on the car which one the ECUs need; test order SingleEcu, MultiEcu, FastInit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitVariant {
    /// One ECU per session (DDT2000's normal single-ECU use): DDT2000 clears Multipoint during
    /// the init (RE: KWP2000Diag4 DiagProtocols single-ECU connect), so the address goes out
    /// plain (Sirius 0x7A, BMT 0x26), the XML inter-byte gap (0) and XML timings apply.
    #[default]
    SingleEcu,
    /// Several ECUs connected together: `KWP@Multipoint` from the XML takes effect, address
    /// ^ 0x80 (0xFA, 0xA6), inter-byte 7 ms and DDT2000's fixed Multipoint timings
    /// (RE: KWP2000Diag4 KWP2000Ecu Multipoint init). Same as `SingleEcu` when the XML has no
    /// Multipoint.
    MultiEcu,
    /// Fallback for a KWP 5-baud ECU: open it with a KWP fast init (x65 addressing lists
    /// FastInit MonoPoint for 0x7A). Not applicable to ISO8.
    FastInit,
}

/// Everything the init and session machines need to talk to one ECU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkConfig {
    /// ECU diagnostic address (`Function@Address`), e.g. 0x7A injection, 0x26 BMT.
    pub ecu_address: u8,
    /// Tester address in KWP headers, default 0xF1.
    pub tool_address: u8,
    /// Timing as declared by the definition (monopoint values). Use
    /// [`LinkConfig::effective_timing`] for what is actually used.
    pub timing: KTiming,
    /// Default [`InitVariant::SingleEcu`].
    pub variant: InitVariant,
    /// Key bytes declared by the ECU definition, compared after init and reported.
    pub expected_keywords: Option<(u8, u8)>,
    /// Fail the init when the received key bytes differ from `expected_keywords`.
    /// Default false: DDT2000 does not check them (RE: IOTBD32V3), and the CLI uses them to pick
    /// a definition (Safir_1 0x1C 0x86 vs Safir_2 0xC2 0x85).
    pub reject_keyword_mismatch: bool,
    /// Idle time before an init, see [`consts::IDLE_BEFORE_INIT_MS`].
    pub idle_before_init_ms: u32,
    /// Read timeout after a 0x78 reply, see [`consts::RESPONSE_PENDING_TIMEOUT_MS`].
    pub response_pending_timeout_ms: u32,
    /// Bound on consecutive 0x78 replies, see [`consts::MAX_RESPONSE_PENDING`].
    pub max_response_pending: u8,
    /// Read window for `55 KW1 KW2` and `[echo, ~address]` in a 5-baud init. `None` (default):
    /// DDT2000 behaviour, KW2Timeout + 100 ms ([`KTiming::kw2_read_window_ms`]). Set it to
    /// widen the window on the car if an ECU's W1 is near the ISO 9141 maximum of 300 ms.
    /// NOT VERIFIED on the car whether any ECU needs it.
    pub kw_window_ms: Option<u32>,
    /// When TesterPresent is sent; default [`TesterPresentMode::Idle`] (DDT2000's wire-level
    /// behaviour). [`TesterPresentMode::DDT2000_PERIODIC`] is the screen player option.
    pub tester_present_mode: crate::retry::TesterPresentMode,
}

impl LinkConfig {
    /// Config with defaults for the given address and declared timing.
    pub fn new(ecu_address: u8, timing: KTiming) -> Self {
        LinkConfig {
            ecu_address,
            tool_address: consts::TOOL_ADDRESS,
            timing,
            variant: InitVariant::SingleEcu,
            expected_keywords: None,
            reject_keyword_mismatch: false,
            idle_before_init_ms: consts::IDLE_BEFORE_INIT_MS,
            response_pending_timeout_ms: consts::RESPONSE_PENDING_TIMEOUT_MS,
            max_response_pending: consts::MAX_RESPONSE_PENDING,
            kw_window_ms: None,
            tester_present_mode: crate::retry::TesterPresentMode::Idle,
        }
    }

    /// The init that will run: the declared kind with `multipoint` only under
    /// [`InitVariant::MultiEcu`], and KWP 5-baud replaced by fast init under
    /// [`InitVariant::FastInit`].
    pub fn init_kind(&self) -> InitKind {
        let multi = self.variant == InitVariant::MultiEcu;
        match (self.variant, self.timing.init) {
            (_, InitKind::Iso8FiveBaud) => InitKind::Iso8FiveBaud,
            (InitVariant::FastInit, InitKind::KwpFiveBaud { .. } | InitKind::KwpFast { .. }) => {
                InitKind::KwpFast { multipoint: false }
            }
            (_, InitKind::KwpFiveBaud { multipoint }) => InitKind::KwpFiveBaud {
                multipoint: multipoint && multi,
            },
            (_, InitKind::KwpFast { multipoint }) => InitKind::KwpFast {
                multipoint: multipoint && multi,
            },
        }
    }

    /// Timing actually used: `timing` with [`LinkConfig::init_kind`], and with Multipoint in
    /// effect DDT2000's fixed values (RE: KWP2000Diag4 KWP2000Ecu Multipoint init): 10400 baud,
    /// inter-byte 7, KW2 delay 25, FirstRequest 20, TesterPresent 3000, KW2Timeout 300,
    /// RequestToReply 300. ReplyToRequest and OnError stay as declared.
    pub fn effective_timing(&self) -> KTiming {
        let mut t = self.timing;
        t.init = self.init_kind();
        if t.init.multipoint() {
            t.baud = consts::DEFAULT_BAUD;
            t.inter_byte_ms = consts::MULTIPOINT_INTER_BYTE_MS;
            t.kw2_delay_ms = 25;
            t.first_request_ms = 20;
            t.tester_present_ms = 3000;
            t.kw2_timeout_ms = 300;
            t.request_to_reply_timeout_ms = 300;
        }
        t
    }

    /// The byte bit-banged during a 5-baud init: 8 raw bits, LSB first, no parity bit
    /// (RE: IOTBD32V3). `address ^ 0x80` with Multipoint in effect, else the address.
    pub fn five_baud_address_byte(&self) -> u8 {
        if self.init_kind().multipoint() {
            self.ecu_address ^ consts::MULTIPOINT_ADDRESS_XOR
        } else {
            self.ecu_address
        }
    }
}
