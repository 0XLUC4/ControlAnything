//! Service Windows « ControlAnything » : installation/désinstallation et point d'entrée SCM.
//!
//! Le service tourne en LocalSystem, démarre au boot et redémarre 5 s après un échec.
//! L'arrêt (Stop/extinction) passe par le même chemin que la console : ventilos rendus au BIOS.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "ControlAnything";
const DISPLAY_NAME: &str = "ControlAnything — contrôle du PC";
const DESCRIPTION: &str = "Contrôle le matériel du PC (ventilateurs, capteurs…) pour ControlAnything \
     et rend les ventilateurs au BIOS à l'arrêt.";
/// Service de l'ancien nom « PC Hub », supprimé par `install`.
const LEGACY_SERVICE_NAME: &str = "PCHub";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_EXISTS: i32 = 1073;

/// Traduit les erreurs Win32 courantes en message clair.
fn explain(err: windows_service::Error, action: &str) -> anyhow::Error {
    let code = match &err {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    };
    match code {
        Some(ERROR_ACCESS_DENIED) => anyhow::anyhow!("accès refusé : relancez en administrateur"),
        Some(ERROR_SERVICE_EXISTS) => {
            anyhow::anyhow!("le service {SERVICE_NAME} est déjà installé (désinstallez-le d'abord)")
        }
        Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            anyhow::anyhow!("le service {SERVICE_NAME} n'est pas installé")
        }
        _ => anyhow::anyhow!("{action} : {err}"),
    }
}

/// Crée le service (démarrage auto, LocalSystem, redémarrage après 5 s) puis le démarre.
pub fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| explain(e, "connexion au gestionnaire de services"))?;
    remove_legacy(&manager)?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec!["service".into()],
        dependencies: Vec::new(),
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
        .map_err(|e| explain(e, "création du service"))?;
    service
        .set_description(DESCRIPTION)
        .map_err(|e| explain(e, "description du service"))?;
    let restart = ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(5),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart.clone(), restart.clone(), restart]),
        })
        .map_err(|e| explain(e, "actions en cas d'échec"))?;
    // Un arrêt en erreur (pas seulement un crash) déclenche aussi le redémarrage.
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| explain(e, "actions en cas d'échec"))?;
    println!("service {SERVICE_NAME} installé ({})", exe.display());
    service
        .start(&[] as &[&OsStr])
        .map_err(|e| explain(e, "démarrage du service"))?;
    println!(
        "service {SERVICE_NAME} démarré ; logs dans {}",
        log_dir().display()
    );
    Ok(())
}

/// Arrête le service (attente max 10 s) puis le supprime.
pub fn uninstall() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| explain(e, "connexion au gestionnaire de services"))?;
    let service = manager
        .open_service(SERVICE_NAME, STOP_DELETE)
        .map_err(|e| explain(e, "ouverture du service"))?;
    stop_and_delete(&service, SERVICE_NAME)?;
    println!("service {SERVICE_NAME} désinstallé");
    Ok(())
}

/// Droits nécessaires pour arrêter puis supprimer un service.
const STOP_DELETE: ServiceAccess = ServiceAccess::QUERY_STATUS
    .union(ServiceAccess::STOP)
    .union(ServiceAccess::DELETE);

/// Supprime l'ancien service « PCHub » s'il existe (son arrêt rend les ventilos au BIOS).
fn remove_legacy(manager: &ServiceManager) -> anyhow::Result<()> {
    let service = match manager.open_service(LEGACY_SERVICE_NAME, STOP_DELETE) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(io))
            if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
            return Ok(());
        }
        Err(e) => return Err(explain(e, "ouverture de l'ancien service PCHub")),
    };
    stop_and_delete(&service, LEGACY_SERVICE_NAME)?;
    println!("ancien service {LEGACY_SERVICE_NAME} supprimé");
    Ok(())
}

/// Arrête `service` (attente max 10 s) puis le supprime.
fn stop_and_delete(service: &Service, name: &str) -> anyhow::Result<()> {
    let status = service
        .query_status()
        .map_err(|e| explain(e, "état du service"))?;
    if status.current_state != ServiceState::Stopped {
        service.stop().map_err(|e| explain(e, "arrêt du service"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = service
                .query_status()
                .map_err(|e| explain(e, "état du service"))?
                .current_state;
            if state == ServiceState::Stopped {
                println!("service {name} arrêté (ventilos rendus au BIOS)");
                break;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "le service {name} ne s'est pas arrêté en 10 s ; suppression à son arrêt"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    service
        .delete()
        .map_err(|e| explain(e, "suppression du service"))?;
    Ok(())
}

/// `%ProgramData%\ControlAnything\logs`, à côté de la config.
fn log_dir() -> PathBuf {
    logs_beside(&crate::config_store::config_path())
}

fn logs_beside(config: &Path) -> PathBuf {
    config.parent().unwrap_or(Path::new(".")).join("logs")
}

define_windows_service!(ffi_service_main, service_main);

/// Point d'entrée `controlanythingd service` : logs fichiers puis rend la main au SCM (bloquant).
pub fn run_dispatcher() -> anyhow::Result<()> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("controlanythingd")
        .filename_suffix("log")
        .max_log_files(14)
        .build(log_dir())?;
    let (writer, _guard) = tracing_appender::non_blocking(appender);
    crate::init_tracing(writer);
    // `_guard` vit jusqu'à la fin du service : il vide les logs restants à la sortie.
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| {
        explain(
            e,
            "lancement par le gestionnaire de services (utilisez `install`)",
        )
    })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        tracing::error!("service : {err:#}");
    }
}

fn run_service() -> anyhow::Result<()> {
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(control); // service déjà en arrêt : rien à faire
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state: ServiceState, exit_code: ServiceExitCode| {
        let controls_accepted = if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        };
        let result = status.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted,
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
        if let Err(err) = result {
            tracing::warn!("statut {state:?} non transmis au SCM : {err}");
        }
    };

    report(ServiceState::StartPending, ServiceExitCode::NO_ERROR);
    let result = tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            runtime.block_on(async {
                let daemon = crate::start()?;
                report(ServiceState::Running, ServiceExitCode::NO_ERROR);
                tracing::info!("service {SERVICE_NAME} démarré");
                crate::run(daemon, async {
                    let control = stop_rx.recv().await;
                    tracing::info!("{control:?} demandé par le SCM, arrêt");
                    report(ServiceState::StopPending, ServiceExitCode::NO_ERROR);
                })
                .await
            })
        });
    // Code non nul -> le SCM applique les actions d'échec (redémarrage après 5 s).
    let exit_code = match &result {
        Ok(()) => ServiceExitCode::NO_ERROR,
        Err(_) => ServiceExitCode::ServiceSpecific(1),
    };
    report(ServiceState::Stopped, exit_code);
    result
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn logs_beside_config() {
        let config = Path::new(r"C:\ProgramData\ControlAnything\config.toml");
        assert_eq!(
            super::logs_beside(config),
            Path::new(r"C:\ProgramData\ControlAnything\logs")
        );
    }
}
