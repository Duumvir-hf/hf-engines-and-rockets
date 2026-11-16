//! HighFleet Steam 1.163 — **engine overheat thresholds and rocket flight tuning**.
//!
//! The game has one `500.0f` in its constant pool, at `0x140352838`, and 82 instructions read it.
//! Editing it changes three unrelated things at once; two of them are decoupled here:
//!
//! * **engine overheat**: `Engine_ApplyThrustForce` `0x140028360` trips a 3 s lockout when a part's
//!   temperature `+0x2ec` reaches 500:
//!   `14002861c  COMISS XMM0, dword ptr [0x140352838]` (7 bytes)
//! * **rocket acceleration**: `creatureLogic` applies `mass * 500` along the rocket's heading for the
//!   1.5 s burn (logics 130 and 132): `1400348f2  MOVSS XMM7, [..]`, `140034c0c  MOVSS XMM9, [..]`.
//!   The HUD's predicted rocket path — the dashed line the reticle shows — integrates the same
//!   acceleration, so its four sites (`1402a02d6`, `1402a02e4`, `1402a04c4`, `1402a04d1`) follow the
//!   same knob; otherwise the reticle would lie once the thrust changes.
//!
//! The third reader, the enemy pre-fire gun laser in `BattleHud_DrawCombatMarkers`
//! (`14029d538`/`d540`/`d82a`/`d832`), is deliberately left on the vanilla value: the reticle is not
//! meant to be modifiable. Those four sites are not touched.
//!
//! ## Mechanism: a displacement rewrite, not a hook
//!
//! Every reader is an `OP xmm, dword ptr [0x140352838]` whose RIP-relative `disp32` is the last four
//! bytes of the instruction. The mod allocates one page **near the image base** (a `disp32` only
//! reaches ±2 GiB, and mod DLLs load at `0x7ff...`), stores the scalars there, and rewrites each
//! site's four displacement bytes so it reads the configured float instead of the pool entry:
//!
//! ```text
//! disp32 = value_address - (site_address + instruction_length)
//! ```
//!
//! No trampoline, no stolen bytes, no register or flag side effects — the instruction keeps its
//! length and its target operand, only the address changes. Install verifies the opcode prefix and
//! the vanilla displacement before writing, so a different build is refused instead of corrupted.
//!
//! The other 75 readers of `0x140352838` keep the vanilla 500.0 and are untouched.
//!
//! ## Per-`m_oid` overrides (v0.2.0)
//!
//! Both knobs can also go below the pooled scalar, which needs real code, so they use trampolines
//! (see `engine_limits` and `rocket_ammo`):
//!
//! * **engine overheat per engine `m_oid`** — the temperature load in `Engine_ApplyThrustForce` is
//!   replaced by a callback that hands the vanilla compare a shifted temperature, so the vanilla
//!   `>= 500` test becomes `>= this engine's threshold`. The scalar knob keeps working for engines
//!   that are not listed, and the two compose.
//! * **rocket acceleration per ammo `item_name`** — the four `FireShell` rocket cases pass the Ammo
//!   record through `R15` where they store `body->logic = ammo->caliber`; a hook there records the
//!   configured acceleration per rocket body in the mod's own table, and the two `creatureLogic`
//!   burn sites ask that table. Nothing is written into the game's objects.
//!
//! Config: `Modloader/config/tuning.json`. `500.0` is vanilla, so a value left at 500.0 patches
//! nothing, and an empty per-`m_oid` map installs no hook at all.

highfleet_mod_api::export_logger!();

mod config;
mod engine_limits;
mod rocket_ammo;

use std::{
    collections::HashSet,
    ffi::{c_char, CStr},
    mem::{size_of, MaybeUninit},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

use patchy::PatchSession;
use windows_sys::Win32::System::{
    LibraryLoader::GetModuleHandleA,
    Memory::{
        VirtualAlloc, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_FREE,
        MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE,
    },
};

const IMAGE_BASE: usize = 0x1_4000_0000;
const _: () = assert!(IMAGE_BASE == 0x140000000);

/// The pooled `500.0f` every reader below uses. Never written to.
const POOL: usize = 0x1_4035_2838;
const _: () = assert!(POOL == 0x140352838);

/// Which configured value a site reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Overheat,
    RocketAcceleration,
}

/// One patched instruction.
struct Site {
    address: usize,
    /// The instruction's bytes before the RIP-relative displacement (opcodes + ModRM).
    opcode: &'static [u8],
    /// The displacement the Steam 1.163 build carries; it resolves to [`POOL`].
    vanilla_disp: i32,
    slot: Slot,
    what: &'static str,
}

impl Site {
    /// The whole instruction, displacement included.
    const fn len(&self) -> usize {
        self.opcode.len() + 4
    }
}

