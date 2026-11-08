# Packaging

## Windows: NSIS installer (cargo-packager 0.11.8)

```powershell
powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
```

The script installs `cargo-packager` if missing, builds `hub-app` + `hub-daemon` in release, then runs
`cargo packager --release -c packaging\Packager.toml -f nsis` (NSIS is downloaded automatically
into `%LOCALAPPDATA%\.cargo-packager\NSIS`). Output: `target\packager\controlanything_<version>_x64-setup.exe`.

The installer (admin, `perMachine`):

- installs into `C:\Program Files\ControlAnything\`: `controlanything.exe`, `controlanythingd.exe`,
  `pawnio\*.bin` (+ `pawnio\README.md`, LGPL license of the modules) and `README.md`;
- creates the Start menu shortcut to `controlanything.exe`;
- **PawnIO**: if the `PawnIO` service does not exist, offers `winget install namazso.PawnIO`
  (otherwise opens https://pawnio.eu). `PawnIO_setup.exe` is **not** bundled (redistribution terms unverified);
- before copying: `controlanythingd.exe uninstall` on the old version if present (unlocks the exe);
- after copying: `controlanythingd.exe install` (creates/starts the service, removes the old PCHub service);
- uninstall: `controlanythingd.exe uninstall` (fans handed back to the BIOS) before the files are deleted.

These hooks live in `windows/installer.nsi`, a copy of the cargo-packager 0.11.8 template: the
changes are tagged `ControlAnything:`. Carry them over if cargo-packager is upgraded.

Silent install: `controlanything_..._x64-setup.exe /S` (no PawnIO question in this mode).

No MSI: WiX would need custom actions for the service hooks, not done.

The version in `Packager.toml` must follow `[workspace.package] version` in `Cargo.toml`.

## Linux: .deb package

```sh
sh packaging/linux/build-deb.sh   # on Linux, from the repo root
```

Builds `target/packager/controlanything_<version>_<arch>.deb` with `dpkg-deb` (cargo-packager 0.11
does not handle postinst/prerm scripts):

- `/usr/bin/controlanything`, `/usr/bin/controlanythingd`
- `/lib/systemd/system/controlanythingd.service`
- `/usr/share/applications/controlanything.desktop` + `/usr/share/pixmaps/controlanything.png`
  (`assets/icons/controlanything.png`, falling back to `fan.png`)
- `postinst`: `systemctl daemon-reload`, `enable`, `restart controlanythingd`;
  `prerm`: `systemctl disable --now controlanythingd`; `postrm`: `daemon-reload`.

Install: `sudo apt install ./target/packager/controlanything_*.deb`.

### AppImage

Only for the UI (`controlanything`): the daemon must run as root as a systemd service, which an
AppImage does not provide. The .deb (or the manual install in [linux/README.md](linux/README.md)) is still
required for `controlanythingd`. Not automated here.
