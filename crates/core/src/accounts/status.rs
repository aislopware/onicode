//! What Claude Code's published door says about one account's sign-in: `claude auth status
//! --json`, run under the account's profile. Its credential files and Keychain item stay Claude
//! Code's own: nothing here reads them.
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Profile, ProfileExt as _};
use crate::process;
use crate::provider::{self, Kind};

/// What `claude auth status --json` printed for `profile`, whatever it exited with: it reports a
/// signed-out account and exits non-zero. `Err` when Claude Code is not installed, does not
/// start, or does not answer within 15 seconds.
pub fn claude_auth_status(profile: &Profile) -> Result<String, String> {
    let binary = provider::resolve(Kind::Claude).ok_or("Claude Code CLI not installed")?;
    let mut command = Command::new(binary);
    crate::scm::shell::apply_gui_env(&mut command);
    profile.apply(Kind::Claude, &mut command)?;
    command
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = super::home() {
        command.current_dir(home);
    }
    process::isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start Claude Code: {error}"))?;
    let _registration = process::register(child.id());
    let _account_process = super::track(child.id(), Kind::Claude, profile);
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(65_536).read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            Ok(None) if Instant::now() >= deadline => {
                break Err("Claude Code did not report its sign-in in time".to_owned());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    process::terminate(&mut child);
    let printed = reader.join().unwrap_or_default();
    status.map(|_| printed)
}
