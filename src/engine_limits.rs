//! Per-engine overheat threshold (`m_oid`).
//!
//! The vanilla test is one instruction pair in `Engine_ApplyThrustForce` (`0x140028360`):
//!
//! ```text
//! 140028614  MOVSS XMM0, dword ptr [RSI + 0x2ec]     ; the part's temperature, RSI = part
//! 14002861c  COMISS XMM0, dword ptr [0x140352838]    ; >= 500?
//! 140028623  JC     0x14028634                       ; yes -> +0x810 = 3.0 (lockout), return
//! ```
//!
//! The hook replaces the load and hands the vanilla compare a **shifted** temperature instead of
//! patching the compare's constant:
//!
//! ```text
//! returned = T + (global_threshold - engine_threshold(oid))
//! ```
//!
//! so `returned >= global_threshold` is exactly `T >= engine_threshold`, for any engine `m_oid` in
//! the config. Engines that are not listed get the raw temperature back, i.e. the global knob
//! (`engine_overheat_threshold`, patched at `0x14002861c`) applies to them. Both knobs therefore
//! compose, and neither touches the other's bytes.
//!
//! The callback runs once per engine per frame (60 Hz, a handful of engines), so it keeps no state:
//! a config lookup on the `m_oid` string is cheap enough.

use patchy::{ReturnType, Trampoline};

/// `MOVSS XMM0, dword ptr [RSI + 0x2ec]` — the part temperature load, RSI = the engine part.
pub(crate) const SITE: usize = 0x1_4002_8614;
pub(crate) const SITE_BYTES: [u8; 8] = [0xf3, 0x0f, 0x10, 0x86, 0xec, 0x02, 0x00, 0x00];
/// `COMISS XMM0, dword ptr [0x140352838]` — left untouched, the shifted temperature is compared here.
pub(crate) const RESUME: usize = 0x1_4002_861c;

/// Part temperature (the overheat accumulator).
const TEMPERATURE: usize = 0x2ec;

/// `mov rcx, rsi` — the callback takes the part in RCX.
const MOV_RCX_RSI: [u8; 3] = [0x48, 0x89, 0xf1];

pub(crate) fn trampoline() -> Trampoline {
    let mut trampoline = Trampoline::new();
    // `ReturnType::Xmm0` keeps the callback's `f32` return in XMM0 and restores everything else.
    trampoline.preserved_call(
        on_temperature_read as *const (),
        &MOV_RCX_RSI,
        &[],
        ReturnType::Xmm0,
    );
    trampoline.relative_jump(RESUME);
    trampoline
}

/// The temperature the vanilla compare sees: shifted so that its `>= global` test becomes
/// `>= this engine's threshold`.
unsafe extern "C" fn on_temperature_read(part: *mut u8) -> f32 {
    let part = part as usize;
    // SAFETY: the game passes the live engine part it is about to compare.
    let temperature = unsafe { crate::read::<f32>(part + TEMPERATURE) };
    let settings = crate::config();
    if settings.engine_overheat_by_oid.is_empty() {
        return temperature;
    }
    // SAFETY: same part, read only for its `m_oid`.
    let Some(oid) = (unsafe { crate::oid_of(part) }) else {
        return temperature;
    };
    let Some(limit) = crate::config::lookup(&settings.engine_overheat_by_oid, oid) else {
        crate::note_unmatched(
            &crate::UNMATCHED_ENGINES,
            "engine",
            oid,
            settings.engine_overheat_threshold,
        );
        return temperature;
    };
    shifted_temperature(temperature, settings.engine_overheat_threshold, limit)
}

/// The temperature the vanilla compare must see for `limit` to be the real trip point.
///
/// The game tests `shifted >= scalar`; returning `T + (scalar - limit)` makes that `T >= limit`.
fn shifted_temperature(temperature: f32, scalar: f32, limit: f32) -> f32 {
    temperature + (scalar - limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zeroed Body-shaped block with a temperature and an inline `m_oid`.
    fn fake_engine(temperature: f32, oid: &str) -> Box<[u8; 0x400]> {
        let mut part = Box::new([0u8; 0x400]);
        part[0x2ec..0x2f0].copy_from_slice(&temperature.to_le_bytes());
        let name = oid.as_bytes();
        assert!(name.len() < 16, "keep the test to the inline EscadraString");
        part[0x190..0x190 + name.len()].copy_from_slice(name);
        let length = (name.len() as u64).to_le_bytes();
        part[0x1a0..0x1a8].copy_from_slice(&length);
        part[0x1a8..0x1b0].copy_from_slice(&length);
        part
    }

    #[test]
    fn the_shift_makes_the_scalar_compare_test_the_engine_limit() {
        // scalar 500, one engine limited to 400 -> shift +100
        assert_eq!(shifted_temperature(399.0, 500.0, 400.0), 499.0);
        assert_eq!(shifted_temperature(400.0, 500.0, 400.0), 500.0);
        // scalar 350 (patched), engine 400 -> shift -50
        assert_eq!(shifted_temperature(400.0, 350.0, 400.0), 350.0);
        // equal values change nothing
        assert_eq!(shifted_temperature(250.0, 500.0, 500.0), 250.0);
    }

    #[test]
    fn the_callback_reads_the_oid_and_shifts_with_the_configured_limit() {
        // One shared test config (scalar 500, MDL_ENGINE_01 limited to 400); see `lib.rs`.
        crate::install_test_config();

        let limited = fake_engine(399.0, "MDL_ENGINE_01");
        assert_eq!(
            unsafe { on_temperature_read(limited.as_ptr() as *mut u8) },
            499.0
        );
        let at_limit = fake_engine(400.0, "MDL_ENGINE_01");
        assert_eq!(
            unsafe { on_temperature_read(at_limit.as_ptr() as *mut u8) },
            500.0
        );
        // An engine that is not listed keeps the scalar behaviour.
        let other = fake_engine(400.0, "MDL_ENGINE_05");
        assert_eq!(
            unsafe { on_temperature_read(other.as_ptr() as *mut u8) },
            400.0
        );
        // A body with no name is left alone as well.
        let nameless = fake_engine(123.0, "");
        assert_eq!(
            unsafe { on_temperature_read(nameless.as_ptr() as *mut u8) },
            123.0
        );
    }
}
