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
    tracing::info!("kestrel-studio listening on http://localhost:{port}");
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
