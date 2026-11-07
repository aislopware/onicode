use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::{AuthAction, Profile, ProfileExt as _, login_arguments};
use crate::process;
use crate::provider::{self, Kind};

/// The CLI opens its browser and owns credential writes. Never relay its URLs,
/// OAuth codes or tokens to logs, clipboard, or the native account metadata.
pub fn authenticate(
    kind: Kind,
    profile: &Profile,
    action: AuthAction,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let args = login_arguments(kind, action)
        .ok_or("This provider does not offer a single browser sign-in flow")?;
    let binary =
        provider::resolve(kind).ok_or_else(|| format!("{} CLI not installed", kind.label()))?;
    let mut command = Command::new(binary);
    crate::scm::shell::apply_gui_env(&mut command);
    profile.apply(kind, &mut command)?;
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(home) = super::home() {
        command.current_dir(home);
    }
    process::isolate(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start {} sign-in: {e}", kind.label()))?;
    let _registration = process::register(child.id());
    let _account_process = super::track(child.id(), kind, profile);
    let mut stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        // Keep draining the pipe even after the bounded diagnostic tail fills.
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if bytes.len() > 65_536 {
                bytes.drain(..bytes.len() - 65_536);
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    let deadline = Instant::now()
        + Duration::from_secs(if action == AuthAction::SignIn {
            600
        } else {
            30
        });
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Sign-in cancelled".into());
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "{} sign-in timed out. Please try again.",
                kind.label()
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(_)) => {
                let detail = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
                let message = safe_detail(&detail);
                break Err(if message.is_empty() {
                    format!("{} sign-in exited unexpectedly", kind.label())
                } else {
                    message
                });
            }
            Err(e) => break Err(format!("Could not complete sign-in: {e}")),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
        }
    };
    process::terminate(&mut child);
    drop(rx);
    let _ = reader.join();
    if result.is_ok() {
        crate::usage::forget(kind, profile);
    }
    result
}
/// Removing an account promises its stored credentials go. Claude Code keeps them where only it
/// should reach (its config dir and Keychain item), so its published logout deletes them.
pub fn sign_out_for_removal(kind: Kind, profile: &Profile) -> Result<(), String> {
    if kind != Kind::Claude || provider::resolve(kind).is_none() {
        return Ok(());
    }
    authenticate(
        kind,
        profile,
        AuthAction::SignOut,
        Arc::new(AtomicBool::new(false)),
    )
    .map_err(|error| format!("Could not sign out of Claude Code: {error}"))
}
fn safe_detail(value: &str) -> String {
    let last = value
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("");
    let urls = regex::Regex::new(r"(?i)https?://\S+").unwrap();
    urls.replace_all(last, "sign-in link")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_auth_commands_and_error_details_match_the_provider_flow() {
        assert_eq!(
            login_arguments(Kind::Claude, AuthAction::SignIn),
            Some(["auth", "login"].as_slice())
        );
        assert_eq!(
            login_arguments(Kind::Codex, AuthAction::SignOut),
            Some(["logout"].as_slice())
        );
        assert_eq!(
            login_arguments(Kind::Grok, AuthAction::SignIn),
            Some(["login", "--oauth"].as_slice())
        );
        assert!(login_arguments(Kind::Pi, AuthAction::SignIn).is_none());
        assert_eq!(
            safe_detail("first line\n Complete at https://example.test/?code=private\n"),
            "Complete at sign-in link"
        );
        assert_eq!(safe_detail(&"a".repeat(300)).len(), 240);
    }
}
