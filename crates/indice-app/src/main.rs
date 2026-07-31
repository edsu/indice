//! indice desktop app shell (rustyweb-desktop-app-i91t.2).
//!
//! Double-click launches this binary, which runs the normal `indice` axum
//! server in-process on an OS-assigned localhost port, then opens a native
//! window whose embedded webview points at `http://127.0.0.1:<port>/`. The CLI
//! (`indice index` / `indice serve`) is unchanged; this is purely an additional
//! front door for non-technical users.
//!
//! Why a real http origin (not `tauri://`): ReplayWeb.page's service worker only
//! registers on a genuine http(s) origin. See the Phase-0 spike findings on the
//! epic for the macOS WKWebView constraints (ATS + bundle identity).

use std::path::PathBuf;

use anyhow::Context;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Data directory that holds `archive/` and `index/`. Defaults to the platform
/// data dir (`~/Library/Application Support/…` on macOS, `%APPDATA%\…` on
/// Windows); `INDICE_HOME` overrides it (handy for pointing the app at an
/// existing CLI-built home, and for tests).
fn data_home() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("INDICE_HOME") {
        let home = PathBuf::from(dir);
        std::fs::create_dir_all(&home)
            .with_context(|| format!("creating INDICE_HOME {}", home.display()))?;
        return Ok(home);
    }
    let dirs = directories::ProjectDirs::from("org", "indice", "indice")
        .context("could not determine a platform data directory")?;
    let home = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating data home {}", home.display()))?;
    Ok(home)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "indice_lib=info,indice_app=info".into()),
        )
        .init();

    let home = data_home()?;
    tracing::info!(home = %home.display(), "indice app data home");

    // A multi-threaded runtime drives the in-process server on its own worker
    // threads; we keep it alive for the whole process (Tauri owns the main
    // thread's event loop).
    let rt = tokio::runtime::Runtime::new().context("building tokio runtime")?;

    // Bind :0 so concurrent instances never collide, then read the assigned port
    // back before opening the window.
    let listener = rt
        .block_on(async { tokio::net::TcpListener::bind("127.0.0.1:0").await })
        .context("binding localhost listener")?;
    let port = listener.local_addr()?.port();
    tracing::info!(port, "serving indice in-process");

    let server_home = home.clone();
    rt.spawn(async move {
        if let Err(e) = indice_lib::server::serve_on_listener(listener, &server_home, None).await {
            tracing::error!(error = %e, "in-process server exited with error");
        }
    });

    let url = format!("http://127.0.0.1:{port}/");

    tauri::Builder::default()
        // macOS keeps apps alive after the last window closes; this is a
        // single-window utility, so closing the window quits the process (which
        // also tears down the in-process server and its runtime).
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                window.app_handle().exit(0);
            }
        })
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("indice")
                .inner_size(1200.0, 800.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .context("running the indice desktop app")?;

    // `run` blocks until the app quits; keep the runtime alive until then.
    drop(rt);
    Ok(())
}