/// The 11 readers that belong to the three knobs (see the module docs for the sites' meaning).
const SITES: [Site; 7] = [
    // Engine_ApplyThrustForce: part temperature +0x2ec >= 500 -> +0x810 = 3.0, no thrust.
    Site {
        address: 0x1_4002_861c,
        opcode: &[0x0f, 0x2f, 0x05],
        vanilla_disp: 0x0032_a215,
        slot: Slot::Overheat,
        what: "Engine_ApplyThrustForce: part temperature >= threshold trips the 3 s lockout",
    },
    // creatureLogic, logic 130 (cluster rocket): burn force = mass * 500.
    Site {
        address: 0x1_4003_48f2,
        opcode: &[0xf3, 0x0f, 0x10, 0x3d],
        vanilla_disp: 0x0031_df3e,
        slot: Slot::RocketAcceleration,
        what: "creatureLogic logic 130: rocket burn acceleration",
    },
    // creatureLogic, logic 132 (300 mm rocket): same thrust.
    Site {
        address: 0x1_4003_4c0c,
        opcode: &[0xf3, 0x44, 0x0f, 0x10, 0x0d],
        vanilla_disp: 0x0031_dc23,
        slot: Slot::RocketAcceleration,
        what: "creatureLogic logic 132: rocket burn acceleration",
    },
    // BattleHud_DrawCombatMarkers: predicted rocket path, two blocks (straight / steering rocket).
    Site {
        address: 0x1_402a_02d6,
        opcode: &[0xf3, 0x44, 0x0f, 0x59, 0x2d],
        vanilla_disp: 0x000b_2559,
        slot: Slot::RocketAcceleration,
        what: "BattleHud_DrawCombatMarkers: rocket path preview, block 1, x",
    },
    Site {
        address: 0x1_402a_02e4,
        opcode: &[0xf3, 0x44, 0x0f, 0x59, 0x35],
        vanilla_disp: 0x000b_254b,
        slot: Slot::RocketAcceleration,
        what: "BattleHud_DrawCombatMarkers: rocket path preview, block 1, y",
    },
    Site {
        address: 0x1_402a_04c4,
        opcode: &[0xf3, 0x0f, 0x59, 0x35],
        vanilla_disp: 0x000b_236c,
        slot: Slot::RocketAcceleration,
        what: "BattleHud_DrawCombatMarkers: rocket path preview, block 2, x",
    },
    Site {
        address: 0x1_402a_04d1,
        opcode: &[0xf3, 0x44, 0x0f, 0x59, 0x35],
        vanilla_disp: 0x000b_235e,
        slot: Slot::RocketAcceleration,
        what: "BattleHud_DrawCombatMarkers: rocket path preview, block 2, y",
    },
];

// The grouped literals above are easy to misread (`0x140_0000_000` is ten digits, not nine).
const _: () = assert!(SITES[0].address == 0x14002861c);
const _: () = assert!(SITES[1].address == 0x1400348f2);
const _: () = assert!(SITES[2].address == 0x140034c0c);
const _: () = assert!(SITES[3].address == 0x1402a02d6);
const _: () = assert!(SITES[4].address == 0x1402a02e4);
const _: () = assert!(SITES[5].address == 0x1402a04c4);
const _: () = assert!(SITES[6].address == 0x1402a04d1);

/// The three values, in one page near the image base. Written once at install (and by hand later
/// for live tuning), read by the patched instructions.
#[repr(C)]
struct Values {
    overheat: f32,
    rocket_acceleration: f32,
}

static SUPPORTED_VERSION: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static VERBOSE: AtomicBool = AtomicBool::new(false);
static VALUES: OnceLock<usize> = OnceLock::new();
/// The sanitised config, for the hook callbacks (which run on the game's threads).
pub(crate) static CONFIG: OnceLock<config::Config> = OnceLock::new();

fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// The live config; `init` stores it before any hook can run.
pub(crate) fn config() -> &'static config::Config {
    CONFIG.get_or_init(config::Config::default)
}

/// One config for every unit test that exercises a callback, so test order cannot matter: the first
/// caller installs it and the rest reuse it.
#[cfg(test)]
pub(crate) fn install_test_config() -> &'static config::Config {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    CONFIG.get_or_init(|| {
        let mut settings = config::Config::default();
        settings.engine_overheat_threshold = 500.0;
        settings.engine_overheat_by_oid = [("MDL_ENGINE_01".to_owned(), 400.0)].into();
        settings
    })
}

/// Body `m_oid`: an `EscadraString` (inline buffer, length, capacity) at `+0x190`.
const OID: usize = 0x190;
const NAME_LENGTH: usize = 0x10;
const NAME_CAPACITY: usize = 0x18;
/// Longest name accepted; the real ids are short `MDL_...` / `ITEM_...` names.
const NAME_LIMIT: u64 = 255;

/// Reads a `Copy` value out of the game's memory.
///
/// # Safety
///
/// `address` must be readable for `size_of::<T>()` bytes.
pub(crate) unsafe fn read<T: Copy>(address: usize) -> T {
    // SAFETY: the caller guarantees readability; `read_unaligned` handles records that are not
    // naturally aligned to `T`.
    unsafe { std::ptr::read_unaligned(address as *const T) }
}

/// Reads an `EscadraString` (16-byte inline buffer, then `u64` length and capacity) at `address`.
///
/// # Safety
///
/// The returned slice points into the game's own memory; it must not outlive the record.
pub(crate) unsafe fn string_at(address: usize) -> Option<&'static str> {
    if !readable(address, 0x20) {
        return None;
    }
    // SAFETY: the header was just proved readable.
    let length: u64 = unsafe { read(address + NAME_LENGTH) };
    // SAFETY: same header.
    let capacity: u64 = unsafe { read(address + NAME_CAPACITY) };
    if length == 0 || length > NAME_LIMIT || capacity < length || capacity > 0x1000 {
        return None;
    }
    let data = if capacity <= 15 {
        address
    // SAFETY: the buffer holds either the inline name or the heap pointer at `address`.
    } else {
        unsafe { read::<usize>(address) }
    };
    if !readable(data, length as usize) {
        return None;
    }
    // SAFETY: just proved readable, and the length came from the same record.
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length as usize) };
    std::str::from_utf8(bytes).ok()
}

/// The `m_oid` of a part or body.
///
/// # Safety
///
/// `body` must be a live Body.
pub(crate) unsafe fn oid_of(body: usize) -> Option<&'static str> {
    // SAFETY: the same live Body the caller is handling.
    unsafe { string_at(body + OID) }
}

