//! BLE command fuzzing: whatever any client writes, nothing but Tier 0 reads (and the
//! session's own protocol frames) reaches the K line, and the simulated ECUs log no write.
//! Codes are never known to the fuzzer, so a Tier 1 request can never be confirmed here.

mod common;

use common::*;
use kline_bridge::gate::Role;
use kline_bridge::gatt::{self, Command, Op};
use kline_bridge::profile::{EcuId, Profile};
use proptest::prelude::*;

fn ecu() -> impl Strategy<Value = EcuId> {
    prop_oneof![
        Just(EcuId::Sirius),
        Just(EcuId::Bmt),
        Just(EcuId::Abs),
        Just(EcuId::Airbag)
    ]
}

/// Payloads biased toward the interesting services (writes, routines, immobilizer, protocol
/// frames), plus anything.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    let service = prop_oneof![
        Just(0x21u8),
        Just(0x14),
        Just(0x30),
        Just(0x31),
        Just(0x32),
        Just(0x3B),
        Just(0x2E),
        Just(0x27),
        Just(0x3D),
        Just(0x34),
        Just(0x10),
        Just(0x3E),
        Just(0x81),
        Just(0x82),
        Just(0x41),
        Just(0x71),
        Just(0x45),
        Just(0x58),
        any::<u8>(),
    ];
    (service, proptest::collection::vec(any::<u8>(), 0..6)).prop_map(|(s, mut rest)| {
        rest.insert(0, s);
        rest
    })
}

/// Documented Tier 1 requests, so PREPARE often makes a real challenge.
fn tier1() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Prepare {
            ecu: EcuId::Sirius,
            payload: vec![0x14, 0x00, 0x00]
        }),
        Just(Op::Prepare {
            ecu: EcuId::Bmt,
            payload: vec![0x31, 0xA0, 0x00, 0x01]
        }),
        Just(Op::Prepare {
            ecu: EcuId::Sirius,
            payload: vec![0x30, 0x94, 0x00, 0x01]
        }),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        tier1(),
        (ecu(), payload()).prop_map(|(ecu, payload)| Op::Request { ecu, payload }),
        (ecu(), payload()).prop_map(|(ecu, payload)| Op::Prepare { ecu, payload }),
        (
            ecu(),
            proptest::collection::vec(any::<u8>(), 1..12),
            payload()
        )
            .prop_map(|(ecu, identification, payload)| Op::PrepareWrite {
                ecu,
                identification,
                payload
            }),
        (any::<u32>(), "[A-Z0-9]{6,10}")
            .prop_map(|(challenge, code)| Op::Confirm { challenge, code }),
        (1u32..4).prop_map(|c| Op::Confirm {
            challenge: c,
            code: "ACDEFG".into()
        }),
        prop_oneof![Just(Profile::Drive), Just(Profile::Parked)].prop_map(Op::StartLive),
        Just(Op::StopLive),
        Just(Op::Lock),
        Just(Op::Unlock),
        Just(Op::YieldKline),
        Just(Op::ResumeKline),
        Just(Op::LwmArm),
        Just(Op::LwmDisarm),
        Just(Op::LwmAck),
        any::<bool>().prop_map(Op::RawStream),
        Just(Op::GetStatus),
        (0u8..=kline_bridge::gatt::MAX_EXTRA_EVERY).prop_map(Op::LiveExtras),
    ]
}

#[derive(Debug, Clone)]
enum Input {
    Cmd {
        client: u8,
        op: Op,
        counter_skew: i8,
    },
    Garbage {
        client: u8,
        bytes: Vec<u8>,
    },
    Battery(u16),
    Wait(u16),
}

