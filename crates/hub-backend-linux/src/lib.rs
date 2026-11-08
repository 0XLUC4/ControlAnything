//! Backend Linux : lecture des capteurs via sysfs hwmon (`/sys/class/hwmon`).
//! Pur std (pas de cfg Linux) pour que les tests tournent aussi sous Windows.

use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

mod fans;
pub use fans::HwmonFans;

const HWMON_ROOT: &str = "/sys/class/hwmon";

/// Un fichier d'entrée hwmon découvert au démarrage.
#[derive(Debug)]
struct Sensor {
    id: String,
    label: String,
    device: Device,
    kind: SensorKind,
    path: PathBuf,
    /// Facteur vers l'unité de `kind` (m°C -> °C, µW -> W).
    scale: f32,
}

/// Fournisseur de capteurs basé sur hwmon.
#[derive(Debug)]
pub struct HwmonProvider {
    sensors: Vec<Sensor>,
}

impl HwmonProvider {
    /// Scanne `/sys/class/hwmon`.
    pub fn discover() -> Result<Self, HwError> {
        Self::discover_in(Path::new(HWMON_ROOT))
    }

    /// Idem, à partir d'un répertoire arbitraire (utilisé par les tests).
    pub fn discover_in(root: &Path) -> Result<Self, HwError> {
        let mut sensors = Vec::new();
        for (dir, name, chip) in list_chips(root)? {
            discover_chip(&dir, &name, &chip, &mut sensors)?;
        }
        Ok(Self { sensors })
    }
}

/// Liste les chips hwmon : (dossier, nom brut, id unique suffixé `#N` si doublon).
/// Partagé avec `HwmonFans` pour que les ids de canaux collent aux ids de capteurs.
fn list_chips(root: &Path) -> Result<Vec<(PathBuf, String, String)>, HwError> {
    if !root.is_dir() {
        return Err(HwError::Unavailable(format!(
            "{} introuvable",
            root.display()
        )));
    }
    let mut chip_dirs: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect();
    // Tri naturel (hwmon2 avant hwmon10) pour un suffixage #N déterministe.
    chip_dirs.sort_by_key(|path| (path.as_os_str().len(), path.clone()));

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut chips = Vec::new();
    for dir in chip_dirs {
        let Some(name) = read_trimmed(&dir.join("name")) else {
            continue;
        };
        let count = seen.entry(name.clone()).or_insert(0);
        *count += 1;
        let chip = if *count == 1 {
            name.clone()
        } else {
            format!("{name}#{count}")
        };
        chips.push((dir, name, chip));
    }
    Ok(chips)
}

impl SensorProvider for HwmonProvider {
    fn name(&self) -> &str {
        "hwmon"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        // Un fichier illisible (capteur débranché, driver déchargé) est simplement ignoré.
        out.extend(self.sensors.iter().filter_map(|sensor| {
            read_value(&sensor.path).map(|raw| Reading {
                id: sensor.id.clone(),
                label: sensor.label.clone(),
                device: sensor.device,
                kind: sensor.kind,
                value: raw * sensor.scale,
            })
        }));
        Ok(())
    }
}

/// Ajoute les entrées lisibles d'un chip hwmon.
fn discover_chip(
    dir: &Path,
    name: &str,
    chip: &str,
    sensors: &mut Vec<Sensor>,
) -> Result<(), HwError> {
    let mut files: Vec<String> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    files.sort();
    let has_power_average = files.iter().any(|f| f == "power1_average");

    for file in &files {
        let Some((stem, kind, scale)) = classify(file) else {
            continue;
        };
        // power1_average prime sur power1_input quand les deux existent.
        if file == "power1_input" && has_power_average {
            continue;
        }
        let path = dir.join(file);
        if read_value(&path).is_none() {
            continue;
        }
        let label = read_trimmed(&dir.join(format!("{stem}_label")))
            .unwrap_or_else(|| format!("{chip} {stem}"));
        sensors.push(Sensor {
            id: format!("{chip}/{stem}"),
            label,
            device: device_for(name),
            kind,
            path,
            scale,
        });
    }
    Ok(())
}

/// Reconnaît un fichier d'entrée : renvoie (radical, nature, facteur d'échelle).
fn classify(file: &str) -> Option<(&str, SensorKind, f32)> {
    if file == "power1_average" || file == "power1_input" {
        return Some(("power1", SensorKind::Power, 1e-6));
    }
    let stem = file.strip_suffix("_input")?;
    let (kind, scale, digits) = if let Some(d) = stem.strip_prefix("temp") {
        (SensorKind::Temperature, 1e-3, d)
    } else {
        (SensorKind::Fan, 1.0, stem.strip_prefix("fan")?)
    };
    let numbered = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    numbered.then_some((stem, kind, scale))
}

