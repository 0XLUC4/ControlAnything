//! Probe of HID LampArray devices (page 0x59): lists them, prints the descriptor
//! layout, the LampArrayAttributes and the attributes of the first lamps.
//! Lighting is never changed. Only write: LampAttributesRequest, which moves the
//! attributes cursor (pass --no-lamps to skip it and stay strictly read-only).

use hidapi::HidApi;
use hub_rgb::lamparray::{self, FeatureReport, usage};

/// Lamps whose attributes are printed.
const LAMPS_SHOWN: u16 = 4;

fn usage_name(usage_id: u16) -> &'static str {
    match usage_id {
        usage::LAMP_ID => "LampId",
        0x23 => "PositionX_um",
        0x24 => "PositionY_um",
        0x25 => "PositionZ_um",
        0x26 => "LampPurposes",
        0x27 => "UpdateLatency_us",
        0x28 => "RedLevels",
        0x29 => "GreenLevels",
        0x2A => "BlueLevels",
        0x2B => "IntensityLevels",
        0x2C => "IsProgrammable",
        0x2D => "InputBinding",
        _ => "?",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ")
}

fn describe(name: &str, report: Option<&FeatureReport>) {
    match report {
        Some(report) => println!(
            "  {name}: id 0x{:02x}, {} bytes, {} fields",
            report.id,
            report.buf_len(),
            report.fields.len()
        ),
        None => println!("  {name}: absent"),
    }
}

fn main() {
    let read_lamps = !std::env::args().any(|arg| arg == "--no-lamps");
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("hidapi unavailable: {e}");
            std::process::exit(1);
        }
    };
    let mut count = 0;
    for info in api.device_list().filter(|info| lamparray::is_lamp_array(info)) {
        count += 1;
        let (vid, pid) = (info.vendor_id(), info.product_id());
        println!(
            "{} \"{}\" dedicated_driver={} path={:?}",
            lamparray::device_id(info),
            info.product_string().unwrap_or("?"),
            hub_rgb::has_dedicated_driver(vid, pid),
            info.path()
        );
        let device = match api.open_path(info.path()) {
            Ok(device) => device,
            Err(e) => {
                println!("  cannot open: {e}");
                continue;
            }
        };
        let layout = match lamparray::read_layout(&device) {
            Ok(layout) => layout,
            Err(e) => {
                println!("  unusable descriptor: {e}");
                continue;
            }
        };
        describe("LampArrayAttributesReport", Some(&layout.attributes));
        describe("LampAttributesRequestReport", layout.attributes_request.as_ref());
        describe("LampAttributesResponseReport", layout.attributes_response.as_ref());
        describe("LampMultiUpdateReport", Some(&layout.multi_update));
        println!("    slots per report: {}", layout.multi_update.count(usage::LAMP_ID));
        describe("LampRangeUpdateReport", layout.range_update.as_ref());
        describe("LampArrayControlReport", Some(&layout.control));
        let attributes = match lamparray::get_feature(&device, &layout, &layout.attributes) {
            Ok(read) => {
                println!("  attributes raw: {}", hex(&read));
                lamparray::parse_attributes(&layout.attributes, &read)
            }
            Err(e) => Err(e),
        };
        let attributes = match attributes {
            Ok(attributes) => attributes,
            Err(e) => {
                println!("  attributes unreadable: {e}");
                continue;
            }
        };
        let [width, height, depth] = attributes.bounding_box_um;
        println!(
            "  LampCount {} box {width}x{height}x{depth} um kind {} MinUpdateInterval {} us",
            attributes.lamp_count, attributes.kind, attributes.min_update_interval_us
        );
        let Some(response) = layout.attributes_response.as_ref().filter(|_| read_lamps) else {
            continue;
        };
        for lamp_id in 0..attributes.lamp_count.min(LAMPS_SHOWN) {
            match lamparray::read_lamp_attributes(&device, &layout, lamp_id) {
                Ok(read) => {
                    let fields: Vec<String> = response
                        .fields
                        .iter()
                        .map(|field| {
                            let value = response.get(&read, field.usage).unwrap_or(0);
                            format!("{}={value}", usage_name(field.usage))
                        })
                        .collect();
                    println!("  lamp {lamp_id}: {}", fields.join(" "));
                }
                Err(e) => println!("  lamp {lamp_id}: unreadable: {e}"),
            }
        }
    }
    if count == 0 {
        println!("No HID LampArray interface (usage page 0x59, usage 0x01) found.");
    }
}