fn input() -> impl Strategy<Value = Input> {
    let client = prop_oneof![Just(OWNER), Just(AGENT), Just(STRANGER)];
    prop_oneof![
        6 => (client.clone(), op(), -2i8..3).prop_map(|(client, op, counter_skew)| Input::Cmd {
            client,
            op,
            counter_skew
        }),
        1 => (client, proptest::collection::vec(any::<u8>(), 0..40))
            .prop_map(|(client, bytes)| Input::Garbage { client, bytes }),
        1 => prop_oneof![Just(11_700u16), Just(11_950), Just(12_600), Just(14_100)]
            .prop_map(Input::Battery),
        3 => (0u16..6_000).prop_map(Input::Wait),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn ble_commands_never_put_a_non_read_on_the_wire(inputs in proptest::collection::vec(input(), 1..40)) {
        let mut rig = Rig::full_car();
        rig.connect(OWNER, Role::Owner);
        rig.connect(AGENT, Role::Agent);
        rig.battery(12_600);
        let mut counters = std::collections::BTreeMap::<u8, i64>::new();
        for i in inputs {
            match i {
                Input::Cmd { client, op, counter_skew } => {
                    let c = counters.entry(client).or_insert(0);
                    *c = (*c + i64::from(counter_skew)).max(0);
                    let Ok(frame) = (Command { counter: *c as u32, op }).encode() else { continue };
                    rig.send_raw(client, &frame);
                }
                Input::Garbage { client, bytes } => rig.send_raw(client, &bytes),
                Input::Battery(mv) => rig.battery(mv),
                Input::Wait(ms) => rig.run_for(u64::from(ms)),
            }
            rig.run_for(50);
        }
        rig.run_for(10_000);
        rig.assert_wire_tier0_except(&[]);
        prop_assert!(rig.car.sirius_journal.writes().is_empty(), "{:?}", rig.car.sirius_journal.writes());
        prop_assert!(rig.car.bmt_journal.writes().is_empty(), "{:?}", rig.car.bmt_journal.writes());
        for j in &rig.car.other_journals {
            prop_assert!(j.writes().is_empty(), "{:?}", j.writes());
        }
    }

    #[test]
    fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = gatt::decode_frame(&bytes);
        let _ = Command::decode(&bytes);
        let _ = gatt::Response::decode(&bytes);
        let _ = gatt::Status::decode(&bytes);
        let _ = gatt::LiveSample::decode(&bytes);
        let _ = gatt::RawFrame::decode(&bytes);
        let _ = kline_bridge::live::sirius_aa(&bytes);
        let _ = kline_bridge::live::sirius_a9(&bytes);
        let _ = kline_bridge::live::bmt(&bytes);
    }

    #[test]
    fn command_round_trip(counter in any::<u32>(), op in op()) {
        let cmd = Command { counter, op };
        if let Ok(f) = cmd.encode() {
            prop_assert!(f.len() <= gatt::MAX_FRAME);
            prop_assert_eq!(Command::decode(&f), Ok(cmd));
        }
    }

    #[test]
    fn any_single_bit_flip_is_rejected(counter in any::<u32>(), op in op(), bit in 0usize..1000) {
        let Ok(mut f) = (Command { counter, op }).encode() else { return Ok(()) };
        let bit = bit % (f.len() * 8);
        f[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(Command::decode(&f).is_err());
    }
}

#[cfg(feature = "approvals")]
proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// A live challenge answered with any code but the right one (by the owner or the agent)
    /// sends nothing.
    #[test]
    fn wrong_codes_never_approve(
        prep in tier1(),
        tries in proptest::collection::vec((prop_oneof![Just(OWNER), Just(AGENT)], "[A-Z0-9]{1,12}"), 1..4),
    ) {
        use kline_bridge::gatt::ResultCode;
        let mut rig = Rig::default_car();
        rig.connect(OWNER, Role::Owner);
        rig.connect(AGENT, Role::Agent);
        rig.battery(12_600);
        let r = rig.call(OWNER, prep, 15_000);
        prop_assert_eq!(r.result, ResultCode::Challenge);
        let id = u32::from_le_bytes(r.data[..4].try_into().unwrap());
        let code = String::from_utf8(r.data[5..].to_vec()).unwrap();
        for (client, guess) in tries {
            if guess == code {
                continue;
            }
            rig.call(client, Op::Confirm { challenge: id, code: guess }, 10_000);
        }
        rig.run_for(5_000);
        rig.assert_wire_tier0_except(&[]);
        prop_assert!(rig.car.sirius_journal.writes().is_empty());
        prop_assert!(rig.car.bmt_journal.writes().is_empty());
    }
}