/// Associe un nom de chip hwmon à un composant.
fn device_for(name: &str) -> Device {
    match name {
        "coretemp" | "k10temp" | "zenpower" => Device::Cpu,
        "amdgpu" | "nouveau" => Device::Gpu,
        "nvme" | "drivetemp" => Device::Storage,
        _ => Device::Motherboard,
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn read_value(path: &Path) -> Option<f32> {
    read_trimmed(path)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Faux arbre sysfs dans le dossier temporaire, supprimé au drop.
    struct FakeSysfs(PathBuf);

    impl FakeSysfs {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("hub-hwmon-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create root");
            Self(root)
        }

        fn file(&self, rel: &str, content: &str) -> &Self {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, content).expect("write");
            self
        }

        fn poll(&self) -> Vec<Reading> {
            let mut provider = HwmonProvider::discover_in(&self.0).expect("discover");
            let mut out = Vec::new();
            provider.poll(&mut out).expect("poll");
            out
        }
    }

    impl Drop for FakeSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn find<'a>(readings: &'a [Reading], id: &str) -> &'a Reading {
        readings
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id} absent: {readings:?}"))
    }

    #[test]
    fn maps_chip_names_to_devices() {
        assert_eq!(device_for("coretemp"), Device::Cpu);
        assert_eq!(device_for("k10temp"), Device::Cpu);
        assert_eq!(device_for("zenpower"), Device::Cpu);
        assert_eq!(device_for("amdgpu"), Device::Gpu);
        assert_eq!(device_for("nouveau"), Device::Gpu);
        assert_eq!(device_for("nvme"), Device::Storage);
        assert_eq!(device_for("drivetemp"), Device::Storage);
        assert_eq!(device_for("nct6687"), Device::Motherboard);
        assert_eq!(device_for("acpitz"), Device::Motherboard);
    }

    #[test]
    fn converts_units_and_keeps_stopped_fans() {
        let sysfs = FakeSysfs::new("units");
        sysfs
            .file("hwmon0/name", "k10temp\n")
            .file("hwmon0/temp1_input", "45500\n")
            .file("hwmon1/name", "amdgpu\n")
            .file("hwmon1/power1_average", "35000000\n")
            .file("hwmon1/power1_input", "1\n")
            .file("hwmon1/fan1_input", "0\n");
        let readings = sysfs.poll();

        let temp = find(&readings, "k10temp/temp1");
        assert_eq!(
            (temp.kind, temp.device),
            (SensorKind::Temperature, Device::Cpu)
        );
        assert!((temp.value - 45.5).abs() < 1e-4);
        let power = find(&readings, "amdgpu/power1");
        assert_eq!(power.kind, SensorKind::Power);
        assert!((power.value - 35.0).abs() < 1e-4);
        assert_eq!(find(&readings, "amdgpu/fan1").value, 0.0);
        assert_eq!(readings.len(), 3);
    }

    #[test]
    fn uses_label_file_or_falls_back() {
        let sysfs = FakeSysfs::new("labels");
        sysfs
            .file("hwmon0/name", "nct6687")
            .file("hwmon0/fan1_input", "1200")
            .file("hwmon0/fan1_label", "CPU Fan\n")
            .file("hwmon0/fan2_input", "800");
        let readings = sysfs.poll();
        assert_eq!(find(&readings, "nct6687/fan1").label, "CPU Fan");
        assert_eq!(find(&readings, "nct6687/fan2").label, "nct6687 fan2");
    }

    #[test]
    fn suffixes_duplicate_chip_names() {
        let sysfs = FakeSysfs::new("dupes");
        for dir in ["hwmon2", "hwmon10", "hwmon5"] {
            sysfs
                .file(&format!("{dir}/name"), "nvme")
                .file(&format!("{dir}/temp1_input"), "30000");
        }
        let mut ids: Vec<String> = sysfs.poll().into_iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, ["nvme#2/temp1", "nvme#3/temp1", "nvme/temp1"]);
    }

    #[test]
    fn skips_unreadable_files() {
        let sysfs = FakeSysfs::new("unreadable");
        sysfs
            .file("hwmon0/name", "coretemp")
            .file("hwmon0/temp1_input", "50000")
            .file("hwmon0/temp2_input", "garbage")
            .file("hwmon0/temp3_input", "60000");
        let mut provider = HwmonProvider::discover_in(&sysfs.0).expect("discover");
        // Fichier disparu après la découverte : ignoré au poll, sans erreur.
        fs::remove_file(sysfs.0.join("hwmon0/temp3_input")).expect("remove");
        let mut out = Vec::new();
        provider.poll(&mut out).expect("poll");
        let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["coretemp/temp1"]);
    }

    #[test]
    fn missing_root_is_unavailable() {
        let missing = std::env::temp_dir().join("hub-hwmon-does-not-exist");
        assert!(matches!(
            HwmonProvider::discover_in(&missing),
            Err(HwError::Unavailable(_))
        ));
    }
}
