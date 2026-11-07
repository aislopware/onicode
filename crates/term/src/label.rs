//! What a terminal's tab is labelled with: the command holding its foreground, unless it is the
//! shell itself (monocode's `pty.rs` `foreground_label`).

/// Interpreters name the script they run: `node vite.js` reads `vite.js`.
#[must_use]
pub fn command_label(args: &str) -> Option<String> {
    let mut parts = args.split_whitespace();
    let exe = parts.next()?;
    let base = std::path::Path::new(exe).file_name()?.to_str()?.to_owned();
    let base = base.trim_start_matches('-').to_owned();
    if matches!(
        base.as_str(),
        "node" | "nodejs" | "python" | "python3" | "ruby" | "deno" | "bun"
    ) {
        for part in parts {
            if part.starts_with('-') {
                continue;
            }
            let name = std::path::Path::new(part).file_name()?.to_str()?;
            if !name.starts_with('-') {
                return Some(name.to_owned());
            }
        }
    }
    Some(base)
}

/// A shell waiting at its prompt labels no tab.
#[must_use]
pub fn is_shell_name(name: &str) -> bool {
    matches!(
        name,
        "zsh" | "bash" | "sh" | "fish" | "nu" | "dash" | "ksh" | "tcsh" | "zsh5"
    )
}

/// The label of the process group `pgrp`, read with `ps`, unless it is the shell.
#[must_use]
pub fn process_label(pgrp: i32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pgrp.to_string(), "-o", "args="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let label = command_label(String::from_utf8_lossy(&output.stdout).trim())?;
    (!is_shell_name(&label)).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_labels_name_the_script_an_interpreter_runs() {
        assert_eq!(
            command_label("node /x/node_modules/.bin/vite --port 3000").as_deref(),
            Some("vite")
        );
        assert_eq!(
            command_label("/usr/bin/python3 -m http.server").as_deref(),
            Some("http.server")
        );
        assert_eq!(
            command_label("cargo watch -x test").as_deref(),
            Some("cargo")
        );
        assert_eq!(command_label("-zsh").as_deref(), Some("zsh"));
        assert!(is_shell_name("zsh"));
    }
}
