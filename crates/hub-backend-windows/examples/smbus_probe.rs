//! Read-only SMBus scan of the Intel PCH (i801) host: SMBus "Receive Byte" only on 0x08..=0x77.
//! Never writes on the bus (see `SmbusI801::receive_byte`). Same probe i2cdetect uses for
//! 0x30-0x37 and 0x50-0x5F (i2c-tools `i2cdetect.c`, MODE_AUTO), here applied to every address.
//! Usage (admin console): cargo run -p hub-backend-windows --example smbus_probe -- assets\pawnio\SmbusI801.bin

#[cfg(windows)]
fn main() -> Result<(), hub_core::HwError> {
    use std::collections::BTreeMap;

    let module = std::env::args().nth(1).unwrap_or_else(|| r"assets\pawnio\SmbusI801.bin".into());
    let bus = match hub_backend_windows::SmbusI801::open(module.as_ref()) {
        Err(hub_core::HwError::PermissionDenied(_)) => {
            eprintln!("Access denied: run this example from an administrator console.");
            std::process::exit(1);
        }
        other => other?,
    };
    let (io_base, pci_ids, subsys_ids) = bus.identity()?;
    println!("i801 SMBus: PCI {pci_ids:08x} (device:vendor), subsystem {subsys_ids:08x}, I/O base {io_base:#x}");

    let mut found = Vec::new();
    let mut errors: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for address in 0x08..=0x77u8 {
        match bus.receive_byte(address) {
            Ok(value) => found.push((address, value)),
            Err(err) => errors.entry(err.to_string()).or_default().push(address),
        }
    }

    println!("\n{} address(es) answered:", found.len());
    for (address, value) in &found {
        println!("  {address:#04x} -> {value:#04x}  {}", annotate(*address));
    }
    println!("\nNo answer (grouped by driver error):");
    for (message, addresses) in &errors {
        println!("  {} address(es): {message}", addresses.len());
    }
    Ok(())
}

/// Known ranges. DDR5 SPD hub / PMIC: JEDEC JESD300 (SPD5118) and JESD301 (PMIC5xxx), DIMM index
/// = low 3 bits. Corsair: OpenRGB `Controllers/CorsairDRAMController/CorsairDRAMControllerDetect.cpp`
/// tries 0x58..=0x5F then 0x18..=0x1F; `CorsairDRAMDevices.cpp` lists "Corsair Vengeance RGB DDR5".
#[cfg(windows)]
fn annotate(address: u8) -> &'static str {
    match address {
        0x50..=0x57 => "DDR5 SPD EEPROM / SPD hub",
        0x48..=0x4F => "DDR5 PMIC?",
        0x58..=0x5F | 0x18..=0x1F => "Corsair DRAM RGB controller range (OpenRGB)",
        _ => "",
    }
}

#[cfg(not(windows))]
fn main() {}
