//! vendor code-signature checks for archives that onicode installs without a
//! digest pinned in its own source (a registry release newer than the pin).
//!
//! every extracted file must carry a valid signature chaining to the vendor's
//! platform identity: google's apple developer id team on macOS. linux builds
//! are unsigned, so there the pinned digest remains the only accepted proof.

use std::path::{Path, PathBuf};

use crate::HarnessError;

#[cfg(target_os = "macos")]
const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// google llc's apple developer id team.
#[cfg(target_os = "macos")]
const GOOGLE_APPLE_TEAM: &str = "EQHXZ8M8AV";

/// whether this platform can prove a download's origin without a pinned digest.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// verify that every file under `dir` is signed by google.
pub async fn verify_google_signed(dir: &Path) -> Result<(), HarnessError> {
    let files = regular_files(dir)?;
    if files.is_empty() {
        return Err(unsigned("the archive contains no files"));
    }
    for file in files {
        verify_file(&file).await?;
    }
    Ok(())
}

fn regular_files(dir: &Path) -> Result<Vec<PathBuf>, HarnessError> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err(unsigned(&format!(
                    "{} is not a regular file",
                    entry.path().display()
                )));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn unsigned(detail: &str) -> HarnessError {
    HarnessError::Install(format!("code signature verification failed: {detail}"))
}

#[cfg(target_os = "macos")]
async fn verify_file(file: &Path) -> Result<(), HarnessError> {
    // developer id leaf and intermediate, issued by apple, to google's team.
    let requirement = format!(
        "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{GOOGLE_APPLE_TEAM}\""
    );
    let mut command = crate::process::Command::new("/usr/bin/codesign");
    command
        .args(["--verify", "--strict", "-R"])
        .arg(requirement)
        .arg(file);
    let output = run(command, file).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(unsigned(&format!(
        "{} is not signed by Google: {}",
        file_name(file),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(not(target_os = "macos"))]
async fn verify_file(file: &Path) -> Result<(), HarnessError> {
    Err(unsigned(&format!(
        "{} cannot be verified on this platform",
        file_name(file)
    )))
}

#[cfg(target_os = "macos")]
async fn run(
    mut command: crate::process::Command,
    file: &Path,
) -> Result<std::process::Output, HarnessError> {
    command
        .stdin(crate::process::Stdio::null())
        .stdout(crate::process::Stdio::piped())
        .stderr(crate::process::Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(VERIFY_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            unsigned(&format!(
                "checking {} took longer than {}s",
                file_name(file),
                VERIFY_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| unsigned(&format!("could not run the verifier: {error}")))
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsigned_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server"), "#!/bin/sh\nexit 0\n").unwrap();
        let error = verify_google_signed(dir.path()).await.unwrap_err();
        assert!(error.to_string().contains("code signature"), "{error}");
    }

    #[tokio::test]
    async fn empty_archives_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(verify_google_signed(dir.path()).await.is_err());
    }

    /// apple's own binaries are signed, but not by google's team.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn binaries_from_another_team_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy("/bin/ls", dir.path().join("ls")).unwrap();
        let error = verify_google_signed(dir.path()).await.unwrap_err();
        assert!(
            error.to_string().contains("not signed by Google"),
            "{error}"
        );
    }

    /// set `ONICODE_TEST_GOOGLE_SIGNED_DIR` to an extracted antigravity archive.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn google_signed_archive_is_accepted() {
        let Some(dir) = std::env::var_os("ONICODE_TEST_GOOGLE_SIGNED_DIR") else {
            return;
        };
        verify_google_signed(Path::new(&dir)).await.unwrap();
    }
}