/// Names seen at runtime that have no entry in one of the per-`m_oid` maps, so they are logged once
/// and can be copied straight out of `client.log`.
pub(crate) static UNMATCHED_ENGINES: Mutex<Option<HashSet<String>>> = Mutex::new(None);
pub(crate) static UNMATCHED_AMMO: Mutex<Option<HashSet<String>>> = Mutex::new(None);
pub(crate) static UNMATCHED_BURN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Upper bound on remembered names; enough to cover every vanilla part and ammo id.
const UNMATCHED_LIMIT: usize = 96;

/// Logs `name` the first time it is seen without an override (bounded, cheap after the cap).
pub(crate) fn note_unmatched(
    registry: &'static Mutex<Option<HashSet<String>>>,
    kind: &str,
    name: &str,
    scalar: f32,
) {
    let mut guard = registry.lock().unwrap_or_else(|error| error.into_inner());
    let seen = guard.get_or_insert_with(HashSet::new);
    if seen.len() >= UNMATCHED_LIMIT || !seen.insert(name.to_owned()) {
        return;
    }
    log::info!(
        "hf_tuning: {kind} {name} has no override (scalar {scalar} applies) — this is the exact id \
         to put in {}",
        config::PATH
    );
}

fn value_of(config: &config::Config, slot: Slot) -> f32 {
    match slot {
        Slot::Overheat => config.engine_overheat_threshold,
        Slot::RocketAcceleration => config.rocket_acceleration,
    }
}

fn slot_name(slot: Slot) -> &'static str {
    match slot {
        Slot::Overheat => "engine overheat threshold",
        Slot::RocketAcceleration => "rocket acceleration",
    }
}

/// `disp32` that makes the instruction at `address` (of `len` bytes) read `target`.
fn disp_for(address: usize, len: usize, target: usize) -> Option<i32> {
    let end = address.checked_add(len)?;
    let distance = (target as i64).checked_sub(end as i64)?;
    i32::try_from(distance).ok()
}

/// The address of one value inside the allocated page.
unsafe fn slot_address(values: *mut Values, slot: Slot) -> usize {
    // SAFETY: `values` points at a committed page holding one `Values`; `addr_of!` does not create a
    // reference, so nothing is read or written.
    unsafe {
        match slot {
            Slot::Overheat => std::ptr::addr_of!((*values).overheat) as usize,
            Slot::RocketAcceleration => {
                std::ptr::addr_of!((*values).rocket_acceleration) as usize
            }
        }
    }
}

