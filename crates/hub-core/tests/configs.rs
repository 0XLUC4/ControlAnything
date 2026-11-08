//! The configs shipped in `configs/` must stay importable.

use hub_core::config::{Config, FanMode, Preset};

/// "Silent" profile = the editor's "Silent" preset buttons, applied to each fan.
#[test]
fn shipped_curves_match_presets() {
    let text = include_str!("../../../configs/controlanything-profiles.toml");
    let config = Config::from_toml(text).expect("invalid shipped config");
    for (name, preset) in [
        ("Silent", Preset::Silent),
        ("Balanced", Preset::Balanced),
        ("Performance", Preset::Performance),
    ] {
        let profile = config.profiles.iter().find(|p| p.name == name).expect(name);
        for (id, fan) in &profile.fans {
            if matches!(fan.mode, FanMode::Curve { .. }) {
                assert!(preset.matches(fan), "{name} / {id} does not match the preset");
            }
        }
    }
}

#[test]
fn shipped_profiles_parse_and_validate() {
    let text = include_str!("../../../configs/controlanything-profiles.toml");
    let config = Config::from_toml(text).expect("invalid shipped config");
    assert_eq!(config.active_profile, "Silent");
    assert_eq!(config.profiles.len(), 4);
    assert_eq!(config.failsafe.cpu_critical, 98.0);
    let silent = config.active().expect("active profile");
    assert!(matches!(
        silent.fans["nct6687/pwm2"].mode,
        FanMode::Fixed { duty: 60 }
    ));
    assert!(matches!(
        silent.fans["nct6687/pwm1"].mode,
        FanMode::Curve { .. }
    ));
}
