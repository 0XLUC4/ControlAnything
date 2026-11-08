//! Super I/O Nuvoton NCT6687D (cartes MSI Intel 600/700, AMD 500/600) via le module PawnIO LpcIO.
//!
//! Carte des registres dérivée de LibreHardwareMonitor (MPL-2.0) — ce fichier est sous MPL-2.0.
//! This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
//! If a copy of the MPL was not distributed with this file, You can obtain one at
//! http://mozilla.org/MPL/2.0/.
//!
//! Références (LibreHardwareMonitor master, nov. 2026, `LibreHardwareMonitorLib/`) :
//! - `Hardware/Motherboard/Lpc/LpcIO.cs` : détection (ID 0xD5/0x92 l.409-428, flux l.466-500,
//!   `IsInvalidRuntimeBase` l.755, constantes l.780-805) ;
//! - `Hardware/Motherboard/Lpc/LpcPort.cs` : clés d'entrée/sortie config (l.66-75) ;
//! - `Hardware/Motherboard/Lpc/Nct677X.cs` : espace EC (l.28-31, accès l.1259-1347),
//!   registres NCT6687D (l.101-108, 527-540, 573), décodage temp. (l.880-886) et RPM (l.1052-1066),
//!   pilotage (l.745-766), sauvegarde/restauration (l.1458-1540) ;
//! - `Hardware/Motherboard/SuperIOHardware.cs` : % -> octet (l.120-125), libellés MSI (l.607-660).
//!
//! Recoupé avec le pilote Linux github.com/Fred78290/nct6687d (`nct6687.c`, mapping « default »,
//! que ce pilote réserve explicitement aux NCT6687D non-DR, Z790 inclus).
//! NON TESTÉ sur matériel réel au moment de l'écriture.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::sleep;
use std::time::{Duration, Instant};

use hub_core::fan::{FanChannel, FanController};
use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::pawnio::PawnIo;

// --- Espace de configuration Super I/O (LpcIO.cs, LpcPort.cs) — confiance élevée ---
/// (slot PawnIO, port d'index) : slot 0 = 0x2E/0x2F, slot 1 = 0x4E/0x4F.
const SLOTS: [(u64, u16); 2] = [(0, 0x2E), (1, 0x4E)];
const ENTER_KEY: u8 = 0x87; // écrit deux fois (Winbond/Nuvoton/Fintek)
const EXIT_KEY: u8 = 0xAA;
const CHIP_ID_REGISTER: u8 = 0x20;
const CHIP_REVISION_REGISTER: u8 = 0x21;
const DEVICE_SELECT_REGISTER: u8 = 0x07;
const HWM_LOGICAL_DEVICE: u8 = 0x0B;
const BASE_ADDRESS_REGISTER: u8 = 0x60;
/// (ID, révision) de la NCT6687D (et de la NCT6687DR, distinguée par le nom de carte).
const NCT6687D_ID: (u8, u8) = (0xD5, 0x92);
/// Cartes MSI équipées de la variante DR, registres de pilotage différents (LpcIO.cs l.413-418).
const NCT6687DR_BOARDS: [&str; 5] = ["B840", "B850", "B860", "X870", "Z890"];

// --- Accès EC par page/index/donnée depuis la BAR (Nct677X.cs l.28-31, 1259-1347) — confiance élevée ---
const EC_PAGE_OFFSET: u16 = 0x04;
const EC_INDEX_OFFSET: u16 = 0x05;
const EC_DATA_OFFSET: u16 = 0x06;
/// Valeur du registre de page quand l'EC est libre ; réécrite après chaque accès.
const EC_PAGE_FREE: u8 = 0xFF;
const EC_ACCESS_TIMEOUT: Duration = Duration::from_millis(500);

