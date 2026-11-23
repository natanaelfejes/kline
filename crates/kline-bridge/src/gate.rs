//! Who may ask for what over BLE (MASTER-PLAN-v2 P2, P3, P6 and M12).
//!
//! The decision on bytes is kline-core's ([`kline_core::safety::tier_of_in`]); this module only
//! adds the BLE roles and the battery rule on top, and kline-core's `Session` checks every
//! request again before it is written. So the gate can only refuse more than kline-core, never
//! less: an agent-role request that the gate let through by mistake would still be refused by
//! `Session::request` unless it is Tier 0.

use kline_core::safety::{self, DefinitionGuard, EcuClass, Target, Tier};

use crate::gatt::ResultCode;
use crate::profile::EcuId;

/// The role of a bonded BLE peer, fixed at pairing (docs/bridge-protocol.md "Roles").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The owner's phone: Tier 0, and Tier 1 with a typed approval code; LOCK/UNLOCK.
    Owner,
    /// The Mac link (`kline --transport ble`, agents): Tier 0 only, always (MASTER-PLAN-v2 M12).
    Agent,
    /// Not bonded or not encrypted: nothing.
    Unbonded,
}

/// The policy target of `ecu` before a session: its protocol, address and the key bytes its
/// definition declares. Inside a session kline-core uses the init's real target.
pub fn target(ecu: EcuId) -> Target {
    Target {
        protocol: ecu.protocol(),
        ecu_address: ecu.address(),
        keywords: Some(ecu.keywords()),
    }
}

/// The definition guard the bridge applies without a definition: the class of the ECU (BMT
/// rules on the BMT, Sirius rules on the Sirius) and the car's name guard as compiled byte
/// prefixes ([`crate::guard_rules`]).
pub fn guard(ecu: EcuId) -> DefinitionGuard {
    let mut g = DefinitionGuard::new(match ecu {
        EcuId::Sirius => EcuClass::Sirius,
        EcuId::Bmt => EcuClass::Immobilizer,
        EcuId::Abs | EcuId::Airbag => EcuClass::Generic,
    });
    for (addr, prefix) in crate::guard_rules::NAME_GUARD_PREFIXES {
        if *addr == ecu.address() {
            g.flag(prefix.to_vec(), crate::guard_rules::COMPILED_NAME.into());
        }
    }
    g
}

/// A REQUEST (one-shot read): allowed only for Tier 0, for the owner and the agent.
pub fn check_request(role: Role, ecu: EcuId, payload: &[u8]) -> Result<(), ResultCode> {
    if role == Role::Unbonded {
        return Err(ResultCode::NotAllowedForRole);
    }
    match safety::tier_of_in(&guard(ecu), &target(ecu), payload) {
        Err(_) => Err(ResultCode::Forbidden),
        Ok(Tier::Read) => Ok(()),
        Ok(_) if role == Role::Agent => Err(ResultCode::NotAllowedForRole),
        Ok(_) => Err(ResultCode::ApprovalRequired),
    }
}

/// A PREPARE (approval challenge): owner only, any non-forbidden tier above Tier 0 (owner
/// decision 2026-11-23: BLE scope = everything the CLI can do), battery at or above the write
/// limit. `battery_ok` is false below 12.0 V or when the voltage is unknown.
pub fn check_prepare(
    role: Role,
    ecu: EcuId,
    payload: &[u8],
    battery_ok: bool,
) -> Result<Tier, ResultCode> {
    if role != Role::Owner {
        return Err(ResultCode::NotAllowedForRole);
    }
    match safety::tier_of_in(&guard(ecu), &target(ecu), payload) {
        Err(_) => Err(ResultCode::Forbidden),
        // A read needs no approval: use REQUEST.
        Ok(Tier::Read) => Err(ResultCode::Unsupported),
        Ok(_) if !battery_ok => Err(ResultCode::LowBattery),
        Ok(_) if !cfg!(feature = "approvals") => Err(ResultCode::Unsupported),
        Ok(t) => Ok(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_is_tier0_only() {
        assert_eq!(
            check_request(Role::Agent, EcuId::Sirius, &[0x21, 0xA9]),
            Ok(())
        );
        assert_eq!(
            check_request(Role::Agent, EcuId::Sirius, &[0x14, 0x00, 0x00]),
            Err(ResultCode::NotAllowedForRole)
        );
        assert_eq!(
            check_prepare(Role::Agent, EcuId::Bmt, &[0x31, 0xA0, 0x00, 0x01], true),
            Err(ResultCode::NotAllowedForRole)
        );
    }

    #[test]
    fn immobilizer_is_forbidden_for_everyone() {
        for role in [Role::Owner, Role::Agent] {
            assert_eq!(
                check_request(role, EcuId::Bmt, &[0x21, 0xA4, 0x00]),
                Err(ResultCode::Forbidden)
            );
            assert_eq!(
                check_request(role, EcuId::Bmt, &[0x3B, 0xD2, 0x02, 0x01]),
                Err(ResultCode::Forbidden)
            );
        }
        assert_eq!(
            check_prepare(Role::Owner, EcuId::Sirius, &[0x31, 0xA3, 0x00], true),
            Err(ResultCode::Forbidden)
        );
        assert_eq!(
            check_prepare(Role::Owner, EcuId::Bmt, &[0x31, 0xA6, 0x00, 0x01], true),
            Err(ResultCode::Forbidden)
        );
    }

    #[test]
    fn owner_tier1_needs_battery_and_prepare() {
        assert_eq!(
            check_request(Role::Owner, EcuId::Bmt, &[0x31, 0xA0, 0x00, 0x01]),
            Err(ResultCode::ApprovalRequired)
        );
        assert_eq!(
            check_prepare(Role::Owner, EcuId::Bmt, &[0x31, 0xA0, 0x00, 0x01], false),
            Err(ResultCode::LowBattery)
        );
        for (ecu, p, tier) in [
            (EcuId::Bmt, &[0x31, 0xA0, 0x00, 0x01][..], Tier::Service),
            (EcuId::Bmt, &[0x3B, 0xD2, 0x00, 0x01][..], Tier::ConfigWrite),
            (EcuId::Sirius, &[0x27, 0x01][..], Tier::Expert),
        ] {
            let ok = check_prepare(Role::Owner, ecu, p, true);
            if cfg!(feature = "approvals") {
                assert_eq!(ok, Ok(tier), "{p:02X?}");
            } else {
                assert_eq!(ok, Err(ResultCode::Unsupported));
            }
        }
        // The compiled name guard: a non-read variant of a flagged BMT read is refused.
        assert_eq!(
            check_prepare(Role::Owner, EcuId::Bmt, &[0x21, 0xA1, 0x01], true),
            Err(ResultCode::Forbidden)
        );
    }

    #[test]
    fn unbonded_gets_nothing() {
        assert_eq!(
            check_request(Role::Unbonded, EcuId::Sirius, &[0x21, 0xA9]),
            Err(ResultCode::NotAllowedForRole)
        );
    }
}
