# PawnIO: signed modules

Official, unmodified PawnIO modules, taken from
https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11 (`release_0_2_11.zip`, version 0.2.11).
License: LGPL-2.1-or-later (© namazso), source code: https://github.com/namazso/PawnIO.Modules

| File | Use | SHA-256 |
|------|-----|---------|
| `IntelMSR.bin` | Intel CPU package temperature (MSR) | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` |
| `LpcIO.bin` | NCT6687D Super I/O (ports 0x2E/0x4E + hardware monitor BAR) | `b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e` |

Requirement: the PawnIO driver must be installed on the machine
(`PawnIO_setup.exe` from https://pawnio.eu or https://github.com/namazso/PawnIO/releases).
Access to the `\Device\PawnIO` device requires administrator rights.