// --- Registres EC de la NCT6687D (Nct677X.cs l.101-108, 527-540, 573) ---
/// Températures : entier signé en `0x100 + 2n`, demi-degré au bit 7 de l'octet suivant. Confiance élevée.
const TEMP_BASE: u16 = 0x100;
/// RPM : 16 bits big-endian en `0x140 + 2n` (mapping « default », 8 canaux). Confiance élevée.
const FAN_RPM_BASE: u16 = 0x140;
/// Mode partagé : bit n = canal n en manuel. Confiance moyenne (LHM + Linux concordent).
const FAN_CONTROL_MODE: u16 = 0xA00;
/// Requête de configuration : 0x80 = début, 0x40 = fin. Confiance moyenne.
const FAN_PWM_REQUEST: u16 = 0xA01;
const FAN_CFG_REQUEST: u8 = 0x80;
const FAN_CFG_COMPLETE: u8 = 0x40;
/// Consigne PWM manuelle (0..=255) du canal n en `0xA28 + n`. Confiance moyenne.
const FAN_PWM_COMMAND_BASE: u16 = 0xA28;
/// Pause après chaque requête de config, comme LHM (`Thread.Sleep(50)`).
const FAN_CFG_DELAY: Duration = Duration::from_millis(50);

/// Libellés MSI par défaut de LHM (SuperIOHardware.cs l.607-660) ; index n -> `temp{n+1}`.
const TEMP_LABELS: [&str; 7] = ["CPU", "System", "VRM MOS", "PCH", "CPU Socket", "PCIe x1", "M2_1"];
/// Libellés MSI par défaut de LHM ; index n -> `fan{n+1}` / `pwm{n+1}`.
const FAN_LABELS: [&str; 8] = [
    "CPU Fan",
    "Pump Fan",
    "System Fan 1",
    "System Fan 2",
    "System Fan 3",
    "System Fan 4",
    "System Fan 5",
    "System Fan 6",
];

/// Attente max du mutex inter-outils (LHM, HWiNFO, MSI Center…) en fonctionnement normal.
const ISA_TIMEOUT: Duration = Duration::from_millis(100);
/// En restauration on attend plus longtemps, puis on restaure quand même.
const ISA_RESTORE_TIMEOUT: Duration = Duration::from_millis(500);

/// Détecte la NCT6687D via le module PawnIO LpcIO et renvoie deux vues partageant la puce.
///
/// Lecture seule côté moniteur matériel : seules les écritures de protocole sont faites
/// (clés d'entrée/sortie du mode config, sélection du périphérique logique, registre de page EC).
pub fn open_nct6687(lpcio_module: &Path) -> Result<(Nct6687Sensors, Nct6687Fans), HwError> {
    let product = baseboard_product()?;
    if is_nct6687dr_board(&product) {
        return Err(HwError::Unavailable(format!("carte {product} : NCT6687DR non prise en charge")));
    }
    let mut chip = Chip { pawn: PawnIo::open(lpcio_module)?, base: 0, isa_mutex: create_isa_mutex()? };
    let base = {
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        chip.detect()?
    };
    chip.base = base;

    let chip = Arc::new(Mutex::new(chip));
    let channels = FAN_LABELS
        .iter()
        .enumerate()
        .map(|(index, label)| FanChannel {
            id: format!("nct6687/pwm{}", index + 1),
            label: (*label).into(),
            rpm_sensor: Some(format!("nct6687/fan{}", index + 1)),
        })
        .collect();
    let fans = Nct6687Fans { chip: Arc::clone(&chip), channels, saved: [None; FAN_LABELS.len()] };
    Ok((Nct6687Sensors { chip }, fans))
}

/// Températures et RPM de la NCT6687D.
pub struct Nct6687Sensors {
    chip: Arc<Mutex<Chip>>,
}

