//! OPT-IN write test: red, green, blue 2 s each on every LampArray device, then hands
//! the lamps back to the firmware (AutonomousMode). Requires `--yes-write`.

use std::thread::sleep;
use std::time::Duration;

use hub_core::rgb::Rgb;

fn main() {
    if !std::env::args().any(|arg| arg == "--yes-write") {
        eprintln!("LampArray write test: rerun with --yes-write to execute it.");
        std::process::exit(2);
    }
    let mut devices = hub_rgb::lamparray::detect();
    if devices.is_empty() {
        eprintln!("No usable HID LampArray device detected (run lamparray_probe).");
        std::process::exit(1);
    }
    for device in &mut devices {
        println!("{} ({}): {:?}", device.info().label, device.info().id, device.info().zones);
        for (name, color) in
            [("red", Rgb::new(255, 0, 0)), ("green", Rgb::new(0, 255, 0)), ("blue", Rgb::new(0, 0, 255))]
        {
            let frame: Vec<Vec<Rgb>> =
                device.info().zones.iter().map(|zone| vec![color; zone.leds]).collect();
            println!("  {name}");
            if let Err(e) = device.set_frame(&frame) {
                eprintln!("  write failed: {e}");
                break;
            }
            sleep(Duration::from_secs(2));
        }
        println!("  restoring (AutonomousMode)");
        device.restore();
    }
}
