use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use clap::{Parser, Subcommand};
use hik_adapter::{
    api::ApiClient,
    config::{AppConfig, Credentials},
    error::{AdapterError, ErrorCategory},
    recording::recording_name,
    runtime::{AdapterState, VtmCameraJob, join_workers, spawn_camera_workers},
    server::{ServerState, router},
};

#[derive(Parser)]
#[command(
    name = "hik-adapter",
    version,
    about = "Hik-Connect VTM feasibility adapter"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    List {
        #[arg(long)]
        config: PathBuf,
    },
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();
    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("error: {:?}: {}", error.category, error.message);
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<(), AdapterError> {
    match cli.command {
        Command::List { config } => list(&config),
        Command::Run { config, overwrite } => run(&config, overwrite).await,
    }
}

fn list(path: &Path) -> Result<(), AdapterError> {
    let config = AppConfig::load(path)?;
    let credentials = Credentials::from_config(&config.hik_connect)?;
    let client = ApiClient::new(
        &config.hik_connect.api_base_url,
        Duration::from_secs(config.hik_connect.request_timeout_seconds),
    )?;
    let session = client.login(&credentials)?;
    let cameras = client.list_cameras(&session)?;
    println!("INDEX\tCAMERA\tDEVICE\tSERIAL\tCHANNEL");
    for camera in cameras {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            camera.index, camera.name, camera.device_name, camera.serial, camera.channel
        );
    }
    Ok(())
}

async fn run(path: &Path, overwrite: bool) -> Result<(), AdapterError> {
    let config = AppConfig::load(path)?;
    let credentials = Credentials::from_config(&config.hik_connect)?;
    let api = ApiClient::new(
        &config.hik_connect.api_base_url,
        Duration::from_secs(config.hik_connect.request_timeout_seconds),
    )?;
    let session = api.login(&credentials)?;
    tracing::debug!(
        api_host = session.api_base_url.host_str().unwrap_or("unknown"),
        "authenticated against regional Hik-Connect API"
    );
    let inventory = api.list_cameras(&session)?;
    let allowlist = &config.recording.camera_serials;
    let cameras = if allowlist.is_empty() {
        inventory
            .into_iter()
            .take(config.recording.max_cameras)
            .collect::<Vec<_>>()
    } else {
        let mut selected = Vec::new();
        for serial in allowlist {
            let matches = inventory
                .iter()
                .filter(|camera| &camera.serial == serial)
                .cloned()
                .collect::<Vec<_>>();
            if matches.is_empty() {
                selected.push(hik_adapter::model::Camera {
                    index: selected.len() + 1,
                    name: serial.clone(),
                    device_name: "direct VTM target".to_owned(),
                    serial: serial.clone(),
                    channel: 1,
                });
            } else {
                selected.extend(matches);
            }
        }
        selected.truncate(config.recording.max_cameras);
        selected
    };
    if cameras.is_empty() {
        return Err(AdapterError::new(
            ErrorCategory::ConfigurationInvalid,
            "no cameras selected",
        ));
    }
    prepare_output(&config.recording.output_dir, &cameras, overwrite)?;
    let state = AdapterState::default();
    state.register(&cameras);
    let bind = config.server.bind.parse().map_err(|_| {
        AdapterError::new(
            ErrorCategory::ConfigurationInvalid,
            "invalid server bind address",
        )
    })?;
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|_| {
        AdapterError::new(
            ErrorCategory::ConfigurationInvalid,
            "cannot bind HTTP server",
        )
    })?;
    println!(
        "playback: http://{}/",
        listener.local_addr().unwrap_or(bind)
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let app = router(ServerState {
        adapter: state.clone(),
        output_dir: config.recording.output_dir.clone(),
    });
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                while !server_shutdown.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await;
    });
    let signal_shutdown = Arc::clone(&shutdown);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_shutdown.store(true, Ordering::Relaxed);
        }
    });
    let job = VtmCameraJob {
        api,
        session,
        config: config.recording.clone(),
        state: state.clone(),
        socket_timeout: Duration::from_secs(config.hik_connect.request_timeout_seconds),
        no_media_timeout: Duration::from_secs(10),
    };
    let handles = spawn_camera_workers(cameras, state, job, Arc::clone(&shutdown))?;
    let results = tokio::task::spawn_blocking(move || join_workers(handles))
        .await
        .map_err(|_| AdapterError::new(ErrorCategory::ValidationFailed, "worker join failed"))?;
    let completed = results
        .iter()
        .filter(|result| result.result.is_ok())
        .count();
    let failed = results.len() - completed;
    println!("workers finished: {completed} completed, {failed} failed");
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _ = server.await;
    Ok(())
}

fn prepare_output(
    directory: &Path,
    cameras: &[hik_adapter::model::Camera],
    overwrite: bool,
) -> Result<(), AdapterError> {
    fs::create_dir_all(directory).map_err(|_| {
        AdapterError::new(
            ErrorCategory::ConfigurationInvalid,
            "cannot create recording directory",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|_| {
            AdapterError::new(
                ErrorCategory::ConfigurationInvalid,
                "cannot secure recording directory",
            )
        })?;
    }
    if overwrite {
        for camera in cameras {
            let final_path = directory.join(recording_name(&camera.serial, camera.channel));
            let part_path = final_path.with_file_name(format!(
                "{}.part.mp4",
                final_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("camera")
            ));
            for path in [final_path, part_path] {
                if path.is_file() {
                    fs::remove_file(path).map_err(|_| {
                        AdapterError::new(
                            ErrorCategory::ConfigurationInvalid,
                            "cannot replace recording",
                        )
                    })?;
                }
            }
        }
    }
    Ok(())
}
