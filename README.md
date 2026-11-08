# ControlAnything

Contrôler tout son PC depuis une seule application. Aujourd'hui, façon FanControl : lit les capteurs (CPU, GPU, carte mère) et pilote les
ventilateurs selon des courbes et des profils. Un daemon privilégié fait le travail matériel,
une UI GPUI s'y connecte en IPC local (named pipe sous Windows, socket Unix sous Linux).

## Architecture

| Crate | Rôle |
|-------|------|
| `hub-core` | Types capteurs/ventilos, config, moteur de régulation (courbes, hystérésis, failsafe), protocole IPC. Sans OS ni UI. |
| `hub-backend-windows` | PawnIO : température CPU Intel (IntelMSR), Super I/O Nuvoton NCT6687D (LpcIO). |
| `hub-backend-linux` | sysfs hwmon (`/sys/class/hwmon`). |
| `hub-daemon` | Binaire `controlanythingd` : boucle de contrôle, serveur IPC, service Windows. |
| `hub-ui` | Interface GPUI (gpui-kit) : tableau de bord, ventilateurs, profils. |
| `hub-app` | Binaire `controlanything` (l'UI). |

## Prérequis

- Rust ≥ 1.98 (`rustup` lit `rust-toolchain.toml`).
- **Windows** : MSVC Build Tools (C++), et le pilote **PawnIO** : https://pawnio.eu ou
  `winget install namazso.PawnIO`. Les modules (`IntelMSR.bin`, `LpcIO.bin`) sont dans
  `assets/pawnio/` ; en release, les copier dans un dossier `pawnio\` à côté de `controlanythingd.exe`.
- **Linux** (Debian/Ubuntu) :
  ```sh
  sudo apt install libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libvulkan1 \
    libfontconfig-dev libglib2.0-dev libssl-dev libzstd-dev clang cmake libgtk-3-dev \
    libayatana-appindicator3-dev libxdo-dev libudev-dev
  ```
  Capteurs : voir [packaging/linux/README.md](packaging/linux/README.md) (`sensors-detect`,
  module DKMS `nct6687d` pour les cartes MSI).

## Compiler

```sh
cargo build --release
```

Binaires : `target/release/controlanythingd` (daemon) et `target/release/controlanything` (UI).

## Lancer

- Daemon en console **administrateur** (root sous Linux) : `controlanythingd` (ou `controlanythingd run`).
  Ctrl+C l'arrête proprement.
- UI (utilisateur normal) : `controlanything`.

## Service Windows (démarrage au boot)

Le service pointe vers l'exe tel quel : installer d'abord les binaires et `pawnio\` dans un
emplacement stable.

```powershell
cargo build --release
# console admin :
$dst = "C:\Program Files\ControlAnything"
New-Item -ItemType Directory -Force $dst | Out-Null
Copy-Item target\release\controlanythingd.exe, target\release\controlanything.exe $dst
Copy-Item assets\pawnio "$dst\pawnio" -Recurse -Force
& "$dst\controlanythingd.exe" install   # supprime aussi l'ancien service PCHub
```

`install` crée le service « ControlAnything » (auto, LocalSystem, redémarre 5 s après un échec)
et le démarre ; `controlanythingd uninstall` l'arrête (ventilos rendus au BIOS) puis le supprime.

Logs du service : `%ProgramData%\ControlAnything\logs\controlanythingd.*.log` (rotation quotidienne, 14 fichiers).

### Migration depuis PC Hub

- `install` arrête et supprime l'ancien service « PCHub » s'il existe.
- La config `%ProgramData%\PC Hub\config.toml` est copiée automatiquement vers
  `%ProgramData%\ControlAnything\config.toml` au premier lancement (l'ancienne reste en place).
- L'ancien démarrage automatique de l'UI est désactivé par la nouvelle UI.
- Le dossier `C:\Program Files\PC Hub` peut ensuite être supprimé.
Sous Linux : unité systemd, voir [packaging/linux](packaging/linux/README.md).

## Sécurité

- Pompe AIO : jamais sous **60 %**.
- Failsafe : CPU ≥ **90 °C** ou GPU ≥ **85 °C** → ventilos pilotés à 100 % jusqu'au retour 5 °C sous le seuil.
- Arrêt propre (Ctrl+C, fermeture, extinction, SIGTERM, stop du service) ou panique : ventilos rendus au BIOS.
- Kill brutal (`taskkill /F`, `kill -9`) : rien ne peut être restauré, le BIOS reprend la main au prochain redémarrage.

## Configuration

`%ProgramData%\ControlAnything\config.toml` (Windows) ou `/etc/controlanything/config.toml` (Linux), créée au
premier lancement. Un fichier invalide est mis de côté en `config.toml.bad` et remplacé par le défaut.

## Tests et validation matérielle

```sh
cargo test --workspace
```

Outils de validation NCT6687D (Windows, console admin) :

```powershell
cargo run -p hub-backend-windows --example nct6687_probe -- assets\pawnio\LpcIO.bin       # lecture seule
cargo run -p hub-backend-windows --example nct6687_write_test -- assets\pawnio\LpcIO.bin  # System Fan 1 : 100 %, 40 %, retour BIOS
```

## Packaging

- Windows (installeur NSIS, installe et démarre le service) : `powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1`
  → `target\packager\controlanything_<version>_x64-setup.exe`.
- Linux (.deb avec unité systemd) : `sh packaging/linux/build-deb.sh`.

Détails : [packaging/README.md](packaging/README.md).
