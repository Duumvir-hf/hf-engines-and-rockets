//! Per-ammo rocket acceleration (`item_name` of the Ammo record).
//!
//! Rockets burn with a hardcoded thrust in `creatureLogic`: `mass * 500` along the body heading
//! while `+0x3d4 > 0` (1.5 s), for logic ids 130 (cluster rocket) and 132 (300 mm rocket):
//!
//! ```text
//! 1400348f2  MOVSS XMM7, dword ptr [0x140352838]     ; logic 130 burn thrust
//! 140034c0c  MOVSS XMM9, dword ptr [0x140352838]     ; logic 132 burn thrust
//! ```
//!
//! **A rocket body does not remember which ammo fired it.** `FireShell`'s rocket cases build the
//! body inline (no `Body_InitializeCannonShell`, no `+0x490` gun link, no `m_oid`); all they copy is
//! `logic`, `explosivePower`, `ttl` and mass, and several ammo items share logic 130 (NAR122, 220 mm,
//! NAR340), so nothing on the body identifies the item.
//!
//! The mod therefore captures the ammo where it *is* available — the four rocket cases each store
//! `body->logic = ammo->caliber` with the Ammo record in R15:
//!
//! ```text
//! 140030e90  MOV dword ptr [RBX + 0x3c4], EAX   (case 51, body in RBX)
//! 1400313c4  MOV dword ptr [RSI + 0x3c4], EAX   (case 52, body in RSI)
//! 1400319d0  MOV dword ptr [RBX + 0x3c4], EAX   (case 54, body in RBX)
//! 140031c46  MOV dword ptr [RSI + 0x3c4], EAX   (case 55, body in RSI)
//! ```
//!
//! — resolves `ammo->item_name` (Ammo `+0x8`) against the config and records `body -> accel` in the
//! mod's own table. The two burn sites then ask that table for the body's acceleration. Nothing is
//! written to the game: no body field is borrowed as storage, so no other mod can collide with the
//! carrier, and a recycled body address is re-recorded at its own launch (a fresh `FireShell` rocket
//! always passes one of the four sites again).
//!
//! Rockets that never pass those sites — the 300 mm rocket launched as a *ship part* (`+0x3d4` is set
//! in its own state-0 block) — have no entry and keep `rocket_acceleration`, the global knob.
//!
//! Entries are dropped as soon as the burn timer reaches zero, so the table only ever holds rockets
//! that are burning right now.

use std::{collections::HashMap, sync::Mutex};

use patchy::{ReturnType, Trampoline};

/// `MOV dword ptr [RBX + 0x3c4], EAX` — `body->logic = ammo->caliber`, Ammo record in R15.
pub(crate) const LAUNCH_RBX: usize = 0x1_4003_0e90;
pub(crate) const LAUNCH_RBX_BYTES: [u8; 6] = [0x89, 0x83, 0xc4, 0x03, 0x00, 0x00];
pub(crate) const LAUNCH_RBX_RESUME: usize = 0x1_4003_0e96;

pub(crate) const LAUNCH_RSI: usize = 0x1_4003_13c4;
pub(crate) const LAUNCH_RSI_BYTES: [u8; 6] = [0x89, 0x86, 0xc4, 0x03, 0x00, 0x00];
pub(crate) const LAUNCH_RSI_RESUME: usize = 0x1_4003_13ca;

pub(crate) const LAUNCH_RBX_2: usize = 0x1_4003_19d0;
pub(crate) const LAUNCH_RBX_2_RESUME: usize = 0x1_4003_19d6;

pub(crate) const LAUNCH_RSI_2: usize = 0x1_4003_1c46;
pub(crate) const LAUNCH_RSI_2_BYTES: [u8; 6] = [0x89, 0x86, 0xc4, 0x03, 0x00, 0x00];
pub(crate) const LAUNCH_RSI_2_RESUME: usize = 0x1_4003_1c4c;

/// `MOVSS XMM7, dword ptr [0x140352838]` — logic 130 burn thrust.
pub(crate) const BURN_130: usize = 0x1_4003_48f2;
pub(crate) const BURN_130_BYTES: [u8; 8] = [0xf3, 0x0f, 0x10, 0x3d, 0x3e, 0xdf, 0x31, 0x00];
pub(crate) const BURN_130_RESUME: usize = 0x1_4003_48fa;

/// `MOVSS XMM9, dword ptr [0x140352838]` — logic 132 burn thrust.
pub(crate) const BURN_132: usize = 0x1_4003_4c0c;
pub(crate) const BURN_132_BYTES: [u8; 9] = [0xf3, 0x44, 0x0f, 0x10, 0x0d, 0x23, 0xdc, 0x31, 0x00];
pub(crate) const BURN_132_RESUME: usize = 0x1_4003_4c15;