impl SensorProvider for Nct6687Sensors {
    fn name(&self) -> &str {
        "nct6687"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        for (index, label) in TEMP_LABELS.iter().enumerate() {
            let register = temp_register(index);
            let value = decode_temperature(chip.ec_read(register)?, chip.ec_read(register + 1)?);
            out.push(reading(format!("nct6687/temp{}", index + 1), label, SensorKind::Temperature, value));
        }
        for (index, label) in FAN_LABELS.iter().enumerate() {
            let register = fan_rpm_register(index);
            let rpm = u16::from_be_bytes([chip.ec_read(register)?, chip.ec_read(register + 1)?]);
            out.push(reading(format!("nct6687/fan{}", index + 1), label, SensorKind::Fan, f32::from(rpm)));
        }
        Ok(())
    }
}

/// État d'origine d'un canal, lu juste avant la première écriture.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SavedChannel {
    /// Bit du canal dans `FAN_CONTROL_MODE` (0 = auto firmware).
    mode_bit: u8,
    command: u8,
}

/// Pilotage PWM des 8 headers. Le Drop rend chaque canal modifié au firmware.
pub struct Nct6687Fans {
    chip: Arc<Mutex<Chip>>,
    channels: Vec<FanChannel>,
    saved: [Option<SavedChannel>; FAN_LABELS.len()],
}

impl Nct6687Fans {
    fn index_of(&self, channel: &str) -> Result<usize, HwError> {
        self.channels
            .iter()
            .position(|c| c.id == channel)
            .ok_or_else(|| HwError::Other(format!("canal inconnu : {channel}")))
    }
}

impl FanController for Nct6687Fans {
    fn name(&self) -> &str {
        "nct6687"
    }

    fn channels(&self) -> &[FanChannel] {
        &self.channels
    }

    /// Séquence LHM non-DR (Nct677X.cs l.745-766) : requête, bit manuel, consigne, fin.
    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError> {
        let index = self.index_of(channel)?;
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        let bit = mode_bit(index);
        if self.saved[index].is_none() {
            self.saved[index] = Some(SavedChannel {
                mode_bit: chip.ec_read(FAN_CONTROL_MODE)? & bit,
                command: chip.ec_read(command_register(index))?,
            });
        }
        chip.config_update(|| {
            let mode = chip.ec_read(FAN_CONTROL_MODE)?;
            chip.ec_write(FAN_CONTROL_MODE, mode | bit)?;
            chip.ec_write(command_register(index), duty_to_raw(percent))
        })
    }

    fn set_auto(&mut self, channel: &str) -> Result<(), HwError> {
        let index = self.index_of(channel)?;
        let Some(saved) = self.saved[index] else {
            return Ok(()); // jamais touché : déjà au firmware
        };
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        restore_channel(&chip, index, saved)?;
        self.saved[index] = None;
        Ok(())
    }

    fn restore_all(&mut self) {
        let chip = lock(&self.chip);
        // shortcut: si un autre outil garde le mutex > 500 ms on restaure sans lui ; les ventilos priment.
        let _isa = chip.lock_isa(ISA_RESTORE_TIMEOUT).ok();
        for (index, slot) in self.saved.iter_mut().enumerate() {
            if let Some(saved) = *slot
                && restore_channel(&chip, index, saved).is_ok()
            {
                *slot = None;
            }
        }
    }
}

impl Drop for Nct6687Fans {
    fn drop(&mut self) {
        self.restore_all();
    }
}

/// Restauration LHM non-DR (Nct677X.cs l.1522-1535) : bit de mode d'abord, puis consigne
/// dans une phase de config. L'ordre (mode avant requête) est celui de LHM, conservé tel quel.
fn restore_channel(chip: &Chip, index: usize, saved: SavedChannel) -> Result<(), HwError> {
    let mode = chip.ec_read(FAN_CONTROL_MODE)?;
    chip.ec_write(FAN_CONTROL_MODE, restored_mode(mode, mode_bit(index), saved.mode_bit))?;
    chip.config_update(|| chip.ec_write(command_register(index), saved.command))
}

/// Puce détectée : module LpcIO chargé + adresse de base (BAR) du moniteur matériel.
struct Chip {
    pawn: PawnIo,
    base: u16,
    isa_mutex: OwnedHandle,
}

