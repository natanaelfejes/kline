//! Several ECUs on one simulated K line, for the `kline-sim` runner (docs/sim.md "Running it").
//!
//! The bus decodes a wake-up once; [`MultiEcu`] hands a 5-baud address byte to the ECU at that
//! address (plain or `^ 0x80`) and from then on gives the tester's bytes only to that ECU,
//! until the next wake-up. A fast-init pulse goes to every present ECU (a KWP header names its
//! target, and the ECUs ignore frames for others). An ECU can be switched off and on while the
//! simulator runs ([`Presence`]): absent, it sees no wake-up and no byte, and whatever it would
//! send is dropped (its timers still run, so a session it had times out as usual).
//!
//! Every wake-up, request, reply, presence change, ECU write log entry, violation and ECU note
//! can go to a transcript ([`Transcript`]), one line each, written as it happens.
//!
//! Simplifications (NOT modelled): ECUs never transmit at the same time (collisions), an ECU
//! that is not addressed does not overhear the other ECUs' frames, and a wake-up for one ECU
//! does not end another ECU's session (it times out after P3max like on the car).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::bus::{Out, Responder, Wake};
use crate::ecu::MULTIPOINT_XOR;
use crate::frame::{Assembler, Feed, WireFormat};
use crate::journal::Journal;

/// Shared on/off switch of one ECU on the line.
#[derive(Debug, Clone)]
pub struct Presence(Arc<AtomicBool>);

impl Presence {
    pub fn new(on: bool) -> Presence {
        Presence(Arc::new(AtomicBool::new(on)))
    }

    pub fn is_on(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::SeqCst)
    }
}

/// Where the transcript lines go (a file, or a buffer in tests). Cheap to clone.
#[derive(Clone)]
pub struct Transcript(Arc<Mutex<Box<dyn Write + Send>>>);

impl Transcript {
    pub fn new(w: impl Write + Send + 'static) -> Transcript {
        Transcript(Arc::new(Mutex::new(Box::new(w))))
    }

    /// One line `t=<ms> ms <text>`, flushed at once so a killed simulator keeps it.
    pub fn line(&self, t_ms: u64, text: &str) {
        let mut w = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "t={t_ms} ms {text}");
        let _ = w.flush();
    }
}

/// One ECU on the line.
pub struct Slot {
    pub address: u8,
    /// Short name for the transcript and the control (`bmt`, `replay`, a file stem).
    pub name: String,
    pub responder: Box<dyn Responder>,
    /// Its wire format, to decode the frames in the transcript.
    pub format: WireFormat,
    /// Its journal (write log, violations, notes), forwarded to the transcript.
    pub journal: Journal,
    pub presence: Presence,
    /// Wakes on a fast-init pulse (a replay does not: it expects its own recorded wake-up).
    pub fast_init: bool,
    seen: (usize, usize, usize),
}

impl Slot {
    pub fn new(
        address: u8,
        name: impl Into<String>,
        responder: impl Responder + 'static,
        format: WireFormat,
        journal: Journal,
    ) -> Slot {
        Slot {
            address,
            name: name.into(),
            responder: Box::new(responder),
            format,
            journal,
            presence: Presence::new(true),
            fast_init: true,
            seen: (0, 0, 0),
        }
    }

    fn label(&self) -> String {
        format!("{:02X} {}", self.address, self.name)
    }
}

/// The ECUs of one K line, as one [`Responder`] for the bus.
pub struct MultiEcu {
    slots: Vec<Slot>,
    /// Slots the tester's bytes go to (those the last wake-up reached).
    active: Vec<usize>,
    transcript: Option<Transcript>,
    asm: Assembler,
    /// Presence as last written to the transcript.
    shown_presence: Vec<bool>,
}

