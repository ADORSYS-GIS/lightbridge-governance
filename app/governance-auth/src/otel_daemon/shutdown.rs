//! Resolves once SIGINT or SIGTERM arrives, ending the accept loop. Split out
//! of `mod.rs` purely for the LoC ceiling.

/// Resolves once SIGINT or SIGTERM arrives, ending the accept loop.
pub(super) async fn signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to install Ctrl-C handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        // SIGTERM is how systemd/launchd stop the daemon (#S3). If the handler
        // cannot be installed, SIGTERM's *default* action still terminates the
        // process, so there is nothing lost by not intercepting it -- await
        // forever and let the OS kill us, rather than panic inside the only
        // path that runs at shutdown time.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                let _ = stream.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to install the SIGTERM handler; relying on the OS default termination");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