impl Chip {
    /// Entre en mode config sur chaque slot, vérifie l'ID exact, lit la BAR, ressort.
    fn detect(&self) -> Result<u16, HwError> {
        for (slot, port) in SLOTS {
            self.pawn.execute("ioctl_select_slot", &[slot], &mut [])?;
            self.pio_out(port, ENTER_KEY)?;
            self.pio_out(port, ENTER_KEY)?;
            let id = (self.sio_in(CHIP_ID_REGISTER)?, self.sio_in(CHIP_REVISION_REGISTER)?);
            if id != NCT6687D_ID {
                // Comme LHM : on ne ressort que si une puce a répondu.
                if !matches!(id.0, 0x00 | 0xFF) {
                    self.pio_out(port, EXIT_KEY)?;
                }
                continue;
            }
            let base = self.read_hwm_base();
            let exited = self.pio_out(port, EXIT_KEY);
            let base = base?;
            exited?;
            return Ok(base);
        }
        Err(HwError::Unavailable("NCT6687D introuvable en 0x2E/0x4E".into()))
    }

    /// `ioctl_find_bars` autorise ensuite l'accès aux ports de la BAR (LpcIO.p).
    fn read_hwm_base(&self) -> Result<u16, HwError> {
        self.pawn.execute("ioctl_find_bars", &[], &mut [])?;
        self.sio_out(DEVICE_SELECT_REGISTER, HWM_LOGICAL_DEVICE)?;
        let address = self.sio_inw(BASE_ADDRESS_REGISTER)?;
        sleep(Duration::from_millis(1));
        let verify = self.sio_inw(BASE_ADDRESS_REGISTER)?;
        validate_base(address, verify)
    }

    /// Lit un octet de l'espace EC (adresse = page << 8 | index).
    fn ec_read(&self, address: u16) -> Result<u8, HwError> {
        self.ec_select(address)?;
        let value = self.pio_in(self.base + EC_DATA_OFFSET);
        let released = self.pio_out(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        let value = value?;
        released?;
        Ok(value)
    }

    fn ec_write(&self, address: u16, value: u8) -> Result<(), HwError> {
        self.ec_select(address)?;
        let written = self.pio_out(self.base + EC_DATA_OFFSET, value);
        let released = self.pio_out(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        written?;
        released
    }

    /// Attend que l'EC soit libre (page = 0xFF, 500 ms max puis force, comme LHM) et pointe `address`.
    fn ec_select(&self, address: u16) -> Result<(), HwError> {
        let page_port = self.base + EC_PAGE_OFFSET;
        let start = Instant::now();
        while self.pio_in(page_port)? != EC_PAGE_FREE {
            if start.elapsed() >= EC_ACCESS_TIMEOUT {
                self.pio_out(page_port, EC_PAGE_FREE)?;
                break;
            }
            sleep(Duration::from_millis(1));
        }
        let (page, index) = ec_page_index(address);
        self.pio_out(page_port, page)?;
        self.pio_out(self.base + EC_INDEX_OFFSET, index)
    }

    /// Encadre `body` par requête/fin de config ; la fin est envoyée même si `body` échoue,
    /// pour ne jamais laisser l'EC bloqué en phase de config.
    fn config_update(&self, body: impl FnOnce() -> Result<(), HwError>) -> Result<(), HwError> {
        self.ec_write(FAN_PWM_REQUEST, FAN_CFG_REQUEST)?;
        sleep(FAN_CFG_DELAY);
        let applied = body();
        let completed = self.ec_write(FAN_PWM_REQUEST, FAN_CFG_COMPLETE);
        sleep(FAN_CFG_DELAY);
        applied?;
        completed
    }

    fn pio_in(&self, port: u16) -> Result<u8, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_pio_inb", &[u64::from(port)], &mut out)?;
        Ok(out[0] as u8)
    }

    fn pio_out(&self, port: u16, value: u8) -> Result<(), HwError> {
        self.pawn.execute("ioctl_pio_outb", &[u64::from(port), u64::from(value)], &mut [])
    }

    fn sio_in(&self, register: u8) -> Result<u8, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_superio_inb", &[u64::from(register)], &mut out)?;
        Ok(out[0] as u8)
    }

