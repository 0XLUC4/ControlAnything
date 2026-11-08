//! Test d'écriture OPT-IN : rouge 3 s, vert 3 s, bleu 3 s sur toutes les zones, puis
//! restauration de l'état d'origine. Exige `--yes-write`, sinon ne fait rien.

use std::thread::sleep;
use std::time::Duration;

use hub_core::rgb::Rgb;

fn main() {
    if !std::env::args().any(|arg| arg == "--yes-write") {
        eprintln!("Test d'écriture Mystic Light : relancer avec --yes-write pour l'exécuter.");
        std::process::exit(2);
    }
    let mut devices = hub_rgb::mystic_light::detect();
    if devices.is_empty() {
        eprintln!("Aucun contrôleur Mystic Light autorisé détecté.");
        std::process::exit(1);
    }
    for device in &mut devices {
        println!("{} : {:?}", device.info().label, device.info().zones);
        for (name, color) in
            [("rouge", Rgb::new(255, 0, 0)), ("vert", Rgb::new(0, 255, 0)), ("bleu", Rgb::new(0, 0, 255))]
        {
            let frame: Vec<Vec<Rgb>> =
                device.info().zones.iter().map(|zone| vec![color; zone.leds]).collect();
            println!("  {name}");
            if let Err(e) = device.set_frame(&frame) {
                eprintln!("  écriture échouée : {e}");
                break;
            }
            sleep(Duration::from_secs(3));
        }
        println!("  restauration");
        device.restore();
    }
}