/// Body `m_logic_f2`: seconds of rocket thrust left (1.5 at launch, counted down per tick).
const BURN_TIME: usize = 0x3d4;
/// Ammo record: `item_name`, an `EscadraString`.
const AMMO_NAME: usize = 0x8;

// ---------------------------------------------------------------------------------------------
// Burn time (`+0x3d4`) stores. The three `FireShell` rocket cases set it to 1.5 right after they
// set `logic`; the 300 mm (logic 132) leaves it to `creatureLogic`'s state-0 block, which stores the
// same 1.5 at `0x14003538e` — that one also covers the part-launched 300 mm. Replacing these stores
// is what makes the burn configurable; the field is what the burn blocks count down and what the
// rocket-glow particle life is built from, so the flame follows it for free.
// ---------------------------------------------------------------------------------------------

/// `MOV dword ptr [RBX + 0x3d4], 0x3fc00000` — a rocket case in `FireShell`.
pub(crate) const BURN_TIME_RBX: usize = 0x1_4003_0e9d;
pub(crate) const BURN_TIME_RBX_BYTES: [u8; 10] =
    [0xc7, 0x83, 0xd4, 0x03, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x3f];
pub(crate) const BURN_TIME_RBX_RESUME: usize = 0x1_4003_0ea7;

pub(crate) const BURN_TIME_RSI: usize = 0x1_4003_13d1;
pub(crate) const BURN_TIME_RSI_BYTES: [u8; 10] =
    [0xc7, 0x86, 0xd4, 0x03, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x3f];
pub(crate) const BURN_TIME_RSI_RESUME: usize = 0x1_4003_13db;

pub(crate) const BURN_TIME_RSI_2: usize = 0x1_4003_1c53;
pub(crate) const BURN_TIME_RSI_2_RESUME: usize = 0x1_4003_1c5d;

/// `MOV dword ptr [RSI + 0x3d4], 0x3fc00000` in `creatureLogic` state 0 — logic 132 (300 mm).
pub(crate) const BURN_TIME_132: usize = 0x1_4003_538e;
pub(crate) const BURN_TIME_132_RESUME: usize = 0x1_4003_5398;

/// `mov rcx, rbx` / `mov rcx, rsi` — the launch callback takes the body in RCX.
const MOV_RCX_RBX: [u8; 3] = [0x48, 0x89, 0xd9];
const MOV_RCX_RSI: [u8; 3] = [0x48, 0x89, 0xf1];
/// `mov rdx, r15` — and the Ammo record in RDX.
const MOV_RDX_R15: [u8; 3] = [0x49, 0x8b, 0xd7];
/// `MOVAPS XMM7, XMM0` / `MOVAPS XMM9, XMM0` — the burn callback returns the thrust in XMM0.
const MOVAPS_XMM7_XMM0: [u8; 3] = [0x0f, 0x28, 0xf8];
const MOVAPS_XMM9_XMM0: [u8; 4] = [0x44, 0x0f, 0x28, 0xc8];

/// Upper bound on live entries; a battle never has this many rockets in the air at once, and a clear
/// only makes an unlucky rocket fall back to the global value for one tick before it is re-recorded.
const LIVE_LIMIT: usize = 512;

/// Hook counts, for the install summary.
pub(crate) const LAUNCH_HOOKS: usize = 4;
pub(crate) const THRUST_HOOKS: usize = 2;
pub(crate) const BURN_TIME_HOOKS: usize = 4;

/// What was recorded for a rocket body at launch.
#[derive(Clone, Copy)]
struct Live {
    acceleration: f32,
    burn: f32,
}

/// `body -> launch parameters` for rockets currently in the air, recorded at launch.
static LIVE: Mutex<Option<HashMap<usize, Live>>> = Mutex::new(None);

/// True for the two `creatureLogic` burn sites, which the per-ammo hooks replace.
pub(crate) fn is_burn_site(address: usize) -> bool {
    address == BURN_130 || address == BURN_132
}

pub(crate) fn launch_trampoline(setup: &[u8], body_move: &[u8], resume: usize) -> Trampoline {
    let mut trampoline = Trampoline::new();
    let mut argument_setup = setup.to_vec();
    argument_setup.extend_from_slice(body_move);
    argument_setup.extend_from_slice(&MOV_RDX_R15);
    // `ReturnType::None`: the original store is re-executed inside the setup, so RAX (the caliber)
    // must survive the callback.
    trampoline.preserved_call(
        on_rocket_launched as *const (),
        &argument_setup,
        &[],
        ReturnType::None,
    );
    trampoline.relative_jump(resume);
    trampoline
}

