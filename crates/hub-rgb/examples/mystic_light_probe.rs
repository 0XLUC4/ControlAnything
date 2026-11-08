//! READ-ONLY probe of the Mystic Light controller: lists MSI HID interfaces, reads
//! feature report 0x52, prints it in hex and decodes the zones. Never writes.

use hidapi::HidApi;
use hub_rgb::mystic_light::{self, REPORT_ID, VID, ZONES};

fn main() {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("hidapi unavailable: {e}");
            std::process::exit(1);
        }
    };
    for info in api.device_list().filter(|info| info.vendor_id() == VID) {
        let pid = info.product_id();
        println!(
            "PID {pid:04x} interface {} usage_page 0x{:04x} usage 0x{:04x} allowed={} path={:?}",
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            mystic_light::is_allowed(VID, pid),
            info.path()
        );
        if !mystic_light::is_allowed(VID, pid) {
            continue;
        }
        let device = match api.open_path(info.path()) {
            Ok(device) => device,
            Err(e) => {
                println!("  cannot open: {e}");
                continue;
            }
        };
        let read = match mystic_light::read_report(&device) {
            Ok(read) => read,
            Err(e) => {
                println!("  cannot read report 0x{REPORT_ID:02x}: {e}");
                continue;
            }
        };
        println!("  report 0x{REPORT_ID:02x}: {} bytes read", read.len());
        for (row, chunk) in read.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
            println!("  {:03}: {}", row * 16, hex.join(" "));
        }
        match mystic_light::parse_report(&read) {
            Ok(report) => {
                for slot in &ZONES {
                    let (effect, color) = mystic_light::zone_state(&report, slot.offset);
                    let tail = &report[slot.offset + 4..slot.offset + 11];
                    println!(
                        "  {} (@{}) effect {effect} color {:02x}{:02x}{:02x} tail {tail:02x?}",
                        slot.label, slot.offset, color.r, color.g, color.b
                    );
                }
                let (effect, color) = mystic_light::zone_state(&report, 74);
                println!(
                    "  ON_BOARD (@74) effect {effect} color {:02x}{:02x}{:02x}",
                    color.r, color.g, color.b
                );
            }
            Err(e) => println!("  report rejected: {e}"),
        }
    }
}
