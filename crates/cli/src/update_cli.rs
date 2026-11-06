//! `onicode update` — check for and apply a newer release, natively (the same
//! flow `edge/src/install.sh` performs: download → verify → symlink swap →
//! service restart). macOS app bundles swap the bundle instead; source builds
//! are report-only.

use anyhow::bail;
use onicode_update::{InstallKind, Offer};

/// `--check` prints the verdict and exits (nonzero when an update is available,
/// so scripts can gate on it). A macOS app follows its release channel
/// (`update-channel.json` in the data dir), as the app's own updater does.
pub async fn update(edge_url: &str, check_only: bool) -> anyhow::Result<()> {
    onicode_update::channel::set_data_dir(&super::paths::data_dir());
    let manifest = onicode_update::fetch_latest(edge_url).await?;
    let current = onicode_update::app_version();
    let running_engine = match onicode_update::detect_install() {
        InstallKind::MacApp { bundle } => onicode_update::bundle_engine_version(&bundle),
        _ => None,
    };
    match onicode_update::offer(
        &manifest,
        current,
        manifest.channel,
        running_engine.as_deref(),
        onicode_update::channel::macos_version(),
    ) {
        Offer::Newer | Offer::Switch => {}
        Offer::Held(why) => {
            println!("onicode {current}: {} isn't installable here. {why}", manifest.version);
            return Ok(());
        }
        Offer::Current => {
            println!(
                "onicode {current} is up to date (latest: {}).",
                manifest.version
            );
            return Ok(());
        }
    }
    println!("onicode {current} → {} available", manifest.version);
    if check_only {
        std::process::exit(1);
    }

    let install = onicode_update::detect_install();
    if let Some(blocker) = install.desktop_update_blocker() {
        bail!("{blocker}");
    }
    match install {
        InstallKind::Managed { app_root } if service_running() => {
            println!(
                "downloading {}…",
                onicode_update::headless_artifact(&manifest.version)
            );
            let data_dir = super::paths::data_dir();
            onicode_update::prepare_install(edge_url, &manifest, &app_root, &data_dir).await?;
            let from = onicode_update::installed_version(&InstallKind::Managed {
                app_root: app_root.clone(),
            })
            .unwrap_or_else(|| onicode_update::current_version().to_owned());
            let requested = onicode_update::transaction::request(
                &data_dir,
                onicode_update::transaction::Layout::Versioned { app_root: app_root.clone() },
                &from,
                &manifest.version,
                onicode_update::transaction::RestartMode::Service {
                    unit: onicode_update::service_unit(),
                },
            )?;
            if let Err(err) = onicode_update::restart_service() {
                onicode_update::transaction::fail_request(
                    &data_dir,
                    &requested.id,
                    "Onicode couldn't restart to install the update, so nothing changed.".into(),
                );
                bail!("couldn't restart the engine service: {err:#}");
            }
            println!("engine service restarting; waiting for {} to prove it's ready…", manifest.version);
            wait_for_outcome(&data_dir, &requested).await
        }
        InstallKind::Managed { app_root } => {
            println!(
                "downloading {}…",
                onicode_update::headless_artifact(&manifest.version)
            );
            onicode_update::stage_headless(edge_url, &manifest, &app_root).await?;
            onicode_update::apply_headless(&app_root, &manifest.version)?;
            println!(
                "installed {} (current → {})",
                app_root.join(&manifest.version).display(),
                manifest.version
            );
            match onicode_update::restart_service() {
                Ok(()) => println!("engine service restarted."),
                Err(err) => println!(
                    "note: service restart failed ({err:#}) — restart the engine manually to finish."
                ),
            }
            Ok(())
        }
        InstallKind::MacApp { bundle } => {
            println!(
                "downloading {}…",
                onicode_update::mac_app_artifact(&manifest.version)
            );
            let data_dir = super::paths::data_dir();
            let staged = onicode_update::stage_mac_app(edge_url, &manifest, &data_dir).await?;
            onicode_update::apply_mac_app(&staged, &bundle)?;
            println!("updated {} — relaunch Onicode to finish.", bundle.display());
            Ok(())
        }
        #[cfg(windows)]
        InstallKind::WindowsPortable { directory } => {
            let staged = onicode_update::windows::stage(edge_url, &manifest, &directory).await?;
            onicode_update::windows::apply(&staged, &directory, false)?;
            println!(
                "updated to {} — relaunch Onicode to finish.",
                manifest.version
            );
            Ok(())
        }
        InstallKind::Unmanaged => {
            bail!(
                "this binary is not update-managed (source build or hand-copied).\n\
                 Linux: curl -fsSL https://zeron.sh/install.sh | sh, or run install.sh from the release tarball\n\
                 macOS: download the new Onicode.app dmg, or rebuild from source.\n\
                 Windows: install with the Onicode setup .exe from {}, or rebuild from source.",
                onicode_update::LATEST_RELEASE_PAGE
            )
        }
    }
}

/// Whether the installed service is up: it's what restarts into the update and
/// what can go back. Without one there's nothing running to protect, and the new
/// version simply starts next time.
fn service_running() -> bool {
    cfg!(target_os = "linux")
        && std::process::Command::new("systemctl")
            .args(["--user", "--quiet", "is-active", &onicode_update::service_unit()])
            .status()
            .is_ok_and(|status| status.success())
}

/// Report how the update went: it may yet go back, so "restarted" says nothing.
async fn wait_for_outcome(
    data_dir: &std::path::Path,
    requested: &onicode_update::transaction::Transaction,
) -> anyhow::Result<()> {
    use onicode_update::UpdateState;
    // The trial's own deadline, plus time to stop and go back.
    let limit = std::time::Duration::from_secs(requested.deadline_secs + 120);
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        let report = onicode_update::transaction::report(data_dir).filter(|r| r.id == requested.id);
        match report.as_ref().map(|report| report.state) {
            Some(UpdateState::Updated) => {
                println!("updated to {}.", requested.target);
                return Ok(());
            }
            Some(UpdateState::RolledBack | UpdateState::Failed) => {
                let reason = report.and_then(|report| report.reason).unwrap_or_default();
                bail!("the update didn't go through. {reason}");
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
        }
    }
    bail!(
        "the update hasn't finished after {} min. Check `onicode daemon status`.",
        limit.as_secs() / 60
    )
}
