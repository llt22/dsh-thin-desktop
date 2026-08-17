mod discovery;
mod launcher;

use launcher::{LauncherState, RuntimeSnapshot};
use tauri::{webview::PageLoadEvent, AppHandle, Manager, State};

const EXTERNAL_LINK_BRIDGE: &str = r#"
(() => {
  if (window.__DSH_THIN_DESKTOP_EXTERNAL_LINKS__) return;
  window.__DSH_THIN_DESKTOP_EXTERNAL_LINKS__ = true;

  const isExternalWebUrl = (value) => {
    try {
      const url = new URL(value, window.location.href);
      return (url.protocol === 'http:' || url.protocol === 'https:')
        && url.origin !== window.location.origin;
    } catch (_) {
      return false;
    }
  };

  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null;
    if (!anchor || !isExternalWebUrl(anchor.href)) return;
    event.preventDefault();
    window.location.assign(anchor.href);
  }, true);

  const nativeOpen = window.open.bind(window);
  window.open = (url, target, features) => {
    if (typeof url === 'string' && isExternalWebUrl(url)) {
      window.location.assign(url);
      return null;
    }
    return nativeOpen(url, target, features);
  };
})();
"#;

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
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("external-links")
                .on_navigation(|webview, url| {
                    if !matches!(url.scheme(), "http" | "https")
                        || url.host_str() == Some("tauri.localhost")
                        || webview.state::<LauncherState>().owns_url(url)
                    {
                        return true;
                    }

                    if let Err(error) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
                        eprintln!("failed to open external URL {url}: {error}");
                    }
                    false
                })
                .on_page_load(|webview, payload| {
                    if payload.event() == PageLoadEvent::Finished
                        && webview.state::<LauncherState>().owns_url(payload.url())
                    {
                        if let Err(error) = webview.eval(EXTERNAL_LINK_BRIDGE) {
                            eprintln!("failed to install external link bridge: {error}");
                        }
                    }
                })
                .build(),
        )
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
