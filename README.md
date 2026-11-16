# hf-engines-and-rockets

HighFleet (**Steam 1.163**) tuning mod. Two systems, each configurable globally and per part/ammo —
no scripting, no editor, one JSON file:

* **engine overheat** — the temperature at which an engine trips its 3-second lockout (heat, not
  damage), globally and per engine `m_oid`.
* **rockets** — burn **acceleration** and burn **duration**, globally and per ammo `item_name`.

## Install

1. [logdot's HighFleet Modloader](https://github.com/logdot/Highfleet-Modloader).
2. Copy `Modloader/mods/hf_tuning.dll` and `Modloader/config/tuning.json` into the game's `Modloader/`
   folder (the release zip already has that layout).
3. Edit `Modloader/config/tuning.json`, start the game. The mod refuses to start on any version other
   than Steam 1.163.

Prefer to build it yourself? `cargo build --release` and take `target/release/hf_tuning.dll`; it is
built against `highfleet-mod-api` v4.0.0 and `patchy-rs` v0.1.0.

## Config

`Modloader/config/tuning.json` — every number is taken as written (nothing is clamped):

```json
{
  "enabled": true,
  "verbose_logging": true,

  "engine_overheat_threshold": 500.0,
  "engine_overheat_by_oid": {},

  "rocket_acceleration": 500.0,
  "rocket_acceleration_by_ammo": {},

  "rocket_burn_seconds": 1.5,
  "rocket_burn_seconds_by_ammo": {}
}
```

| key | vanilla | what it does |
|---|---|---|
| `engine_overheat_threshold` | `500.0` | part temperature that trips the 3 s engine lockout |
| `engine_overheat_by_oid` | `{}` | per **engine** override, keyed by `m_oid` |
| `rocket_acceleration` | `500.0` | rocket burn thrust, game units/s², applied as `mass × this` |
| `rocket_acceleration_by_ammo` | `{}` | per **ammo** override, keyed by the ammo `item_name` |
| `rocket_burn_seconds` | `1.5` | how long the thrust is applied |
| `rocket_burn_seconds_by_ammo` | `{}` | per **ammo** burn time override |

* Keys are matched exactly first, then case-insensitively.
* Precedence: `engine_overheat_by_oid[oid]` > `engine_overheat_threshold`;
  `rocket_acceleration_by_ammo[item]` > `rocket_acceleration`;
  `rocket_burn_seconds_by_ammo[item]` > `rocket_burn_seconds`. Anything not listed uses the scalar.
* Vanilla values + empty maps = the mod does nothing (it patches nothing and installs no hooks).
* `0` and negative numbers are allowed: a zero burn time means no thrust at all, and a negative
  acceleration pushes rockets backwards. Non-finite values fall back to vanilla.
* Unknown keys are named in `client.log` instead of being ignored silently.

### Examples

All engines survive longer, one engine (RD-59) even longer, and the 300 mm rocket is a slow burner:

```json
"engine_overheat_threshold": 700.0,
"engine_overheat_by_oid": { "MDL_ENGINE_01": 900.0 },
"rocket_burn_seconds_by_ammo": { "ITEM_AMMO_300MM": 4.0 }
```

A short, hard-hitting rocket (220 mm pod): more thrust, but for only 0.8 s:

```json
"rocket_acceleration_by_ammo": { "ITEM_AMMO_220MM": 1200.0 },
"rocket_burn_seconds_by_ammo": { "ITEM_AMMO_220MM": 0.8 }
```

### Where the keys come from

* Engine `m_oid` is the part id in `Libraries/OL.seria` / `parts.seria`. Vanilla engines:
  `MDL_ENGINE_01` (RD-59, swivels), `02` (RD-59S, static), `03` (D-30, swivels), `04` (NK-25,
  swivels), `05` (D-30S, static), `06` (no placeable prototype); `OL.seria` also holds the `_OLD`
  variants. Part ids from content mods work too (they only exist with that mod installed).
* Ammo key is the Ammo record's `item_name`: `ITEM_NAR122`, `ITEM_NAR340`, `ITEM_AMMO_220MM`,
  `ITEM_AMMO_300MM`, `ITEM_AMMO_300MM_INC`, … (rockets are the class-3 ammo records).
* Don't guess: the first time an engine or a rocket ammo is handled that has no override, the log
  prints its exact id, ready to copy into the JSON.

## How it works

HighFleet keeps one hardcoded `500.0f` and 82 instructions read it; the scalar knobs retarget the
few instructions that matter to a writable copy of the value (a 4-byte displacement rewrite, the
instruction itself is unchanged). The per-`m_oid` values cannot be a constant, so they use small
trampolines instead: the engine hook shifts the temperature the vanilla compare sees, and the rocket
hooks record the firing ammo's settings per rocket body at launch. Only the mod's own memory is
written — the game's constant pool, ammo table and body fields are read, never modified.

**The enemy pre-fire aim laser is deliberately not tunable** — the reticle stays exactly as shipped.

## Known limits

* The HUD's dashed rocket path preview assumes the vanilla `1.5 s` burn and the global acceleration,
  so a per-ammo override or a different burn makes the drawn curve longer/shorter than the real
  flight. The rocket itself behaves as configured.
* A 300 mm rocket launched as a ship part never passes the gun launch path, so it takes the scalar
  values, not the per-ammo ones.
* The engine override is a temperature threshold per `m_oid`, not a per-part (damage-scaled) curve.
* Values are read once at startup; edit the JSON and restart the game.

## Compatibility

Installs only through logdot's `patchy-rs`, which verifies the original bytes and refuses overlapping
patches, so a conflict with another patchy mod (e.g. `ammo_extended`, `qol` pack) fails with a log
error instead of corrupting memory. All hook sites are in code none of those mods touch.

## License

MIT — see [LICENSE](LICENSE).
