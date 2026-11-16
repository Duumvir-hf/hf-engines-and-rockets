//! `Modloader/config/tuning.json` — the decoupled knobs that all used to read one shared `500.0f`.
//!
//! Every number is taken as written (the mod clamps nothing); only a non-finite value falls back to
//! its vanilla default. `500.0` is what the Steam 1.163 build uses for all three scalars, so a file
//! full of `500.0` with empty per-`m_oid` maps leaves the game exactly as it shipped.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub(crate) const PATH: &str = "Modloader/config/tuning.json";

/// The vanilla value of the pooled `500.0f` every scalar knob replaces.
pub(crate) const VANILLA: f32 = 500.0;

/// The vanilla rocket burn time (Body `m_logic_f2`, `+0x3d4`); a different literal, not the pool.
pub(crate) const BURN_VANILLA: f32 = 1.5;

/// True when the burn-time knobs ask for anything other than vanilla.
pub(crate) fn burn_is_tuned(config: &Config) -> bool {
    config.rocket_burn_seconds != BURN_VANILLA || !config.rocket_burn_seconds_by_ammo.is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub enabled: bool,
    pub verbose_logging: bool,
    /// `Engine_ApplyThrustForce`: a part whose temperature `+0x2ec` is at or above this trips the
    /// 3 s engine lockout (`+0x810 = 3.0`) instead of producing thrust. Engines listed in
    /// `engine_overheat_by_oid` override it.
    pub engine_overheat_threshold: f32,
    /// Per-engine override of `engine_overheat_threshold`, keyed by the part's `m_oid`
    /// (`Body +0x190`, e.g. `MDL_ENGINE_01`). Case-insensitive.
    pub engine_overheat_by_oid: HashMap<String, f32>,
    /// `creatureLogic`: rocket burn acceleration in game units/s², applied as `mass * this` for
    /// 1.5 s by logic ids 130 and 132. Ammo listed in `rocket_acceleration_by_ammo` overrides it.
    /// The rocket trajectory preview in the HUD follows it.
    pub rocket_acceleration: f32,
    /// Per-ammo override of `rocket_acceleration`, keyed by the Ammo record's `item_name`
    /// (`Ammo +0x8`, e.g. `ITEM_AMMO_220MM`). Case-insensitive.
    pub rocket_acceleration_by_ammo: HashMap<String, f32>,
    /// Rocket burn time in seconds (Body `m_logic_f2`, `+0x3d4`): how long the `mass * accel` thrust
    /// is applied. `1.5` is vanilla. Ammo listed in `rocket_burn_seconds_by_ammo` overrides it.
    pub rocket_burn_seconds: f32,
    /// Per-ammo override of `rocket_burn_seconds`, same keys as `rocket_acceleration_by_ammo`.
    pub rocket_burn_seconds_by_ammo: HashMap<String, f32>,
    /// Keys the mod does not know (a knob that was removed, or a typo). Kept only so `init` can name
    /// them in the log instead of ignoring them silently.
    #[serde(flatten)]
    pub unknown: HashMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            verbose_logging: true,
            engine_overheat_threshold: VANILLA,
            engine_overheat_by_oid: HashMap::new(),
            rocket_acceleration: VANILLA,
            rocket_acceleration_by_ammo: HashMap::new(),
            rocket_burn_seconds: BURN_VANILLA,
            rocket_burn_seconds_by_ammo: HashMap::new(),
            unknown: HashMap::new(),
        }
    }
}

impl Config {
    pub(crate) fn sanitised(self) -> Self {
        Self {
            enabled: self.enabled,
            verbose_logging: self.verbose_logging,
            engine_overheat_threshold: finite_or(self.engine_overheat_threshold),
            engine_overheat_by_oid: finite_entries(self.engine_overheat_by_oid),
            rocket_acceleration: finite_or(self.rocket_acceleration),
            rocket_acceleration_by_ammo: finite_entries(self.rocket_acceleration_by_ammo),
            rocket_burn_seconds: finite_or_burn(self.rocket_burn_seconds),
            rocket_burn_seconds_by_ammo: finite_entries(self.rocket_burn_seconds_by_ammo),
            unknown: self.unknown,
        }
    }
}

/// A value from a per-`m_oid` / per-ammo map: exact key first, then case-insensitively.
pub(crate) fn lookup(map: &HashMap<String, f32>, name: &str) -> Option<f32> {
    if let Some(value) = map.get(name) {
        return Some(*value);
    }
    map.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

/// True when a knob differs from the vanilla pool value, i.e. its sites need patching.
pub(crate) fn is_tuned(value: f32) -> bool {
    value != VANILLA
}

fn finite_entries(map: HashMap<String, f32>) -> HashMap<String, f32> {
    map.into_iter()
        .filter(|(key, value)| !key.trim().is_empty() && value.is_finite())
        .map(|(key, value)| (key.trim().to_owned(), value))
        .collect()
}

fn finite_or(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        VANILLA
    }
}

fn finite_or_burn(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        BURN_VANILLA
    }
}