pub(crate) fn burn_trampoline(result_handling: &[u8], resume: usize) -> Trampoline {
    let mut trampoline = Trampoline::new();
    trampoline.preserved_call(
        on_burn_thrust as *const (),
        &MOV_RCX_RSI,
        result_handling,
        ReturnType::Xmm0,
    );
    trampoline.relative_jump(resume);
    trampoline
}

/// Replaces a `MOV dword ptr [body + 0x3d4], 1.5` store: the callback writes the configured burn
/// time instead, so nothing from the vanilla store has to be re-executed.
pub(crate) fn burn_time_trampoline(body_move: &[u8], resume: usize) -> Trampoline {
    let mut trampoline = Trampoline::new();
    trampoline.preserved_call(
        on_burn_time_store as *const (),
        body_move,
        &[],
        ReturnType::None,
    );
    trampoline.relative_jump(resume);
    trampoline
}

/// `mov rcx, rbx` — for the launch sites whose body is in RBX.
pub(crate) fn body_in_rbx() -> &'static [u8] {
    &MOV_RCX_RBX
}

/// `mov rcx, rsi` — for the launch sites whose body is in RSI.
pub(crate) fn body_in_rsi() -> &'static [u8] {
    &MOV_RCX_RSI
}

/// `movaps xmm7, xmm0` for the logic-130 site.
pub(crate) fn burn_result_130() -> &'static [u8] {
    &MOVAPS_XMM7_XMM0
}

/// `movaps xmm9, xmm0` for the logic-132 site.
pub(crate) fn burn_result_132() -> &'static [u8] {
    &MOVAPS_XMM9_XMM0
}

/// Records the ammo's configured acceleration and burn time for this rocket body.
unsafe extern "C" fn on_rocket_launched(body: *mut u8, ammo: *mut u8) {
    // SAFETY: the launch site is inside `FireShell`, where both pointers are live.
    let acceleration = unsafe { acceleration_for_ammo(ammo as usize) };
    // SAFETY: same live Ammo record.
    let burn = unsafe { burn_for_ammo(ammo as usize) };
    let mut guard = LIVE.lock().unwrap_or_else(|error| error.into_inner());
    let live = guard.get_or_insert_with(HashMap::new);
    if live.len() >= LIVE_LIMIT {
        live.clear();
    }
    live.insert(body as usize, Live { acceleration, burn });
}

/// The acceleration this Ammo record is configured for, or the global value.
unsafe fn acceleration_for_ammo(ammo: usize) -> f32 {
    let settings = crate::config();
    let fallback = settings.rocket_acceleration;
    if settings.rocket_acceleration_by_ammo.is_empty() {
        return fallback;
    }
    // SAFETY: `ammo` is the Ammo record `FireShell` was called with.
    let Some(name) = (unsafe { crate::string_at(ammo + AMMO_NAME) }) else {
        return fallback;
    };
    match crate::config::lookup(&settings.rocket_acceleration_by_ammo, name) {
        Some(acceleration) => acceleration,
        None => {
            crate::note_unmatched(
                &crate::UNMATCHED_AMMO,
                "ammo",
                name,
                settings.rocket_acceleration,
            );
            fallback
        }
    }
}

/// The burn time this Ammo record is configured for, or the global value.
unsafe fn burn_for_ammo(ammo: usize) -> f32 {
    let settings = crate::config();
    let fallback = settings.rocket_burn_seconds;
    if settings.rocket_burn_seconds_by_ammo.is_empty() {
        return fallback;
    }
    // SAFETY: `ammo` is the Ammo record `FireShell` was called with.
    let Some(name) = (unsafe { crate::string_at(ammo + AMMO_NAME) }) else {
        return fallback;
    };
    match crate::config::lookup(&settings.rocket_burn_seconds_by_ammo, name) {
        Some(seconds) => seconds,
        None => {
            crate::note_unmatched(
                &crate::UNMATCHED_BURN,
                "ammo (burn)",
                name,
                settings.rocket_burn_seconds,
            );
            fallback
        }
    }
}

/// Writes the configured burn time into the rocket body (`+0x3d4`).
unsafe extern "C" fn on_burn_time_store(body: *mut u8) {
    let body = body as usize;
    let seconds = burn_seconds_for(body);
    // SAFETY: the store site is where the game would have written the same field.
    unsafe { std::ptr::write_unaligned((body + BURN_TIME) as *mut f32, seconds) };
}

/// The burn time for this body: its per-ammo value recorded at launch, or the global one.
/// A 300 mm rocket launched as a ship part never passes a `FireShell` launch hook, so it has no
/// entry and takes the global value.
fn burn_seconds_for(body: usize) -> f32 {
    let fallback = crate::config().rocket_burn_seconds;
    let guard = LIVE.lock().unwrap_or_else(|error| error.into_inner());
    guard
        .as_ref()
        .and_then(|live| live.get(&body))
        .map(|entry| entry.burn)
        .unwrap_or(fallback)
}

