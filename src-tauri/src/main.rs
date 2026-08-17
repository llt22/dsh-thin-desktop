mod discovery;
mod launcher;

use launcher::{LauncherState, RuntimeSnapshot};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
fn get_snapshot(state: State<'_, LauncherState>) -> Result<RuntimeSnapshot, String> {
    state.snapshot()
}

#[tauri::command]
fn get_diagnostics(state: State<'_, LauncherState>) -> Result<String, String> {
    state.diagnostics()
}

#[tauri::command]
async fn restart_dsh(app: AppHandle, state: State<'_, LauncherState>) -> Result<(), String> {
    let launcher = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || launcher.restart(&app))
        .await
        .map_err(|error| format!("重启任务失败：{error}"))?
}

fn main() {
    let launcher = LauncherState::new();
    let setup_launcher = launcher.clone();

    let app = tauri::Builder::default()
        .manage(launcher)
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_diagnostics,
            restart_dsh
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let signal_handle = handle.clone();
            let signal_launcher = setup_launcher.clone();
            ctrlc::set_handler(move || {
                let _ = signal_launcher.stop_current(&signal_handle);
                signal_handle.exit(0);
            })
            .map_err(|error| format!("failed to register signal handler: {error}"))?;

            let launcher = setup_launcher.clone();
            std::thread::spawn(move || {
                if let Err(error) = launcher.start(&handle) {
                    launcher.set_start_error(&handle, error);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build DSH Thin Desktop");

    app.run(|app_handle, event| match event {
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { .. },
            ..
        } if label == "main" => {
            let launcher = app_handle.state::<LauncherState>();
            let _ = launcher.stop_current(app_handle);
            app_handle.exit(0);
        }
        tauri::RunEvent::Exit => {
            let launcher = app_handle.state::<LauncherState>();
            let _ = launcher.stop_current(app_handle);
        }
        _ => {}
    });
}
