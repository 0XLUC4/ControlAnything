//! "ControlAnything" Windows service: install/uninstall and SCM entry point.
//!
//! The service runs as LocalSystem, starts at boot and restarts 5 s after a failure.
//! Stopping (Stop/shutdown) goes through the same path as the console: fans handed back to the BIOS.

use crate::control::Command;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc::Sender};
use std::time::{Duration, Instant};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use windows_service::service::{
    PowerEventParam, Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl,
    ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
    ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "ControlAnything";
const DISPLAY_NAME: &str = "ControlAnything - PC control";
const DESCRIPTION: &str = "Controls the PC hardware (fans, sensors...) for ControlAnything \
     and hands the fans back to the BIOS on stop.";
/// Service of the former "PC Hub" name, removed by `install`.
const LEGACY_SERVICE_NAME: &str = "PCHub";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_EXISTS: i32 = 1073;

/// Translates common Win32 errors into a clear message.
fn explain(err: windows_service::Error, action: &str) -> anyhow::Error {
    let code = match &err {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    };
    match code {
        Some(ERROR_ACCESS_DENIED) => anyhow::anyhow!("access denied: run again as administrator"),
        Some(ERROR_SERVICE_EXISTS) => {
            anyhow::anyhow!("the {SERVICE_NAME} service is already installed (uninstall it first)")
        }
        Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            anyhow::anyhow!("the {SERVICE_NAME} service is not installed")
        }
        _ => anyhow::anyhow!("{action}: {err}"),
    }
}

/// Creates the service (auto start, LocalSystem, restart after 5 s) then starts it.
pub fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| explain(e, "connecting to the service manager"))?;
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
        .map_err(|e| explain(e, "creating the service"))?;
    service
        .set_description(DESCRIPTION)
        .map_err(|e| explain(e, "service description"))?;
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
        .map_err(|e| explain(e, "failure actions"))?;
    // A stop with an error (not only a crash) also triggers the restart.
    service
        .set_failure_actions_on_non_crash_failures(true)
        .map_err(|e| explain(e, "failure actions"))?;
    println!("service {SERVICE_NAME} installed ({})", exe.display());
    service
        .start(&[] as &[&OsStr])
        .map_err(|e| explain(e, "starting the service"))?;
    println!(
        "service {SERVICE_NAME} started; logs in {}",
        log_dir().display()
    );
    Ok(())
}

/// Stops the service (waits up to 10 s) then deletes it.
pub fn uninstall() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| explain(e, "connecting to the service manager"))?;
    let service = manager
        .open_service(SERVICE_NAME, STOP_DELETE)
        .map_err(|e| explain(e, "opening the service"))?;
    stop_and_delete(&service, SERVICE_NAME)?;
    println!("service {SERVICE_NAME} uninstalled");
    Ok(())
}

/// Rights needed to stop then delete a service.
const STOP_DELETE: ServiceAccess = ServiceAccess::QUERY_STATUS
    .union(ServiceAccess::STOP)
    .union(ServiceAccess::DELETE);

/// Removes the old "PCHub" service if it exists (stopping it hands the fans back to the BIOS).
fn remove_legacy(manager: &ServiceManager) -> anyhow::Result<()> {
    let service = match manager.open_service(LEGACY_SERVICE_NAME, STOP_DELETE) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(io))
            if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST) =>
        {
            return Ok(());
        }
        Err(e) => return Err(explain(e, "opening the old PCHub service")),
    };
    stop_and_delete(&service, LEGACY_SERVICE_NAME)?;
    println!("old service {LEGACY_SERVICE_NAME} removed");
    Ok(())
}

/// Stops `service` (waits up to 10 s) then deletes it.
fn stop_and_delete(service: &Service, name: &str) -> anyhow::Result<()> {
    let status = service
        .query_status()
        .map_err(|e| explain(e, "service status"))?;
    if status.current_state != ServiceState::Stopped {
        service
            .stop()
            .map_err(|e| explain(e, "stopping the service"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = service
                .query_status()
                .map_err(|e| explain(e, "service status"))?
                .current_state;
            if state == ServiceState::Stopped {
                println!("service {name} stopped (fans handed back to the BIOS)");
                break;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "the {name} service did not stop within 10 s; it will be deleted when it stops"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    service
        .delete()
        .map_err(|e| explain(e, "deleting the service"))?;
    Ok(())
}

/// `%ProgramData%\ControlAnything\logs`, next to the config.
fn log_dir() -> PathBuf {
    logs_beside(&crate::config_store::config_path())
}

fn logs_beside(config: &Path) -> PathBuf {
    config.parent().unwrap_or(Path::new(".")).join("logs")
}

define_windows_service!(ffi_service_main, service_main);

/// `controlanythingd service` entry point: file logs, then hands control to the SCM (blocking).
pub fn run_dispatcher() -> anyhow::Result<()> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("controlanythingd")
        .filename_suffix("log")
        .max_log_files(14)
        .build(log_dir())?;
    let (writer, _guard) = tracing_appender::non_blocking(appender);
    crate::init_tracing(writer);
    // `_guard` lives until the service ends: it flushes the remaining logs on exit.
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| explain(e, "launch by the service manager (use `install`)"))
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        tracing::error!("service: {err:#}");
    }
}

fn run_service() -> anyhow::Result<()> {
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
    // Control thread commands, known once the daemon has started.
    let commands: Arc<OnceLock<Sender<Command>>> = Arc::default();
    let power = commands.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(control); // service already stopping: nothing to do
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(event) => {
            let command = match event {
                PowerEventParam::Suspend => Some(Command::Suspend),
                PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend => {
                    Some(Command::Resume)
                }
                _ => None,
            };
            if let (Some(command), Some(tx)) = (command, power.get()) {
                let _ = tx.send(command); // control thread stopped: nothing to do
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state: ServiceState, exit_code: ServiceExitCode| {
        let controls_accepted = if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::POWER_EVENT
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
            tracing::warn!("status {state:?} not sent to the SCM: {err}");
        }
    };

    report(ServiceState::StartPending, ServiceExitCode::NO_ERROR);
    let result = tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            runtime.block_on(async {
                let daemon = crate::start()?;
                let _ = commands.set(daemon.commands.clone()); // set once, here
                report(ServiceState::Running, ServiceExitCode::NO_ERROR);
                tracing::info!("service {SERVICE_NAME} started");
                crate::run(daemon, async {
                    let control = stop_rx.recv().await;
                    tracing::info!("{control:?} requested by the SCM, stopping");
                    report(ServiceState::StopPending, ServiceExitCode::NO_ERROR);
                })
                .await
            })
        });
    // Non-zero code -> the SCM applies the failure actions (restart after 5 s).
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
