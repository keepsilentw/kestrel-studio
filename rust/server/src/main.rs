mod admin;
mod agent;
mod auth;
mod bailian;
mod common;
mod config;
mod conversation;
mod db;
mod env_file;
mod error;
mod http;
mod media;
mod sse;
mod state;
mod task;
mod voice;
mod net;
#[cfg(test)]
mod test_utils;
mod view;

use std::error::Error;

use crate::config::AppConfig;
use crate::state::AppState;

/// ANSI escapes only when stderr really is a terminal.
///
/// `tracing_subscriber`'s `ansi` feature is on by default and the fmt layer does
/// **not** look at the stream, so a container's log file used to be full of
/// escape sequences: `method` and `=` were separated by `\e[0m`, which made
/// `grep 'method='` on `docker logs` return nothing and read as "the access log
/// is not firing". Docker captures stdout without a TTY, so this returns false
/// in production and the log is plain text; a developer at a terminal keeps the
/// colours. Docs: docs/verification.md §13.8, docs/deployment.md §2.1.
fn log_uses_ansi() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// One process, SSR plus streaming, no separate front-end service — the shape the
/// Nest server had, kept on purpose (it is what makes an 8 GB host work).
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Before any read of the environment: the container gets its values from
    // docker's env_file and has no .env of its own, so this only ever overlays a
    // local developer file, and never over something already exported.
    let cwd = std::env::current_dir()?;
    let overlay = env_file::load_env_file(&cwd);

    tracing_subscriber::fmt()
        .with_ansi(log_uses_ansi())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app_config = match AppConfig::load() {
        Ok(loaded) => loaded,
        Err(message) => {
            eprintln!("configuration error: {message}");
            std::process::exit(1);
        }
    };
    if overlay.present {
        tracing::info!(
            path = %overlay.path.display(),
            applied = overlay.applied,
            skipped = overlay.skipped,
            "overlaid .env (existing environment variables win)"
        );
    }
    let port = app_config.port;
    let is_production = app_config.is_production;
    let super_admin = app_config.super_admin.clone();

    // The provider key is resolved once at boot; generation paths answer 503 when
    // it is absent. See bailian::token for the precedence.
    let api_key = bailian::token::resolve_key().await;
    match api_key.as_deref() {
        Some(_) => tracing::info!("Bailian credentials resolved"),
        None => tracing::warn!("{}", bailian::token::MISSING_KEY_MESSAGE),
    }

    let pool = db::open(&app_config).await?;
    let state = AppState::new(app_config, pool, api_key);

    // Idempotent: a restart never resets a password that was changed.
    if auth::seed::ensure_admin_user(&state.pool, is_production).await? {
        tracing::info!("seeded the local convenience account");
    }
    if auth::seed::ensure_super_admin(&state.pool, super_admin.as_ref()).await? {
        tracing::info!("seeded the bootstrap super admin");
    }

    // A hard kill (restart, crash) stops a running turn with its assistant
    // placeholder empty. Ending those rows here — before any turn can read
    // history — keeps the orphaned-user-message hazard the turn loop's finalizer
    // guards against from surviving a process boundary.
    let abandoned = state.conversations.abandon_stale_placeholders().await?;
    if abandoned > 0 {
        tracing::info!(abandoned, "finalized assistant rows left by a previous process");
    }

    // Expired logins: cleared at boot and then on a quarter-hour tick, which is
    // what the express-session store did with its own interval.
    tracing::info!(cleared = auth::session::sweep(&state.pool).await?, "expired sessions");
    let mut expiry = tokio::time::interval(std::time::Duration::from_secs(15 * 60));
    let sweep_state = state.clone();
    tokio::spawn(async move {
        loop {
            expiry.tick().await;
            match auth::session::sweep(&sweep_state.pool).await {
                Ok(0) => {}
                Ok(cleared) => tracing::info!(cleared, "cleared expired sessions"),
                Err(error) => tracing::warn!(%error, "session sweep failed"),
            }
        }
    });

    // Video renders outlive a turn: the tool submits, this worker polls the
    // provider and mirrors the bytes the moment the render succeeds.
    let worker_state = state.clone();
    tokio::spawn(async move {
        task::worker::TaskWorker::new(worker_state).run().await;
    });

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    // The bundle is logged because it is the one setting an operator cannot read off a
    // page: which client users got is the first question after a grey release.
    tracing::info!(
        client = state.config.client_bundle.as_str(),
        "kestrel-studio listening on http://localhost:{port}"
    );
    axum::serve(listener, http::router::build(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("signal received, closing the listener");
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    use tracing::span;
    use tracing::Level;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::EnvFilter;

    // fmt 层往这里写。两个测试各自起线程，所以整段渲染串行化。
    static CAPTURE: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    static RENDER: Mutex<()> = Mutex::new(());

    fn captured() -> &'static Mutex<Vec<u8>> {
        CAPTURE.get_or_init(|| Mutex::new(Vec::new()))
    }

    #[derive(Clone)]
    struct Sink;

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            captured().lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Sink {
        type Writer = Sink;

        fn make_writer(&'a self) -> Sink {
            Sink
        }
    }

    /// 复刻访问日志那一行的形状：tower_http 把 `method` / `uri` 挂在**父 span** 上，
    /// 事件本身只有消息，所以渲染出来是
    /// `request{method=GET uri=/login?probe=1}: … started processing request`。
    fn render_one_line(ansi: bool) -> String {
        let _serialised = RENDER.lock().expect("render lock");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(ansi)
            .with_env_filter(EnvFilter::new("debug"))
            .with_writer(Sink)
            .without_time()
            .with_target(false)
            .finish();

        captured().lock().expect("capture lock").clear();
        tracing::subscriber::with_default(subscriber, || {
            // `%` on purpose: that is how tower_http records these two fields
            // (`method = %request.method()`), and it is what makes the plain-text
            // line read `method=GET` rather than `method="GET"`.
            let request = span!(
                Level::DEBUG,
                "request",
                method = %"GET",
                uri = %"/login?probe=1",
            );
            let _entered = request.enter();
            tracing::debug!("started processing request");
        });
        String::from_utf8(captured().lock().expect("capture lock").clone()).expect("utf-8")
    }

    #[test]
    fn a_non_terminal_log_is_plain_text_so_access_lines_are_greppable() {
        let plain = render_one_line(false);
        assert!(plain.contains("method=GET"), "字段名与 = 之间不该有东西：{plain:?}");
        assert!(plain.contains("uri=/login?probe=1"), "查询串要留在日志里：{plain:?}");
        assert!(!plain.contains('\u{1b}'), "容器里不该有 ANSI：{plain:?}");
    }

    #[test]
    fn ansi_mode_is_what_broke_the_ops_grep() {
        let coloured = render_one_line(true);
        assert!(coloured.contains('\u{1b}'), "开着 ANSI 时应带转义序列：{coloured:?}");
        // 这正是 2026-11-02 第一次取证 0 命中的原因，也是 §13.8 的根因。
        assert!(
            !coloured.contains("method=GET"),
            "转义序列把字段名与 = 隔开，朴素 grep 因此抓不到：{coloured:?}"
        );
    }
}
