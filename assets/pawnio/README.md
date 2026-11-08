# PawnIO: signed modules

Official, unmodified PawnIO modules, taken from
https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11 (`release_0_2_11.zip`, version 0.2.11).
License: LGPL-2.1-or-later (© namazso), source code: https://github.com/namazso/PawnIO.Modules

| File | Use | SHA-256 |
|------|-----|---------|
| `IntelMSR.bin` | Intel CPU package temperature (MSR) | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` |
| `LpcIO.bin` | NCT6687D Super I/O (ports 0x2E/0x4E + hardware monitor BAR) | `b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e` |
| `SmbusI801.bin` | Intel i801/PCH SMBus host (`smbus_probe` scan, Corsair DRAM RGB in the daemon) | `a0f7d066e7efda28c0e754c1f52dbd8dc280d388ba8575b514a80cb67490530c` |

`SmbusI801.bin` comes from the same `release_0_2_11.zip` (downloaded with
`gh release download 0.2.11 -R namazso/PawnIO.Modules`); source: `SmbusI801.p` at tag 0.2.11.

Requirement: the PawnIO driver must be installed on the machine
(`PawnIO_setup.exe` from https://pawnio.eu or https://github.com/namazso/PawnIO/releases).
Access to the `\Device\PawnIO` device requires administrator rights.
