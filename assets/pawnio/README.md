# PawnIO — modules signés

Modules PawnIO officiels, non modifiés, issus de
https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11 (`release_0_2_11.zip`, version 0.2.11).
Licence : LGPL-2.1-or-later (© namazso), code source : https://github.com/namazso/PawnIO.Modules

| Fichier | Usage | SHA-256 |
|---------|-------|---------|
| `IntelMSR.bin` | température package CPU Intel (MSR) | `d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f` |
| `LpcIO.bin` | Super I/O NCT6687D (ports 0x2E/0x4E + BAR du moniteur matériel) | `b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e` |

Prérequis : le pilote PawnIO doit être installé sur la machine
(`PawnIO_setup.exe` depuis https://pawnio.eu ou https://github.com/namazso/PawnIO/releases).
L'accès au périphérique `\Device\PawnIO` exige les droits administrateur.