/// True when `size` bytes at `address` are committed and readable.
fn readable(address: usize, size: usize) -> bool {
    if address < 0x1_0000 {
        return false;
    }
    let mut information = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
    // SAFETY: VirtualQuery only inspects an address; the buffer is sized and aligned for the struct.
    let queried = unsafe {
        VirtualQuery(
            address as *const _,
            information.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if queried != size_of::<MEMORY_BASIC_INFORMATION>() {
        return false;
    }
    // SAFETY: VirtualQuery returned a complete structure.
    let information = unsafe { information.assume_init() };
    if information.State != MEM_COMMIT
        || information.Protect == PAGE_NOACCESS
        || information.Protect & PAGE_GUARD != 0
    {
        return false;
    }
    let region_offset = address.saturating_sub(information.BaseAddress as usize);
    size <= information.RegionSize.saturating_sub(region_offset)
}

/// Tries to reserve the values page at exactly `probe` (which must lie in free memory).
fn allocate_at(probe: usize) -> Option<*mut Values> {
    // SAFETY: an exact-address reservation; the address is checked by the caller via VirtualQuery.
    let pointer = unsafe {
        VirtualAlloc(
            probe as *const _,
            0x1000,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };
    if pointer.is_null() {
        None
    } else {
        Some(pointer as *mut Values)
    }
}

/// Allocates the values page within ±1.25 GiB of the image base.
/// Windows reserves virtual memory in 64 KiB *allocation granularity* steps, and an unaligned
/// request is rounded **down**. A free region usually starts right after the image (page-aligned but
/// not granularity-aligned), so asking for the region's first byte rounds down into the occupied
/// image and fails — which is why v0.2.0's first allocator never found a page in a real game process.
/// Candidates are therefore rounded **up**, and must still leave room for the page inside the region.
const ALLOCATION_GRANULARITY: usize = 0x1_0000;

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
}

/// First granularity-aligned page address at or after `probe` that still lies inside the region.
/// Rounding **up** is the whole point: a reservation that is not granularity-aligned is rounded down
/// by Windows, and a free region usually starts right after the image, so rounding down lands inside
/// the image and the reservation fails.
fn page_candidate(base: usize, size: usize, probe: usize) -> Option<usize> {
    let end = base.checked_add(size)?;
    let candidate = align_up(probe, ALLOCATION_GRANULARITY)?;
    if candidate.checked_add(0x1000)? > end {
        return None;
    }
    Some(candidate)
}

/// Tries to reserve the values page inside the free region `[base, base + size)`, at the first
/// granularity-aligned address at or after `probe`.
fn allocate_in_region(base: usize, size: usize, probe: usize) -> Option<*mut Values> {
    let end = base.checked_add(size)?;
    let mut candidate = page_candidate(base, size, probe)?;
    for _ in 0..4 {
        if let Some(pointer) = allocate_at(candidate) {
            return Some(pointer);
        }
        candidate = candidate.checked_add(ALLOCATION_GRANULARITY)?;
        if candidate.checked_add(0x1000)? > end {
            return None;
        }
    }
    None
}

/// `(base, size, free)` of the memory region containing `address`.
fn region_at(address: usize) -> Option<(usize, usize, bool)> {
    let mut information = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
    // SAFETY: VirtualQuery only inspects an address; the buffer is sized and aligned for the struct.
    let queried = unsafe {
        VirtualQuery(
            address as *const _,
            information.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if queried != size_of::<MEMORY_BASIC_INFORMATION>() {
        return None;
    }
    // SAFETY: VirtualQuery returned a complete structure.
    let information = unsafe { information.assume_init() };
    Some((
        information.BaseAddress as usize,
        information.RegionSize,
        information.State == MEM_FREE,
    ))
}

/// Allocates the values page within ±1.25 GiB of the image base.
///
/// A `disp32` reaches ±2 GiB, and the patched sites sit in the first 3 MB of the image, so a page
/// anywhere near the image base is in range while the mod DLL's own memory (`0x7ff...`) is not.
fn allocate_values() -> Option<*mut Values> {
    const SPAN: usize = 0x5000_0000;
    let mut free_regions = 0usize;
    let mut largest = (0usize, 0usize);

    // Upwards first: the image is below, and the space above it is the least contested.
    let mut probe = IMAGE_BASE;
    while probe.saturating_sub(IMAGE_BASE) <= SPAN {
        let Some((base, size, free)) = region_at(probe) else {
            break;
        };
        if free && size >= 0x2000 {
            free_regions += 1;
            if size > largest.1 {
                largest = (base, size);
            }
            if let Some(pointer) = allocate_in_region(base, size, probe) {
                log::info!(
                    "hf_tuning: values page at {:#x} ({} bytes above the image base)",
                    pointer as usize,
                    probe - IMAGE_BASE
                );
                return Some(pointer);
            }
        }
        match base.checked_add(size) {
            Some(next) if next > probe => probe = next,
            _ => break,
        }
    }

    // Then downwards, one region at a time.
    let mut probe = IMAGE_BASE - ALLOCATION_GRANULARITY;
    while probe >= 0x1_0000 && IMAGE_BASE.saturating_sub(probe) <= SPAN {
        let Some((base, size, free)) = region_at(probe) else {
            break;
        };
        if free && size >= 0x2000 {
            free_regions += 1;
            if size > largest.1 {
                largest = (base, size);
            }
            if let Some(pointer) = allocate_in_region(base, size, probe) {
                log::info!(
                    "hf_tuning: values page at {:#x} ({} bytes below the image base)",
                    pointer as usize,
                    IMAGE_BASE - probe
                );
                return Some(pointer);
            }
        }
        if base <= 0x1_0000 {
            break;
        }
        probe = base - 1;
    }

    log::error!(
        "hf_tuning: no free page for the values table within {SPAN:#x} of the image base \
         ({free_regions} free region(s) examined, largest {:#x} at {:#x})",
        largest.1,
        largest.0
    );
    None
}

/// Writes a 32-bit displacement into the middle of a code page.
unsafe fn write_disp(target: usize, disp: i32) -> Result<(), String> {
    let mut previous = 0u32;
    // SAFETY: `target` is inside a verified instruction; the protection change is scoped to 4 bytes
    // and restored immediately.
    let changed = unsafe {
        VirtualProtect(
            target as *const _,
            4,
            PAGE_EXECUTE_READWRITE,
            &mut previous,
        )
    };
    if changed == 0 {
        return Err(format!(
            "VirtualProtect failed for {target:#x}; site not patched"
        ));
    }
    // SAFETY: 4 writable bytes, written unaligned because the disp starts mid-instruction.
    unsafe { std::ptr::write_unaligned(target as *mut u32, disp as u32) };
    let mut restored = 0u32;
    // SAFETY: restoring the original protection; the return value is not needed.
    unsafe { VirtualProtect(target as *const _, 4, previous, &mut restored) };
    Ok(())
}

/// Points one site at `value_address`.
///
/// Verifies the opcode prefix and the vanilla displacement before writing, so an unexpected build is
/// refused rather than patched into nonsense. The Steam build unpacks itself while the modloader
/// starts, so a mismatch is retried (20 ? 100 ms) instead of being treated as fatal at once.
unsafe fn retarget(site: &Site, value_address: usize) -> Result<i32, String> {
    let len = site.len();
    let Some(new_disp) = disp_for(site.address, len, value_address) else {
        return Err(format!(
            "{:#x}: the value at {value_address:#x} is out of disp32 range; site skipped",
            site.address
        ));
    };
    let mut problem = String::from("not readable");
    for attempt in 0..=20 {
        if readable(site.address, len) {
            // SAFETY: the range was just proved readable and is exactly the instruction's length.
            let bytes = unsafe { std::slice::from_raw_parts(site.address as *const u8, len) };
            if &bytes[..site.opcode.len()] == site.opcode {
                let current = i32::from_le_bytes([
                    bytes[site.opcode.len()],
                    bytes[site.opcode.len() + 1],
                    bytes[site.opcode.len() + 2],
                    bytes[site.opcode.len() + 3],
                ]);
                if current == new_disp {
                    return Ok(new_disp);
                }
                if current == site.vanilla_disp {
                    if attempt > 0 {
                        log::info!(
                            "hf_tuning: {:#x} matched after {attempt} retr(ies)",
                            site.address
                        );
                    }
                    // SAFETY: the instruction bytes were verified above.
                    unsafe { write_disp(site.address + site.opcode.len(), new_disp)? };
                    return Ok(new_disp);
                }
                let resolves_to = (site.address + len) as i64 + current as i64;
                problem = format!(
                    "displacement {current:#x} points at {resolves_to:#x}, not at the 500 pool"
                );
            } else {
                problem = format!(
                    "opcode {:02x?} differs from the Steam 1.163 build",
                    &bytes[..site.opcode.len()]
                );
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("{:#x}: {problem}; site skipped", site.address))
}

#[no_mangle]
unsafe extern "C" fn version(version: *const c_char) -> bool {
    // SAFETY: the loader passes a live NUL-terminated version string.
    let supported =
        !version.is_null() && unsafe { CStr::from_ptr(version) }.to_bytes() == b"Steam 1.163";
    SUPPORTED_VERSION.store(supported, Ordering::Relaxed);
    supported
}

#[no_mangle]
unsafe extern "C" fn init() -> bool {
    let config = config::load().sanitised();
    VERBOSE.store(config.verbose_logging, Ordering::Relaxed);
    // SAFETY: a null module name asks for the main module.
    let image_base = unsafe { GetModuleHandleA(std::ptr::null()) } as usize;
    let supported = SUPPORTED_VERSION.load(Ordering::Relaxed);
    log::info!(
        "hf_tuning v{}: image base {image_base:#x}, version {}",
        env!("CARGO_PKG_VERSION"),
        if supported { "Steam 1.163" } else { "unsupported" }
    );
    if !supported {
        log::error!("hf_tuning: needs Steam 1.163; not started");
        return false;
    }
    if image_base != IMAGE_BASE {
        log::error!(
            "hf_tuning: unexpected image base {image_base:#x} (want {IMAGE_BASE:#x}); not started"
        );
        return false;
    }
    if !config.enabled {
        log::info!("hf_tuning: disabled in {}", config::PATH);
        return true;
    }
    if INSTALLED.load(Ordering::Relaxed) {
        return true;
    }

    log::info!(
        "hf_tuning: overheat {} ({} engine override(s)), rocket acceleration {} ({} ammo override(s)), \
         rocket burn {}s ({} ammo override(s)); vanilla {} / {}s",
        config.engine_overheat_threshold,
        config.engine_overheat_by_oid.len(),
        config.rocket_acceleration,
        config.rocket_acceleration_by_ammo.len(),
        config.rocket_burn_seconds,
        config.rocket_burn_seconds_by_ammo.len(),
        config::VANILLA,
        config::BURN_VANILLA
    );
    let _ = CONFIG.set(config.clone());

    for (key, _) in &config.unknown {
        log::warn!(
            "hf_tuning: ignoring unknown config key '{key}' (removed knob or typo) — valid keys: \
             engine_overheat_threshold, engine_overheat_by_oid, rocket_acceleration, \
             rocket_acceleration_by_ammo, rocket_burn_seconds, rocket_burn_seconds_by_ammo"
        );
    }

    for (oid, limit) in &config.engine_overheat_by_oid {
        log::info!("hf_tuning: engine {oid} overheats at {limit}");
    }
    for (name, acceleration) in &config.rocket_acceleration_by_ammo {
        log::info!("hf_tuning: ammo {name} accelerates at {acceleration}");
    }
    for (name, seconds) in &config.rocket_burn_seconds_by_ammo {
        log::info!("hf_tuning: ammo {name} burns for {seconds}s");
    }

    // Hooks first: the two burn sites are replaced wholesale when the per-ammo hooks go in, so the
    // scalar displacement patch must not touch them (patchy verifies the original bytes).
    let engine_wanted = !config.engine_overheat_by_oid.is_empty();
    let acceleration_wanted = !config.rocket_acceleration_by_ammo.is_empty();
    let burn_time_wanted = config::burn_is_tuned(&config);
    let rocket_wanted = acceleration_wanted || burn_time_wanted;
    let mut hooks = 0;
    if engine_wanted {
        hooks += unsafe { install_engine_limit_hook() };
    }
    let mut thrust_hooked = false;
    if rocket_wanted {
        let (launch, thrust, burn_time) =
            unsafe { install_rocket_hooks(acceleration_wanted, burn_time_wanted) };
        hooks += launch + thrust + burn_time;
        thrust_hooked = thrust > 0;
    }
    let burn_hooked = thrust_hooked;

    let wanted: Vec<&Site> = SITES
        .iter()
        .filter(|site| config::is_tuned(value_of(&config, site.slot)))
        .filter(|site| !(burn_hooked && rocket_ammo::is_burn_site(site.address)))
        .collect();
    if burn_hooked {
        log::info!(
            "hf_tuning: the two burn sites are served by the per-ammo hooks; their scalar value is \
             the fallback inside the hook"
        );
    }
    if wanted.is_empty() && !engine_wanted && !rocket_wanted {
        log::info!(
            "hf_tuning: every value is vanilla and no per-m_oid override is set; nothing to patch \
             (edit {} to tune)",
            config::PATH
        );
        return true;
    }

    let mut patched = 0;
    if wanted.is_empty() {
        if verbose() {
            log::info!("hf_tuning: no scalar knob differs; the value page is not needed");
        }
    } else if let Some(values) = allocate_values() {
        // SAFETY: `values` is a fresh, committed, writable page of one `Values`.
        unsafe {
            std::ptr::write(
                values,
                Values {
                    overheat: config.engine_overheat_threshold,
                    rocket_acceleration: config.rocket_acceleration,
                },
            );
        }
        let _ = VALUES.set(values as usize);

        for site in &wanted {
            // SAFETY: the page was just written above.
            let value_address = unsafe { slot_address(values, site.slot) };
            // SAFETY: `retarget` verifies the instruction before touching it.
            match unsafe { retarget(site, value_address) } {
                Ok(disp) => {
                    patched += 1;
                    log::info!(
                        "hf_tuning: {:#x} reads the {} ({}) via disp {disp:#010x} — {}",
                        site.address,
                        slot_name(site.slot),
                        value_of(&config, site.slot),
                        site.what
                    );
                }
                Err(error) => log::error!("hf_tuning: {error}"),
            }
        }
        if verbose() {
            for site in SITES.iter().filter(|site| {
                !config::is_tuned(value_of(&config, site.slot))
            }) {
                log::info!(
                    "hf_tuning: {:#x} left at vanilla 500.0 ({})",
                    site.address,
                    site.what
                );
            }
        }
    } else {
        log::error!(
            "hf_tuning: the scalar knobs need a writable page near the image base; none could be \
             allocated, so they are disabled (the per-m_oid hooks are unaffected)"
        );
    }

    if patched == 0 && hooks == 0 {
        log::error!("hf_tuning: nothing could be installed; the mod does nothing");
        return true;
    }
    INSTALLED.store(true, Ordering::Relaxed);
    log::info!(
        "hf_tuning: {patched}/{} value site(s) patched, {hooks} hook(s) installed",
        wanted.len()
    );
    true
}

/// Waits for `bytes` to appear at `address`, then verifies them.
///
/// The Steam build unpacks itself while the modloader starts, so the first check can land on still
/// encrypted or not yet committed memory.
fn wait_for(address: usize, bytes: &[u8]) -> Result<(), String> {
    for attempt in 0..=20 {
        if readable(address, bytes.len()) {
            // SAFETY: just proved the range readable.
            let current = unsafe { std::slice::from_raw_parts(address as *const u8, bytes.len()) };
            if current == bytes {
                if attempt > 0 {
                    log::info!("hf_tuning: {address:#x} matched after {attempt} retr(ies)");
                }
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!(
        "code at {address:#x} differs from the Steam 1.163 build; not patched"
    ))
}

/// Installs the per-engine overheat hook. Returns 1 when it went in.
unsafe fn install_engine_limit_hook() -> usize {
    if let Err(error) = wait_for(engine_limits::SITE, &engine_limits::SITE_BYTES) {
        log::error!("hf_tuning: {error}");
        return 0;
    }
    let mut session = PatchSession::new();
    // SAFETY: the site's bytes were verified above.
    if let Err(error) = unsafe {
        session.detour_trampoline(
            engine_limits::SITE,
            engine_limits::SITE_BYTES.len(),
            engine_limits::trampoline(),
        )
    } {
        log::error!("hf_tuning: engine overheat hook not prepared: {error}");
        return 0;
    }
    // SAFETY: a single verified site; the session is installed permanently below.
    match unsafe { session.install_permanently() } {
        Ok(()) => {
            log::info!(
                "hf_tuning: engine overheat hook at {:#x} ({} m_oid override(s))",
                engine_limits::SITE,
                config().engine_overheat_by_oid.len()
            );
            1
        }
        Err(error) => {
            log::error!("hf_tuning: engine overheat hook not installed: {error}");
            0
        }
    }
}

/// Installs the per-ammo rocket hooks.
///
/// The four `FireShell` launch hooks are always needed — they are what records the Ammo record for a
/// rocket body. The two burn-thrust hooks serve `rocket_acceleration_by_ammo`; the four `+0x3d4`
/// stores serve `rocket_burn_seconds[_by_ammo]`. Returns `(launch, thrust, burn_time)` counts, all
/// zero when anything failed (the session is transactional).
unsafe fn install_rocket_hooks(acceleration: bool, burn_time: bool) -> (usize, usize, usize) {
    // (site, original bytes, body register move, resume address)
    let launches: [(usize, &[u8], &[u8], usize); 4] = [
        (
            rocket_ammo::LAUNCH_RBX,
            &rocket_ammo::LAUNCH_RBX_BYTES,
            rocket_ammo::body_in_rbx(),
            rocket_ammo::LAUNCH_RBX_RESUME,
        ),
        (
            rocket_ammo::LAUNCH_RSI,
            &rocket_ammo::LAUNCH_RSI_BYTES,
            rocket_ammo::body_in_rsi(),
            rocket_ammo::LAUNCH_RSI_RESUME,
        ),
        (
            rocket_ammo::LAUNCH_RBX_2,
            &rocket_ammo::LAUNCH_RBX_BYTES,
            rocket_ammo::body_in_rbx(),
            rocket_ammo::LAUNCH_RBX_2_RESUME,
        ),
        (
            rocket_ammo::LAUNCH_RSI_2,
            &rocket_ammo::LAUNCH_RSI_2_BYTES,
            rocket_ammo::body_in_rsi(),
            rocket_ammo::LAUNCH_RSI_2_RESUME,
        ),
    ];
    let burns: [(usize, &[u8], &[u8], usize, &str); 2] = [
        (
            rocket_ammo::BURN_130,
            &rocket_ammo::BURN_130_BYTES,
            rocket_ammo::burn_result_130(),
            rocket_ammo::BURN_130_RESUME,
            "logic 130",
        ),
        (
            rocket_ammo::BURN_132,
            &rocket_ammo::BURN_132_BYTES,
            rocket_ammo::burn_result_132(),
            rocket_ammo::BURN_132_RESUME,
            "logic 132",
        ),
    ];
    // `MOV dword ptr [body + 0x3d4], 1.5` stores: three rocket cases plus logic 132's state 0.
    let burn_times: [(usize, &[u8], &[u8], usize, &str); 4] = [
        (
            rocket_ammo::BURN_TIME_RBX,
            &rocket_ammo::BURN_TIME_RBX_BYTES,
            rocket_ammo::body_in_rbx(),
            rocket_ammo::BURN_TIME_RBX_RESUME,
            "FireShell rocket case",
        ),
        (
            rocket_ammo::BURN_TIME_RSI,
            &rocket_ammo::BURN_TIME_RSI_BYTES,
            rocket_ammo::body_in_rsi(),
            rocket_ammo::BURN_TIME_RSI_RESUME,
            "FireShell rocket case",
        ),
        (
            rocket_ammo::BURN_TIME_RSI_2,
            &rocket_ammo::BURN_TIME_RSI_BYTES,
            rocket_ammo::body_in_rsi(),
            rocket_ammo::BURN_TIME_RSI_2_RESUME,
            "FireShell rocket case",
        ),
        (
            rocket_ammo::BURN_TIME_132,
            &rocket_ammo::BURN_TIME_RSI_BYTES,
            rocket_ammo::body_in_rsi(),
            rocket_ammo::BURN_TIME_132_RESUME,
            "creatureLogic state 0 (300 mm)",
        ),
    ];

    let mut session = PatchSession::new();
    for (site, bytes, body_move, resume) in launches {
        if let Err(error) = wait_for(site, bytes) {
            log::error!("hf_tuning: {error}");
            return (0, 0, 0);
        }
        // SAFETY: the site's bytes were verified above.
        if let Err(error) = unsafe {
            session.detour_trampoline(
                site,
                bytes.len(),
                rocket_ammo::launch_trampoline(bytes, body_move, resume),
            )
        } {
            log::error!("hf_tuning: rocket launch hook at {site:#x} not prepared: {error}");
            return (0, 0, 0);
        }
        log::info!("hf_tuning: rocket launch hook at {site:#x}");
    }
    if acceleration {
        for (site, bytes, result, resume, logic) in burns {
            if let Err(error) = wait_for(site, bytes) {
                log::error!("hf_tuning: {error}");
                return (0, 0, 0);
            }
            // SAFETY: the site's bytes were verified above.
            if let Err(error) = unsafe {
                session.detour_trampoline(
                    site,
                    bytes.len(),
                    rocket_ammo::burn_trampoline(result, resume),
                )
            } {
                log::error!("hf_tuning: rocket burn hook at {site:#x} not prepared: {error}");
                return (0, 0, 0);
            }
            log::info!("hf_tuning: rocket burn hook at {site:#x} ({logic})");
        }
    }
    if burn_time {
        for (site, bytes, body_move, resume, what) in burn_times {
            if let Err(error) = wait_for(site, bytes) {
                log::error!("hf_tuning: {error}");
                return (0, 0, 0);
            }
            // SAFETY: the site's bytes were verified above.
            if let Err(error) = unsafe {
                session.detour_trampoline(
                    site,
                    bytes.len(),
                    rocket_ammo::burn_time_trampoline(body_move, resume),
                )
            } {
                log::error!("hf_tuning: rocket burn-time hook at {site:#x} not prepared: {error}");
                return (0, 0, 0);
            }
            log::info!("hf_tuning: rocket burn-time hook at {site:#x} ({what})");
        }
    }
    // SAFETY: every site was verified above; the session is installed permanently.
    match unsafe { session.install_permanently() } {
        Ok(()) => {
            let (launch, thrust, burn) = (
                rocket_ammo::LAUNCH_HOOKS,
                if acceleration {
                    rocket_ammo::THRUST_HOOKS
                } else {
                    0
                },
                if burn_time {
                    rocket_ammo::BURN_TIME_HOOKS
                } else {
                    0
                },
            );
            log::info!(
                "hf_tuning: rocket hooks installed (launch {launch}, thrust {thrust}, burn time {burn}); \
                 overrides: {} ammo acceleration, {} ammo burn, burn scalar {}s",
                config().rocket_acceleration_by_ammo.len(),
                config().rocket_burn_seconds_by_ammo.len(),
                config().rocket_burn_seconds
            );
            (launch, thrust, burn)
        }
        Err(error) => {
            log::error!("hf_tuning: rocket hooks not installed: {error}");
            (0, 0, 0)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vanilla_disp_resolves_to_the_pool() {
        for site in &SITES {
            let resolves = (site.address + site.len()) as i64 + site.vanilla_disp as i64;
            assert_eq!(
                resolves, POOL as i64,
                "{:#x} does not resolve to the 500 pool",
                site.address
            );
        }
    }

    #[test]
    fn instruction_lengths_match_the_disassembly() {
        let lengths: Vec<usize> = SITES.iter().map(Site::len).collect();
        assert_eq!(lengths, [7, 8, 9, 9, 9, 8, 9]);
    }

    /// The full instruction as Ghidra shows it in the Steam 1.163 program, displacement included.
    /// A typo in an opcode (same length, wrong ModRM) would otherwise only show up at install time.
    /// The four enemy-aim-line sites are deliberately absent: the reticle must stay vanilla.
    #[test]
    fn instruction_bytes_match_the_disassembly() {
        const EXPECTED: [[u8; 9]; 7] = [
            [0x0f, 0x2f, 0x05, 0x15, 0xa2, 0x32, 0x00, 0x00, 0x00],
            [0xf3, 0x0f, 0x10, 0x3d, 0x3e, 0xdf, 0x31, 0x00, 0x00],
            [0xf3, 0x44, 0x0f, 0x10, 0x0d, 0x23, 0xdc, 0x31, 0x00],
            [0xf3, 0x44, 0x0f, 0x59, 0x2d, 0x59, 0x25, 0x0b, 0x00],
            [0xf3, 0x44, 0x0f, 0x59, 0x35, 0x4b, 0x25, 0x0b, 0x00],
            [0xf3, 0x0f, 0x59, 0x35, 0x6c, 0x23, 0x0b, 0x00, 0x00],
            [0xf3, 0x44, 0x0f, 0x59, 0x35, 0x5e, 0x23, 0x0b, 0x00],
        ];
        for (site, expected) in SITES.iter().zip(EXPECTED.iter()) {
            let mut actual = site.opcode.to_vec();
            actual.extend_from_slice(&site.vanilla_disp.to_le_bytes());
            assert_eq!(
                &actual[..],
                &expected[..site.len()],
                "{:#x}",
                site.address
            );
        }
    }

    #[test]
    fn slot_mapping_is_one_and_six() {
        let count = |slot: Slot| SITES.iter().filter(|site| site.slot == slot).count();
        assert_eq!(count(Slot::Overheat), 1);
        assert_eq!(count(Slot::RocketAcceleration), 6);
    }

    #[test]
    fn a_region_start_that_is_not_granularity_aligned_rounds_up() {
        // The image ends on a 4 KiB page boundary, not on a 64 KiB allocation-granularity boundary.
        // The first allocator asked for exactly that address, Windows rounded it down into the image
        // and every reservation failed — which is why the scalar knobs did nothing in play.
        assert_eq!(
            page_candidate(0x1408b4000, 0x1000_0000, 0x1408b4000),
            Some(0x1408c0000)
        );
    }

    #[test]
    fn a_page_must_fit_inside_the_region() {
        assert_eq!(
            page_candidate(0x140000000, 0x2000, 0x140000000),
            Some(0x140000000)
        );
        // A 0x1000 region holds exactly one page; anything smaller cannot.
        assert_eq!(
            page_candidate(0x140000000, 0x1000, 0x140000000),
            Some(0x140000000)
        );
        assert_eq!(page_candidate(0x140000000, 0x800, 0x140000000), None);
        // The aligned candidate is past the end of this small region.
        assert_eq!(page_candidate(0x140001000, 0x2000, 0x140001000), None);
    }

    #[test]
    fn disp_for_reproduces_the_vanilla_displacements() {
        for site in &SITES {
            assert_eq!(
                disp_for(site.address, site.len(), POOL),
                Some(site.vanilla_disp),
                "{:#x}",
                site.address
            );
        }
    }

    #[test]
    fn hook_trampolines_build_and_reach_their_resume_points() {
        let address = IMAGE_BASE + 0x100_0000;
        let resume_of = |code: &[u8]| {
            assert_eq!(code[code.len() - 5], 0xe9, "the tail is a near jump");
            let displacement =
                i32::from_le_bytes(code[code.len() - 4..].try_into().unwrap()) as i64;
            (address + code.len()) as i64 + displacement
        };

        let engine = engine_limits::trampoline().build(address).expect("builds");
        assert_eq!(resume_of(&engine), engine_limits::RESUME as i64);

        for (bytes, body_move, resume) in [
            (
                &rocket_ammo::LAUNCH_RBX_BYTES[..],
                rocket_ammo::body_in_rbx(),
                rocket_ammo::LAUNCH_RBX_RESUME,
            ),
            (
                &rocket_ammo::LAUNCH_RSI_BYTES[..],
                rocket_ammo::body_in_rsi(),
                rocket_ammo::LAUNCH_RSI_RESUME,
            ),
        ] {
            let code = rocket_ammo::launch_trampoline(bytes, body_move, resume)
                .build(address)
                .expect("builds");
            assert_eq!(resume_of(&code), resume as i64);
            assert!(
                code.windows(3).any(|window| window == body_move),
                "the body move is emitted"
            );
            assert!(
                code.windows(6).any(|window| window == bytes),
                "the original store is re-executed"
            );
        }

        for (result, resume) in [
            (rocket_ammo::burn_result_130(), rocket_ammo::BURN_130_RESUME),
            (rocket_ammo::burn_result_132(), rocket_ammo::BURN_132_RESUME),
        ] {
            let code = rocket_ammo::burn_trampoline(result, resume)
                .build(address)
                .expect("builds");
            assert_eq!(resume_of(&code), resume as i64);
            assert!(
                code.windows(result.len()).any(|window| window == result),
                "the result move is emitted"
            );
        }
    }

    #[test]
    fn hook_register_moves_are_the_expected_bytes() {
        // MOV RCX,RBX / MOV RCX,RSI
        assert_eq!(rocket_ammo::body_in_rbx(), &[0x48, 0x89, 0xd9][..]);
        assert_eq!(rocket_ammo::body_in_rsi(), &[0x48, 0x89, 0xf1][..]);
        // MOVAPS XMM7,XMM0 / MOVAPS XMM9,XMM0
        assert_eq!(rocket_ammo::burn_result_130(), &[0x0f, 0x28, 0xf8][..]);
        assert_eq!(rocket_ammo::burn_result_132(), &[0x44, 0x0f, 0x28, 0xc8][..]);
    }

    #[test]
    fn only_the_two_thrust_sites_are_hooked_for_per_ammo() {
        assert_eq!(
            SITES
                .iter()
                .filter(|site| rocket_ammo::is_burn_site(site.address))
                .count(),
            2
        );
        for site in SITES.iter().filter(|site| site.slot == Slot::RocketAcceleration) {
            let thrust = site.address == rocket_ammo::BURN_130
                || site.address == rocket_ammo::BURN_132;
            // The four preview sites keep the scalar value patch; only the two thrust sites are
            // replaced by the per-ammo hook.
            assert_eq!(rocket_ammo::is_burn_site(site.address), thrust, "{:#x}", site.address);
        }
    }

    #[test]
    fn disp_for_stays_inside_the_signed_range() {
        let site = &SITES[0];
        assert_eq!(disp_for(site.address, site.len(), site.address + site.len()), Some(0));
        assert!(disp_for(site.address, site.len(), POOL + 0x7000_0000).is_some());
        assert!(disp_for(site.address, site.len(), POOL + 0x9000_0000).is_none());
        assert!(disp_for(site.address, site.len(), 0x1000).is_none());
    }

    #[test]
    fn displacement_of_a_nearby_page_is_in_range_for_every_site() {
        // The allocator caps the page at ±1.25 GiB from the image base; every site is inside the
        // first 3 MB of the image, so the worst-case distance must still fit a disp32.
        const SPAN: usize = 0x5000_0000;
        for site in &SITES {
            for target in [IMAGE_BASE + SPAN, IMAGE_BASE - SPAN] {
                assert!(
                    disp_for(site.address, site.len(), target).is_some(),
                    "{:#x} cannot reach {target:#x}",
                    site.address
                );
            }
        }
    }
}

