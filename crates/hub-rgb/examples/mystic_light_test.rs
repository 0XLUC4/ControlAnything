//! OPT-IN write test: red 3 s, green 3 s, blue 3 s on every zone, then restores
//! the original state. Requires `--yes-write`, otherwise does nothing.

use std::thread::sleep;
use std::time::Duration;

use hub_core::rgb::Rgb;

fn main() {
    if !std::env::args().any(|arg| arg == "--yes-write") {
        eprintln!("Mystic Light write test: rerun with --yes-write to execute it.");
        std::process::exit(2);
    }
    let mut devices = hub_rgb::mystic_light::detect();
    if devices.is_empty() {
        eprintln!("No allowed Mystic Light controller detected.");
        std::process::exit(1);
    }
    for device in &mut devices {
        println!("{}: {:?}", device.info().label, device.info().zones);
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
            sleep(Duration::from_secs(3));
        }
        println!("  restoring");
        device.restore();
    }
}