    fn sio_inw(&self, register: u8) -> Result<u16, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_superio_inw", &[u64::from(register)], &mut out)?;
        Ok(out[0] as u16)
    }

    fn sio_out(&self, register: u8, value: u8) -> Result<(), HwError> {
        self.pawn.execute("ioctl_superio_outb", &[u64::from(register), u64::from(value)], &mut [])
    }

    /// Mutex `Global\Access_ISABUS.HTP.Method` partagé avec LHM/HWiNFO/etc. (LpcIO.cs l.23).
    fn lock_isa(&self, timeout: Duration) -> Result<IsaGuard<'_>, HwError> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: handle de mutex valide, possédé par `self` qui survit au guard.
        let wait = unsafe { WaitForSingleObject(HANDLE(self.isa_mutex.as_raw_handle()), millis) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(IsaGuard(&self.isa_mutex))
        } else {
            Err(HwError::Other("bus ISA occupé par un autre outil".into()))
        }
    }
}

/// Libère le mutex ISA au Drop (même thread que l'acquisition : guard local).
struct IsaGuard<'a>(&'a OwnedHandle);

impl Drop for IsaGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: mutex acquis par ce thread dans `lock_isa`, handle encore ouvert.
        let _ = unsafe { ReleaseMutex(HANDLE(self.0.as_raw_handle())) };
    }
}