/// The thrust for this rocket body: its recorded per-ammo value, or the global one.
unsafe extern "C" fn on_burn_thrust(body: *mut u8) -> f32 {
    let body = body as usize;
    // SAFETY: the burn site is inside `creatureLogic`, where the body is live.
    let remaining = unsafe { crate::read::<f32>(body + BURN_TIME) };
    let settings = crate::config();
    let mut guard = LIVE.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(live) = guard.as_mut() {
        if !(remaining > 0.0) {
            // The burn is over (the vanilla code takes the skip branch); the entry is dead weight
            // from here on, and the rocket never burns again.
            live.remove(&body);
            return settings.rocket_acceleration;
        }
        if let Some(entry) = live.get(&body) {
            return entry.acceleration;
        }
    }
    settings.rocket_acceleration
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burn_time_of(body: &[u8; 0x400]) -> f32 {
        f32::from_le_bytes(body[BURN_TIME..BURN_TIME + 4].try_into().unwrap())
    }

    #[test]
    fn the_burn_time_store_writes_the_recorded_value_or_the_scalar() {
        crate::install_test_config();
        let mut body = Box::new([0u8; 0x400]);
        let address = body.as_mut_ptr() as usize;

        // No launch record for this body: the scalar applies (default 1.5, the vanilla value).
        unsafe { on_burn_time_store(address as *mut u8) };
        assert_eq!(burn_time_of(&body), crate::config().rocket_burn_seconds);

        // With a record from the launch hook the per-ammo value wins.
        {
            let mut guard = LIVE.lock().unwrap_or_else(|error| error.into_inner());
            guard.get_or_insert_with(HashMap::new).insert(
                address,
                Live {
                    acceleration: 900.0,
                    burn: 2.75,
                },
            );
        }
        unsafe { on_burn_time_store(address as *mut u8) };
        assert_eq!(burn_time_of(&body), 2.75);

        // The accel hook returns the recorded acceleration, and drops the entry once the burn is
        // over so the table only ever holds live rockets.
        body[BURN_TIME..BURN_TIME + 4].copy_from_slice(&1.0f32.to_le_bytes());
        assert_eq!(unsafe { on_burn_thrust(address as *mut u8) }, 900.0);
        body[BURN_TIME..BURN_TIME + 4].copy_from_slice(&0.0f32.to_le_bytes());
        assert_eq!(
            unsafe { on_burn_thrust(address as *mut u8) },
            crate::config().rocket_acceleration
        );
        let guard = LIVE.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            guard.as_ref().is_some_and(|live| !live.contains_key(&address)),
            "the entry is dropped when the burn ends"
        );
    }

    #[test]
    fn burn_time_trampoline_builds_and_reaches_its_resume() {
        let address = 0x1_4010_0000usize;
        let code = burn_time_trampoline(body_in_rbx(), BURN_TIME_RBX_RESUME)
            .build(address)
            .expect("builds");
        assert_eq!(code[code.len() - 5], 0xe9);
        let displacement = i32::from_le_bytes(code[code.len() - 4..].try_into().unwrap()) as i64;
        assert_eq!(
            (address + code.len()) as i64 + displacement,
            BURN_TIME_RBX_RESUME as i64
        );
        assert!(code.windows(3).any(|window| window == body_in_rbx()));
    }

    #[test]
    fn the_burn_time_sites_are_the_vanilla_stores() {
        // `MOV dword ptr [body + 0x3d4], 0x3fc00000` — the RBX and RSI encodings.
        assert_eq!(
            BURN_TIME_RBX_BYTES,
            [0xc7, 0x83, 0xd4, 0x03, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x3f]
        );
        assert_eq!(
            BURN_TIME_RSI_BYTES,
            [0xc7, 0x86, 0xd4, 0x03, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x3f]
        );
        assert_eq!(BURN_TIME_RBX, 0x140030e9d);
        assert_eq!(BURN_TIME_RSI, 0x1400313d1);
        assert_eq!(BURN_TIME_RSI_2, 0x140031c53);
        assert_eq!(BURN_TIME_132, 0x14003538e);
        // Every resume point is the site plus the 10-byte store.
        assert_eq!(BURN_TIME_RBX_RESUME, BURN_TIME_RBX + 10);
        assert_eq!(BURN_TIME_RSI_RESUME, BURN_TIME_RSI + 10);
        assert_eq!(BURN_TIME_RSI_2_RESUME, BURN_TIME_RSI_2 + 10);
        assert_eq!(BURN_TIME_132_RESUME, BURN_TIME_132 + 10);
    }
}
