#![forbid(unsafe_code)]
#[cfg(not(target_os = "linux"))]
compile_error!("kvm-mcp supports Linux only");

use kvm_mcp::{
    comet::Comet,
    config::{Config, read_secret},
    server::{App, router},
};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("kvm-mcp: {error}");
        std::process::exit(1);
    }
}
async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let config = match args.next().as_deref() {
        Some("--version") => {
            println!("kvm-mcp {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--config") => PathBuf::from(args.next().ok_or("--config needs a file")?),
        _ => return Err("usage: kvm-mcp --config /path/config.toml | --version".into()),
    };
    if args.next().is_some() {
        return Err("unexpected arguments".into());
    }
    let config = Config::read(&config)?;
    let auth = read_secret(&config.auth_token_file)?;
    let timeout = Duration::from_secs(config.transfer_timeout_seconds);
    let backend = Arc::new(Comet::new(config.comet, timeout)?);
    let app = App::new(
        backend,
        config.public_url,
        &auth,
        config.max_upload_bytes,
        timeout,
    )?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| "cannot bind local listener")?;
    eprintln!("kvm-mcp ready (loopback listener; credentials and request URLs are not logged)");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = terminate.recv()=>{}}
        })
        .await
        .map_err(|_| "server failed")?;
    Ok(())
}