pub(crate) fn load() -> Config {
    match std::fs::read_to_string(PATH) {
        Ok(text) => {
            let text = text.trim_start_matches('\u{feff}');
            serde_json::from_str(text).unwrap_or_else(|error| {
                log::error!("hf_tuning: bad {PATH}: {error}; using defaults");
                Config::default()
            })
        }
        Err(_) => {
            let config = Config::default();
            if let Ok(text) = serde_json::to_string_pretty(&config) {
                let _ = std::fs::write(PATH, text);
            }
            config
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_vanilla() {
        let config = Config::default();
        assert_eq!(config.engine_overheat_threshold, VANILLA);
        assert_eq!(config.rocket_acceleration, VANILLA);
        assert_eq!(config.rocket_burn_seconds, BURN_VANILLA);
        assert!(config.engine_overheat_by_oid.is_empty());
        assert!(config.rocket_acceleration_by_ammo.is_empty());
        assert!(config.rocket_burn_seconds_by_ammo.is_empty());
        assert!(!burn_is_tuned(&config));
        assert!(config.enabled);
    }

    #[test]
    fn burn_knobs_are_tuned_only_when_they_change_something() {
        let scalar = Config {
            rocket_burn_seconds: 3.0,
            ..Config::default()
        };
        assert!(burn_is_tuned(&scalar));
        let per_ammo = Config {
            rocket_burn_seconds_by_ammo: HashMap::from([("ITEM_NAR122".to_owned(), 0.5)]),
            ..Config::default()
        };
        assert!(burn_is_tuned(&per_ammo));
        assert!(!burn_is_tuned(&Config::default()));
    }

    #[test]
    fn partial_json_keeps_the_other_defaults() {
        let config: Config = serde_json::from_str(
            r#"{ "rocket_acceleration": 900.0,
                 "rocket_acceleration_by_ammo": { "ITEM_AMMO_220MM": 700.0 } }"#,
        )
        .expect("parses");
        assert_eq!(config.rocket_acceleration, 900.0);
        assert_eq!(
            lookup(&config.rocket_acceleration_by_ammo, "ITEM_AMMO_220MM"),
            Some(700.0)
        );
        assert_eq!(config.engine_overheat_threshold, VANILLA);
        assert!(config.enabled);
    }

    #[test]
    fn per_oid_maps_round_trip_through_json() {
        let config = Config {
            engine_overheat_by_oid: HashMap::from([("MDL_ENGINE_01".to_owned(), 420.0)]),
            rocket_acceleration_by_ammo: HashMap::from([("ITEM_NAR122".to_owned(), 650.0)]),
            ..Config::default()
        };
        let text = serde_json::to_string(&config).expect("serializes");
        let parsed: Config = serde_json::from_str(&text).expect("parses");
        assert_eq!(
            lookup(&parsed.engine_overheat_by_oid, "MDL_ENGINE_01"),
            Some(420.0)
        );
        assert_eq!(
            lookup(&parsed.rocket_acceleration_by_ammo, "ITEM_NAR122"),
            Some(650.0)
        );
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let map = HashMap::from([("MDL_ENGINE_01".to_owned(), 420.0)]);
        assert_eq!(lookup(&map, "MDL_ENGINE_01"), Some(420.0));
        assert_eq!(lookup(&map, "mdl_engine_01"), Some(420.0));
        assert_eq!(lookup(&map, "MDL_ENGINE_04"), None);
    }

    #[test]
    fn non_finite_values_fall_back_and_are_dropped() {
        let config = Config {
            engine_overheat_threshold: f32::NAN,
            rocket_acceleration: f32::NEG_INFINITY,
            rocket_burn_seconds: f32::NAN,
            engine_overheat_by_oid: HashMap::from([
                ("MDL_ENGINE_01".to_owned(), 400.0),
                ("MDL_ENGINE_03".to_owned(), f32::NAN),
            ]),
            rocket_acceleration_by_ammo: HashMap::from([(" ".to_owned(), 700.0)]),
            rocket_burn_seconds_by_ammo: HashMap::from([("ITEM_NAR122".to_owned(), f32::INFINITY)]),
            ..Config::default()
        }
        .sanitised();
        assert_eq!(config.engine_overheat_threshold, VANILLA);
        assert_eq!(config.rocket_acceleration, VANILLA);
        assert_eq!(config.rocket_burn_seconds, BURN_VANILLA);
        assert_eq!(config.engine_overheat_by_oid.len(), 1);
        assert_eq!(
            lookup(&config.engine_overheat_by_oid, "MDL_ENGINE_01"),
            Some(400.0)
        );
        assert!(config.rocket_acceleration_by_ammo.is_empty());
        assert!(config.rocket_burn_seconds_by_ammo.is_empty());
    }

    #[test]
    fn shipped_config_parses() {
        let config: Config =
            serde_json::from_str(include_str!("../release/Modloader/config/tuning.json"))
                .expect("the shipped config parses");
        assert_eq!(config.engine_overheat_threshold, VANILLA);
        assert_eq!(config.rocket_acceleration, VANILLA);
        assert_eq!(config.rocket_burn_seconds, BURN_VANILLA);
        assert!(config.engine_overheat_by_oid.is_empty());
        assert!(config.rocket_acceleration_by_ammo.is_empty());
        assert!(config.rocket_burn_seconds_by_ammo.is_empty());
        assert!(config.unknown.is_empty());
    }

    #[test]
    fn zero_and_negative_are_taken_as_written() {
        let config = Config {
            engine_overheat_threshold: 0.0,
            rocket_acceleration: -500.0,
            engine_overheat_by_oid: HashMap::from([("MDL_ENGINE_01".to_owned(), -1.0)]),
            ..Config::default()
        }
        .sanitised();
        assert_eq!(config.engine_overheat_threshold, 0.0);
        assert_eq!(config.rocket_acceleration, -500.0);
        assert_eq!(
            lookup(&config.engine_overheat_by_oid, "MDL_ENGINE_01"),
            Some(-1.0)
        );
    }
}
