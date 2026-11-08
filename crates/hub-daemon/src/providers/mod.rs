//! Builds the sensor sources and fan controllers of this machine.

mod nvidia;
mod system;

use hub_core::fan::FanController;
use hub_core::{HwError, SensorProvider};

pub type Sensors = Vec<Box<dyn SensorProvider>>;
pub type Fans = Vec<Box<dyn FanController>>;

/// Opens each backend; a failure is logged and ignored (the hardware may be missing).
pub fn build_all() -> (Sensors, Fans) {
    let mut sensors: Sensors = Vec::new();
    let mut fans: Fans = Vec::new();
    push_sensor(&mut sensors, "sysinfo", system::SysinfoProvider::new());
    push_sensor(&mut sensors, "nvidia", nvidia::NvidiaProvider::open());
    push_fans(&mut fans, "nvidia", nvidia::NvidiaFans::open());
    #[cfg(windows)]
    {
        let dir = pawnio_dir();
        push_sensor(
            &mut sensors,
            "intel-msr",
            hub_backend_windows::IntelCpuTemp::open(&dir.join("IntelMSR.bin")),
        );
        match hub_backend_windows::open_nct6687(&dir.join("LpcIO.bin")) {
            Ok((nct_sensors, nct_fans)) => {
                sensors.push(Box::new(nct_sensors));
                fans.push(Box::new(nct_fans));
            }
            Err(err) => tracing::warn!("nct6687 unavailable: {err}"),
        }
    }
    #[cfg(target_os = "linux")]
    {
        push_sensor(
            &mut sensors,
            "hwmon",
            hub_backend_linux::HwmonProvider::discover(),
        );
        push_fans(&mut fans, "hwmon", hub_backend_linux::HwmonFans::discover());
    }
    (sensors, fans)
}

fn push_sensor<P: SensorProvider + 'static>(
    sensors: &mut Sensors,
    name: &str,
    result: Result<P, HwError>,
) {
    match result {
        Ok(provider) => sensors.push(Box::new(provider)),
        Err(err) => tracing::warn!("provider {name} unavailable: {err}"),
    }
}

fn push_fans<C: FanController + 'static>(fans: &mut Fans, name: &str, result: Result<C, HwError>) {
    match result {
        Ok(controller) => fans.push(Box::new(controller)),
        Err(err) => tracing::warn!("{name} fans unavailable: {err}"),
    }
}

/// PawnIO modules shipped next to the executable (`<exe dir>/pawnio/`); in debug, falls back to
/// the repo `assets/pawnio/` so that `cargo run` works.
#[cfg(windows)]
fn pawnio_dir() -> std::path::PathBuf {
    let shipped = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("pawnio")))
        .unwrap_or_else(|| "pawnio".into());
    if cfg!(debug_assertions) && !shipped.is_dir() {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/pawnio")
    } else {
        shipped
    }
}

/// Converts bytes to MiB.
pub fn bytes_to_mib(bytes: u64) -> f32 {
    (bytes as f64 / (1024.0 * 1024.0)) as f32
}

#[cfg(test)]
mod tests {
    #[test]
    fn bytes_to_mib_converts() {
        assert_eq!(super::bytes_to_mib(3 * 1024 * 1024), 3.0);
        assert_eq!(super::bytes_to_mib(0), 0.0);
    }
}
