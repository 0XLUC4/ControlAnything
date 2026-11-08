//! Cœur de ControlAnything : types de capteurs, trait des backends matériels, protocole IPC.
//! Aucune dépendance UI ni OS : partagé par le daemon, les backends et l'UI.

use serde::{Deserialize, Serialize};

/// Nature d'une mesure, qui détermine son unité.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorKind {
    Temperature,
    Load,
    Fan,
    Power,
    Clock,
    Memory,
}

impl SensorKind {
    /// Unité d'affichage de la valeur brute.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Load => "%",
            Self::Fan => "RPM",
            Self::Power => "W",
            Self::Clock => "MHz",
            Self::Memory => "MiB",
        }
    }
}

/// Composant matériel auquel appartient un capteur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Device {
    Cpu,
    Gpu,
    Motherboard,
    Memory,
    Storage,
}

/// Une mesure instantanée.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// Identifiant stable et unique, ex. `cpu/package_temp`. Sert de clé d'historique.
    pub id: String,
    pub label: String,
    pub device: Device,
    pub kind: SensorKind,
    pub value: f32,
}

/// Erreurs remontées par les backends matériels.
#[derive(Debug, thiserror::Error)]
pub enum HwError {
    #[error("accès refusé (droits admin/root requis) : {0}")]
    PermissionDenied(String),
    #[error("matériel ou pilote non disponible : {0}")]
    Unavailable(String),
    #[error("erreur E/S : {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Source de capteurs (un backend par famille de matériel).
pub trait SensorProvider: Send {
    /// Nom court pour les logs.
    fn name(&self) -> &str;
    /// Ajoute les mesures courantes dans `out`.
    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError>;
}

pub mod config;
pub mod control;
pub mod fan;
pub mod rgb;

/// Protocole daemon <-> UI : une ligne JSON par message.
pub mod ipc {
    use super::Reading;
    use crate::config::Config;
    use crate::fan::{FanChannel, FanStatus};
    use crate::rgb::RgbDeviceInfo;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    /// Messages envoyés par le daemon.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum DaemonMsg {
        /// Mesures, à chaque tick.
        Snapshot(Snapshot),
        /// Consignes appliquées + état du failsafe, à chaque tick.
        Fans {
            fans: Vec<FanStatus>,
            failsafe: bool,
        },
        /// Config courante et canaux détectés : à la connexion et après chaque changement.
        State {
            config: Config,
            channels: Vec<FanChannel>,
        },
        /// Appareils RGB détectés : à la connexion et quand la liste change.
        Rgb { devices: Vec<RgbDeviceInfo> },
        /// Commande refusée (config invalide, profil inconnu…).
        Error { message: String },
    }

    /// Commandes envoyées par l'UI. Le daemon re-valide tout : il ne fait jamais confiance au client.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ClientMsg {
        /// Remplace toute la config (validée puis sauvegardée par le daemon).
        SetConfig { config: Config },
        /// Change de profil actif.
        SetProfile { name: String },
    }

    /// Nom du named pipe Windows exposé par le daemon.
    pub const PIPE_NAME: &str = r"\\.\pipe\controlanything";
    /// Socket Unix exposée par le daemon (Linux).
    pub const SOCKET_PATH: &str = "/run/controlanything/daemon.sock";

    /// État complet des capteurs à un instant donné.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Snapshot {
        /// Millisecondes depuis l'époque Unix.
        pub timestamp_ms: u64,
        pub readings: Vec<Reading>,
    }

    /// Sérialise un message en une ligne terminée par `\n`.
    pub fn encode<T: Serialize>(msg: &T) -> serde_json::Result<String> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        Ok(line)
    }

    /// Désérialise une ligne reçue.
    pub fn decode<T: DeserializeOwned>(line: &str) -> serde_json::Result<T> {
        serde_json::from_str(line.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip() {
        let snapshot = ipc::Snapshot {
            timestamp_ms: 42,
            readings: vec![Reading {
                id: "cpu/package_temp".into(),
                label: "CPU Package".into(),
                device: Device::Cpu,
                kind: SensorKind::Temperature,
                value: 51.5,
            }],
        };
        let msg = ipc::DaemonMsg::Snapshot(snapshot);
        let line = ipc::encode(&msg).expect("encode");
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(ipc::decode::<ipc::DaemonMsg>(&line).expect("decode"), msg);
    }
}
