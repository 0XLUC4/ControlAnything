//! Corsair DRAM RGB test. Without `--yes-write`: identification only (Read Byte Data of
//! registers 0x43/0x44 on 0x58..=0x5F and 0x18..=0x1F, nothing written).
//! With `--yes-write`: opens each identified DIMM (device info read), then red 2 s, green 2 s,
//! blue 2 s, gradient 3 s, in volatile direct mode. The last colors stay afterwards: no
//! volatile way back to the firmware effect exists (see `corsair_dram.rs`).
//! Usage (admin console):
//!   cargo run -p hub-backend-windows --example corsair_dram_test -- assets\pawnio\SmbusI801.bin
//!   cargo run -p hub-backend-windows --example corsair_dram_test -- assets\pawnio\SmbusI801.bin --yes-write

#[cfg(windows)]
fn main() -> Result<(), hub_core::HwError> {
    use std::sync::Arc;
    use std::time::Duration;

    use hub_backend_windows::{CorsairDram, SmbusI801};
    use hub_core::rgb::{Rgb, RgbDevice};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let yes_write = args.iter().any(|arg| arg == "--yes-write");
    let module = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .map_or(r"assets\pawnio\SmbusI801.bin", String::as_str);
    let bus = match SmbusI801::open(module.as_ref()) {
        Err(hub_core::HwError::PermissionDenied(_)) => {
            eprintln!("Access denied: run this example from an administrator console.");
            std::process::exit(1);
        }
        other => Arc::new(other?),
    };

    let mut identified = Vec::new();
    for address in (0x58..=0x5Fu8).chain(0x18..=0x1F) {
        match bus.identify_corsair(address) {
            Ok((ids, valid)) => {
                let verdict = if valid { "Corsair DRAM controller" } else { "not Corsair" };
                println!("{address:#04x}: reg 0x43 = {:#04x}, reg 0x44 = {:#04x} -> {verdict}", ids[0], ids[1]);
                if valid {
                    identified.push(address);
                }
            }
            Err(err) => println!("{address:#04x}: no answer ({err})"),
        }
    }
    if !yes_write {
        println!("\nRead-only run. Pass --yes-write to test the colors on {} DIMM(s).", identified.len());
        return Ok(());
    }

    let mut dimms = Vec::new();
    for address in identified {
        match CorsairDram::open(Arc::clone(&bus), address) {
            Ok(dimm) => {
                println!("opened {} ({} LEDs)", dimm.info().label, dimm.info().zones[0].leds);
                dimms.push(dimm);
            }
            Err(err) => println!("{address:#04x}: cannot open: {err}"),
        }
    }

    let mut show = |name: &str, color_at: &dyn Fn(usize, usize) -> Rgb, secs: u64| {
        println!("{name}");
        for dimm in &mut dimms {
            let leds = dimm.info().zones[0].leds;
            let frame = vec![(0..leds).map(|led| color_at(led, leds)).collect()];
            if let Err(err) = dimm.set_frame(&frame) {
                println!("  {}: {err}", dimm.info().id);
            }
        }
        std::thread::sleep(Duration::from_secs(secs));
    };
    show("red", &|_, _| Rgb::new(255, 0, 0), 2);
    show("green", &|_, _| Rgb::new(0, 255, 0), 2);
    show("blue", &|_, _| Rgb::new(0, 0, 255), 2);
    show(
        "gradient",
        &|led, leds| {
            let blue = (led * 255 / (leds - 1).max(1)) as u8;
            Rgb::new(255 - blue, 0, blue)
        },
        3,
    );

    for dimm in &mut dimms {
        dimm.restore();
    }
    println!("done: the gradient stays (no volatile way back to the firmware effect).");
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