impl MultiEcu {
    pub fn new(slots: Vec<Slot>, transcript: Option<Transcript>) -> MultiEcu {
        let shown_presence = slots.iter().map(|s| s.presence.is_on()).collect();
        MultiEcu {
            slots,
            active: Vec::new(),
            transcript,
            asm: Assembler::default(),
            shown_presence,
        }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    fn log(&self, t: u64, text: &str) {
        if let Some(tr) = &self.transcript {
            tr.line(t, text);
        }
    }

    /// Presence changes and new journal entries, to the transcript.
    fn sync(&mut self, t: u64) {
        if self.transcript.is_none() {
            return;
        }
        for i in 0..self.slots.len() {
            let on = self.slots[i].presence.is_on();
            if on != self.shown_presence[i] {
                self.shown_presence[i] = on;
                let l = self.slots[i].label();
                self.log(
                    t,
                    &format!("ECU {l} {}", if on { "back on the line" } else { "gone" }),
                );
            }
            let s = &self.slots[i];
            let (w, v, n) = (
                s.journal.writes(),
                s.journal.violations(),
                s.journal.notes(),
            );
            let label = s.label();
            let (sw, sv, sn) = s.seen;
            for r in w.iter().skip(sw) {
                self.log(r.t_ms, &format!("ECU {label} WRITE LOG {r}"));
            }
            for x in v.iter().skip(sv) {
                self.log(
                    x.t_ms,
                    &format!("ECU {label} violation {:?}: {}", x.kind, x.detail),
                );
            }
            for (nt, text) in n.iter().skip(sn) {
                // One per byte an asleep ECU did not take: noise in a transcript.
                if text.starts_with("asleep, byte") {
                    continue;
                }
                self.log(*nt, &format!("ECU {label} note: {text}"));
            }
            self.slots[i].seen = (w.len(), v.len(), n.len());
        }
    }

    /// Log and pass on what slot `i` sent.
    fn emit(&mut self, i: usize, o: Out, out: &mut Out) {
        if o.is_empty() {
            return;
        }
        let s = &self.slots[i];
        if !s.presence.is_on() {
            let t = o.bytes()[0].0;
            self.log(
                t,
                &format!("ECU {} is off the line, reply dropped", s.label()),
            );
            return;
        }
        if self.transcript.is_some() {
            let t = o.bytes()[0].0;
            let hex = hex(o.bytes().iter().map(|&(_, b)| b));
            self.log(t, &format!("RX {} -> {hex}", s.label()));
        }
        out.append(o);
    }

    fn present(&self, i: usize) -> bool {
        self.slots[i].presence.is_on()
    }
}

fn hex(b: impl IntoIterator<Item = u8>) -> String {
    b.into_iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Responder for MultiEcu {
    fn on_wake(&mut self, wake: Wake, out: &mut Out) {
        let t = match wake {
            Wake::FiveBaud { t_end, .. } => t_end,
            Wake::Fast { t_high, .. } => t_high,
        };
        self.sync(t);
        self.asm.clear();
        self.active.clear();
        match wake {
            Wake::FiveBaud { byte, .. } => {
                let hit: Vec<usize> = (0..self.slots.len())
                    .filter(|&i| {
                        let a = self.slots[i].address;
                        byte == a || byte == a ^ MULTIPOINT_XOR
                    })
                    .collect();
                let mut woke = Vec::new();
                for &i in &hit {
                    if self.present(i) {
                        woke.push(self.slots[i].label());
                        self.active.push(i);
                    }
                }
                self.log(
                    t,
                    &format!(
                        "WAKE 5-baud {byte:02X}: {}",
                        if woke.is_empty() {
                            if hit.is_empty() {
                                "no ECU at this address".to_string()
                            } else {
                                "the ECU at this address is off the line".to_string()
                            }
                        } else {
                            woke.join(", ")
                        }
                    ),
                );
            }
            Wake::Fast { .. } => {
                self.active = (0..self.slots.len())
                    .filter(|&i| self.slots[i].fast_init && self.present(i))
                    .collect();
                self.log(t, "WAKE fast init");
            }
        }
        for k in 0..self.active.len() {
            let i = self.active[k];
            let mut o = Out::default();
            self.slots[i].responder.on_wake(wake, &mut o);
            self.emit(i, o, out);
        }
        self.sync(t);
    }

    fn on_byte(&mut self, t_ms: u64, byte: u8, out: &mut Out) {
        self.sync(t_ms);
        self.active.retain(|&i| self.slots[i].presence.is_on());
        if self.transcript.is_some() {
            if let Some(&i) = self.active.first() {
                let fmt = self.slots[i].format;
                match self.asm.push(byte, fmt) {
                    Feed::More => {}
                    Feed::Frame(p) => {
                        let to = p.target.map(|x| format!("{x:02X}")).unwrap_or_default();
                        let to = if to.is_empty() {
                            self.slots[i].label()
                        } else {
                            to
                        };
                        self.log(t_ms, &format!("TX {to} <- {}", hex(p.payload)));
                    }
                    Feed::Bad(e) => self.log(
                        t_ms,
                        &format!("TX ..{byte:02X} (init byte or not a request frame: {e})"),
                    ),
                }
            } else {
                self.log(t_ms, &format!("TX {byte:02X} (no ECU listening)"));
            }
        }
        for k in 0..self.active.len() {
            let i = self.active[k];
            let mut o = Out::default();
            self.slots[i].responder.on_byte(t_ms, byte, &mut o);
            self.emit(i, o, out);
        }
        self.sync(t_ms);
    }

    fn next_timer(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter_map(|s| s.responder.next_timer())
            .min()
    }

    fn on_timer(&mut self, t_ms: u64, out: &mut Out) {
        for i in 0..self.slots.len() {
            if self.slots[i]
                .responder
                .next_timer()
                .is_some_and(|nt| nt <= t_ms)
            {
                let mut o = Out::default();
                self.slots[i].responder.on_timer(t_ms, &mut o);
                self.emit(i, o, out);
            }
        }
        self.sync(t_ms);
    }
}
