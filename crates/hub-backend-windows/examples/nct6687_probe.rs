//! Read-only probe: detects the NCT6687D and prints 3 readings. Never calls set_duty.
//! Usage (admin console): cargo run -p hub-backend-windows --example nct6687_probe -- assets\pawnio\LpcIO.bin

#[cfg(windows)]
fn main() -> Result<(), hub_core::HwError> {
    use hub_core::SensorProvider;
    use hub_core::fan::FanController;

    let module = std::env::args().nth(1).unwrap_or_else(|| r"assets\pawnio\LpcIO.bin".into());
    let (mut sensors, fans) = hub_backend_windows::open_nct6687(module.as_ref())?;
    for channel in fans.channels() {
        println!("channel {} = {} (rpm: {:?})", channel.id, channel.label, channel.rpm_sensor);
    }
    for _ in 0..3 {
        let mut readings = Vec::new();
        sensors.poll(&mut readings)?;
        for r in &readings {
            println!("{:<16} {:<14} {:>8.1} {}", r.id, r.label, r.value, r.kind.unit());
        }
        println!("---");
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
