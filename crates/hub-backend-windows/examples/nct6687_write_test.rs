//! Premier test d'écriture, limité à System Fan 1 (jamais la pompe ni le CPU) :
//! 100 % pendant 6 s, 40 % pendant 6 s, puis retour au BIOS et 8 s d'observation.
//! Usage (console admin) : nct6687_write_test.exe assets\pawnio\LpcIO.bin

#[cfg(windows)]
fn main() -> Result<(), hub_core::HwError> {
    use hub_core::SensorProvider;
    use hub_core::fan::FanController;
    use std::time::Duration;

    const CHANNEL: &str = "nct6687/pwm3"; // System Fan 1
    const RPM: &str = "nct6687/fan3";

    let module = std::env::args().nth(1).unwrap_or_else(|| r"assets\pawnio\LpcIO.bin".into());
    let (mut sensors, mut fans) = hub_backend_windows::open_nct6687(module.as_ref())?;

    let mut show = |phase: &str, secs: u64| -> Result<(), hub_core::HwError> {
        for _ in 0..secs {
            std::thread::sleep(Duration::from_secs(1));
            let mut readings = Vec::new();
            sensors.poll(&mut readings)?;
            let rpm = readings.iter().find(|r| r.id == RPM).map_or(f32::NAN, |r| r.value);
            println!("{phase:<10} System Fan 1 = {rpm:>6.0} RPM");
        }
        Ok(())
    };

    show("BIOS", 3)?;
    fans.set_duty(CHANNEL, 100)?;
    show("100 %", 6)?;
    fans.set_duty(CHANNEL, 40)?;
    show("40 %", 6)?;
    fans.set_auto(CHANNEL)?;
    show("BIOS", 8)?;
    // `fans` est relâché ici : Drop → restore_all (double sécurité).
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