fn create_isa_mutex() -> Result<OwnedHandle, HwError> {
    // SAFETY: nom statique terminé par zéro, attributs de sécurité par défaut.
    let raw = unsafe { CreateMutexW(None, false, w!(r"Global\Access_ISABUS.HTP.Method")) }
        .map_err(|err| HwError::Io(err.into()))?;
    // SAFETY: handle valide venant de CreateMutexW, possédé exclusivement ici.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}

/// Nom de la carte mère (SMBIOS, recopié par Windows dans le registre).
fn baseboard_product() -> Result<String, HwError> {
    let mut buffer = [0u16; 256];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: tampon et taille valides pendant l'appel, chaînes statiques terminées par zéro.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\BIOS"),
            w!("BaseBoardProduct"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .map_err(|err| HwError::Unavailable(format!("modèle de carte mère illisible : {err}")))?;
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

fn lock(chip: &Mutex<Chip>) -> MutexGuard<'_, Chip> {
    // La puce n'a pas d'invariant Rust cassable par un panic : on récupère le verrou.
    chip.lock().unwrap_or_else(PoisonError::into_inner)
}

fn reading(id: String, label: &str, kind: SensorKind, value: f32) -> Reading {
    Reading { id, label: label.into(), device: Device::Motherboard, kind, value }
}

// --- Fonctions pures (testées) ---

fn is_nct6687dr_board(product: &str) -> bool {
    let product = product.to_ascii_uppercase();
    NCT6687DR_BOARDS.iter().any(|model| product.contains(model))
}

/// BAR stable entre deux lectures, ≥ 0x100 et alignée sur 8 (LpcIO.cs l.755).
fn validate_base(address: u16, verify: u16) -> Result<u16, HwError> {
    if address != verify {
        return Err(HwError::Unavailable(format!("BAR instable : {address:#x} puis {verify:#x}")));
    }
    if address < 0x100 || address & 0xF007 != 0 {
        return Err(HwError::Unavailable(format!("BAR invalide : {address:#x}")));
    }
    Ok(address)
}

fn ec_page_index(address: u16) -> (u8, u8) {
    let [page, index] = address.to_be_bytes();
    (page, index)
}

fn temp_register(index: usize) -> u16 {
    TEMP_BASE + 2 * index as u16
}

fn fan_rpm_register(index: usize) -> u16 {
    FAN_RPM_BASE + 2 * index as u16
}

fn command_register(index: usize) -> u16 {
    FAN_PWM_COMMAND_BASE + index as u16
}

fn mode_bit(index: usize) -> u8 {
    1 << index
}

/// Octet signé + demi-degré au bit 7 de l'octet suivant (Nct677X.cs l.884-886).
fn decode_temperature(integer: u8, half: u8) -> f32 {
    f32::from(integer as i8) + 0.5 * f32::from(half >> 7)
}

/// 0..=100 % -> 0..=255, troncature en f32 comme `GetSoftwareValueAsByte` de LHM.
fn duty_to_raw(percent: u8) -> u8 {
    (f32::from(percent.min(100)) * 2.55) as u8
}

/// Remet uniquement le bit du canal à sa valeur sauvegardée, sans toucher aux autres canaux.
fn restored_mode(current: u8, bit: u8, saved_bit: u8) -> u8 {
    (current & !bit) | (saved_bit & bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Nct6687Sensors>();
        assert_send::<Nct6687Fans>();
    }

    #[test]
    fn register_addresses() {
        assert_eq!(temp_register(0), 0x100);
        assert_eq!(temp_register(6), 0x10C);
        assert_eq!(fan_rpm_register(0), 0x140);
        assert_eq!(fan_rpm_register(7), 0x14E);
        assert_eq!(command_register(0), 0xA28);
        assert_eq!(command_register(7), 0xA2F);
        assert_eq!(mode_bit(7), 0x80);
        assert_eq!(ec_page_index(0xA28), (0x0A, 0x28));
        assert_eq!(ec_page_index(0x100), (0x01, 0x00));
    }

    #[test]
    fn duty_scaling_matches_lhm() {
        assert_eq!(duty_to_raw(0), 0);
        assert_eq!(duty_to_raw(1), 2); // 2.55 tronqué
        assert_eq!(duty_to_raw(50), 127);
        assert_eq!(duty_to_raw(100), 255);
        assert_eq!(duty_to_raw(250), 255); // borné à 100 %
    }

    #[test]
    fn temperature_decoding() {
        assert_eq!(decode_temperature(45, 0x00), 45.0);
        assert_eq!(decode_temperature(45, 0x80), 45.5);
        assert_eq!(decode_temperature(45, 0x7F), 45.0); // seul le bit 7 compte
        assert_eq!(decode_temperature(0xFE, 0x80), -1.5);
    }

    #[test]
    fn chip_and_board_checks() {
        assert_eq!(NCT6687D_ID, (0xD5, 0x92));
        assert!(!is_nct6687dr_board("MAG Z790 GAMING PLUS WIFI (MS-7E06)"));
        assert!(is_nct6687dr_board("MAG X870 TOMAHAWK WIFI (MS-7E51)"));
        assert!(is_nct6687dr_board("mpg z890 carbon wifi"));
    }

    #[test]
    fn base_address_validation() {
        assert_eq!(validate_base(0xA20, 0xA20).ok(), Some(0xA20));
        assert!(validate_base(0xA20, 0xA28).is_err()); // instable
        assert!(validate_base(0x0A0, 0x0A0).is_err()); // < 0x100
        assert!(validate_base(0xA25, 0xA25).is_err()); // non aligné
        assert!(validate_base(0xFFFF, 0xFFFF).is_err());
    }

    #[test]
    fn restore_touches_only_own_bit() {
        // Canal 2 était en auto (bit à 0) ; canaux 0 et 5 en manuel par ailleurs.
        assert_eq!(restored_mode(0b0010_0101, 0b100, 0), 0b0010_0001);
        // Canal 2 était déjà en manuel à l'origine : le bit reste à 1.
        assert_eq!(restored_mode(0b0000_0000, 0b100, 0b100), 0b0000_0100);
    }
}
