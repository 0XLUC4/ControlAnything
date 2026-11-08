//! Les configs livrées dans `configs/` doivent rester importables.

use hub_core::config::{Config, FanMode};

#[test]
fn shipped_profiles_parse_and_validate() {
    let text = include_str!("../../../configs/controlanything-profils.toml");
    let config = Config::from_toml(text).expect("config livrée invalide");
    assert_eq!(config.active_profile, "Silencieux");
    assert_eq!(config.profiles.len(), 4);
    assert_eq!(config.failsafe.cpu_critical, 98.0);
    let silent = config.active().expect("profil actif");
    assert!(matches!(
        silent.fans["nct6687/pwm2"].mode,
        FanMode::Fixed { duty: 60 }
    ));
    assert!(matches!(
        silent.fans["nct6687/pwm1"].mode,
        FanMode::Curve { .. }
    ));
}
