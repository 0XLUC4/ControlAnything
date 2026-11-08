# Packaging

## Windows : installeur NSIS (cargo-packager 0.11.8)

```powershell
powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
```

Le script installe `cargo-packager` si absent, compile `hub-app` + `hub-daemon` en release, puis lance
`cargo packager --release -c packaging\Packager.toml -f nsis` (NSIS est téléchargé automatiquement
dans `%LOCALAPPDATA%\.cargo-packager\NSIS`). Sortie : `target\packager\controlanything_<version>_x64-setup.exe`.

L'installeur (admin, `perMachine`) :

- installe dans `C:\Program Files\ControlAnything\` : `controlanything.exe`, `controlanythingd.exe`,
  `pawnio\*.bin` (+ `pawnio\README.md`, licence LGPL des modules) et `README.md` ;
- crée le raccourci du menu Démarrer vers `controlanything.exe` ;
- **PawnIO** : si le service `PawnIO` n'existe pas, propose `winget install namazso.PawnIO`
  (sinon ouvre https://pawnio.eu). `PawnIO_setup.exe` n'est **pas** embarqué (conditions de redistribution non vérifiées) ;
- avant copie : `controlanythingd.exe uninstall` sur l'ancienne version si présente (exe déverrouillé) ;
- après copie : `controlanythingd.exe install` (crée/démarre le service, supprime l'ancien service PCHub) ;
- désinstallation : `controlanythingd.exe uninstall` (ventilos rendus au BIOS) avant suppression des fichiers.

Ces hooks vivent dans `windows/installer.nsi`, copie du modèle de cargo-packager 0.11.8 : les
modifications sont marquées `ControlAnything:`. À reporter si cargo-packager est mis à jour.

Installation silencieuse : `controlanything_..._x64-setup.exe /S` (pas de question PawnIO dans ce mode).

Pas de MSI : WiX demanderait des custom actions pour les hooks du service, non fait.

La version de `Packager.toml` doit suivre `[workspace.package] version` de `Cargo.toml`.

## Linux : paquet .deb

```sh
sh packaging/linux/build-deb.sh   # sur Linux, depuis la racine du dépôt
```

Construit `target/packager/controlanything_<version>_<arch>.deb` avec `dpkg-deb` (cargo-packager 0.11
ne gère pas les scripts postinst/prerm) :

- `/usr/bin/controlanything`, `/usr/bin/controlanythingd`
- `/lib/systemd/system/controlanythingd.service`
- `/usr/share/applications/controlanything.desktop` + `/usr/share/pixmaps/controlanything.png`
  (`assets/icons/controlanything.png`, à défaut `fan.png`)
- `postinst` : `systemctl daemon-reload`, `enable`, `restart controlanythingd` ;
  `prerm` : `systemctl disable --now controlanythingd` ; `postrm` : `daemon-reload`.

Installer : `sudo apt install ./target/packager/controlanything_*.deb`.

### AppImage

Seulement pour l'UI (`controlanything`) : le daemon doit tourner en root comme service systemd, ce qu'une
AppImage ne fournit pas. Le .deb (ou l'installation manuelle de [linux/README.md](linux/README.md)) reste
nécessaire pour `controlanythingd`. Non automatisé ici.
