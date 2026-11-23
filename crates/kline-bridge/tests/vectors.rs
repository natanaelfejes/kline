//! The test vectors of docs/bridge-protocol.md: encoding gives exactly these bytes and they
//! decode back. The CRCs were cross-checked with Python's `binascii.crc_hqx(data, 0xFFFF)`
//! (CRC-16/CCITT-FALSE), an implementation independent of this crate's.

use kline_bridge::gatt::*;
use kline_bridge::profile::{EcuId, Profile};

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn cmd(v: &str, c: Command) {
    assert_eq!(c.encode().unwrap(), hex(v), "{c:?}");
    assert_eq!(Command::decode(&hex(v)), Ok(c));
}

#[test]
fn command_vectors() {
    cmd(
        "01 03 06 01 00 00 00 01 01 B1 FC",
        Command {
            counter: 1,
            op: Op::StartLive(Profile::Drive),
        },
    );
    cmd(
        "01 03 08 02 00 00 00 03 7A 21 A9 D1 8E",
        Command {
            counter: 2,
            op: Op::Request {
                ecu: EcuId::Sirius,
                payload: vec![0x21, 0xA9],
            },
        },
    );
    cmd(
        "01 03 09 03 00 00 00 04 7A 14 00 00 47 CB",
        Command {
            counter: 3,
            op: Op::Prepare {
                ecu: EcuId::Sirius,
                payload: vec![0x14, 0x00, 0x00],
            },
        },
    );
    cmd(
        "01 03 0F 04 00 00 00 05 07 00 00 00 41 43 44 45 46 47 93 EE",
        Command {
            counter: 4,
            op: Op::Confirm {
                challenge: 7,
                code: "ACDEFG".into(),
            },
        },
    );
    cmd(
        "01 03 05 05 00 00 00 06 FF AE",
        Command {
            counter: 5,
            op: Op::Lock,
        },
    );
}

#[test]
fn notification_vectors() {
    let r = Response {
        counter: 2,
        result: ResultCode::Ok,
        data: vec![0x61, 0xA9, 0x04, 0x35],
    };
    assert_eq!(
        r.encode().unwrap(),
        hex("01 04 09 02 00 00 00 00 61 A9 04 35 FB 93")
    );
    assert_eq!(Response::decode(&r.encode().unwrap()), Ok(r));

    let mut data = 7u32.to_le_bytes().to_vec();
    data.push(1);
    data.extend_from_slice(b"ACDEFG");
    let r = Response {
        counter: 3,
        result: ResultCode::Challenge,
        data,
    };
    assert_eq!(
        r.encode().unwrap(),
        hex("01 04 10 03 00 00 00 10 07 00 00 00 01 41 43 44 45 46 47 6A D0")
    );

    let l = LiveSample {
        seq: 1,
        t_ms: 12345,
        rpm: 1077,
        speed_kmh: 0,
        inj_us: 7412,
        fuel_mlh: 1400,
        coolant_c: 19,
        iat_c: 15,
        map_mbar: 406,
        batt_cv: 1433,
        flags: 0x07,
    };
    assert_eq!(
        l.encode(),
        hex("01 01 14 01 00 39 30 00 00 35 04 00 F4 1C 78 05 13 0F 96 01 99 05 07 9A EA")
    );
    assert_eq!(LiveSample::decode(&l.encode()), Ok(l));

    let s = Status {
        fw_major: 0,
        fw_minor: 1,
        state: BridgeState::Session,
        ecu: 0x7A,
        profile: Profile::Drive,
        batt_cv: 1410,
        lock: LockState::Unknown,
        flags: 0x04,
        last_error: 0,
        uptime_s: 60,
    };
    assert_eq!(
        s.encode(),
        hex("01 02 0E 00 01 02 7A 01 82 05 00 04 00 3C 00 00 00 3B A7")
    );
    assert_eq!(Status::decode(&s.encode()), Ok(s));

    let raw = RawFrame {
        t_ms: 2500,
        dir: RawDir::Tx,
        ecu: 0x7A,
        bytes: vec![0x82, 0x7A, 0xF1, 0x21, 0xA9, 0xB7],
    };
    assert_eq!(
        raw.encode().unwrap(),
        hex("01 05 0C C4 09 00 00 00 7A 82 7A F1 21 A9 B7 4C 33")
    );
    assert_eq!(RawFrame::decode(&raw.encode().unwrap()), Ok(raw));
}

#[test]
fn malformed_vectors() {
    // Bad CRC, wrong version, length mismatch, unknown opcode, extra byte.
    assert_eq!(
        Command::decode(&hex("01 03 06 01 00 00 00 01 01 B1 FD")),
        Err(CodecError::BadCrc)
    );
    assert_eq!(
        decode_frame(&hex("02 03 06 01 00 00 00 01 01 B1 FC")),
        Err(CodecError::BadVersion(2))
    );
    assert_eq!(
        decode_frame(&hex("01 03 07 01 00 00 00 01 01 B1 FC")),
        Err(CodecError::LengthMismatch)
    );
    let f = encode_frame(Channel::Command, &[1, 0, 0, 0, 0x7F]).unwrap();
    assert_eq!(Command::decode(&f), Err(CodecError::BadOpcode(0x7F)));
    let f = encode_frame(Channel::Command, &[1, 0, 0, 0, 0x06, 0x00]).unwrap();
    assert_eq!(Command::decode(&f), Err(CodecError::BadArgs));
    // A request for an ECU other than 0x7A / 0x26 / 0x01 / 0x2C does not decode.
    let f = encode_frame(Channel::Command, &[1, 0, 0, 0, 0x03, 0x10, 0x21, 0x80]).unwrap();
    assert_eq!(Command::decode(&f), Err(CodecError::BadArgs));
}

#[test]
fn long_data_is_refused_not_truncated() {
    let r = Response {
        counter: 1,
        result: ResultCode::Ok,
        data: vec![0; MAX_RESPONSE_DATA + 1],
    };
    assert_eq!(r.encode(), Err(CodecError::BodyTooLong));
    let r = Response {
        data: vec![0; MAX_RESPONSE_DATA],
        ..r
    };
    assert_eq!(
        Response::decode(&r.encode().unwrap()).unwrap().data.len(),
        MAX_RESPONSE_DATA
    );
    let raw = RawFrame {
        t_ms: 0,
        dir: RawDir::Rx,
        ecu: 0x7A,
        bytes: vec![0; MAX_RAW_BYTES + 1],
    };
    assert_eq!(raw.encode(), Err(CodecError::BodyTooLong));
}
