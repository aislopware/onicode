//! The engine's local tokens: who may drive it over its Unix socket ([`crate::serve_unix`]).
//!
//! The socket sits in the owner-only data dir and every peer must run as the engine's own uid,
//! but a process of that user is not yet the person: every engine start mints two fresh 32-byte
//! tokens, writes them to `{data_dir}/ipc-token` and `{data_dir}/ipc-token-agent` (owner-only,
//! atomic replace), and a connection's first line must carry one. Clients read the file at dial
//! time, so they keep working across engine restarts.
//!
//! Two tokens, two authorities ([`Scope`], [`crate::Caller`]): the full token is the app's; the
//! agent-scoped token is what the engine hands the `onicode mcp` servers it injects into agents'
//! runs, and the engine answers it only the calls its protocol marks as an agent's
//! (`Call::AGENT`). A client given `$ONICODE_IPC_TOKEN_FILE` uses that file alone: it never falls
//! back to the full token. In-process transports never see any of this.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// File name of the token inside the engine's data dir.
pub const TOKEN_FILE_NAME: &str = "ipc-token";
/// Explicit token value for a client (wins over every file).
pub const TOKEN_ENV: &str = "ONICODE_IPC_TOKEN";
/// Path of a token file for a client (the engine injects this into `onicode mcp`).
pub const TOKEN_FILE_ENV: &str = "ONICODE_IPC_TOKEN_FILE";
/// File name of the agent-scoped token inside the engine's data dir.
pub const AGENT_TOKEN_FILE_NAME: &str = "ipc-token-agent";

/// `{data_dir}/ipc-token`.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE_NAME)
}

/// `{data_dir}/ipc-token-agent`.
pub fn agent_token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(AGENT_TOKEN_FILE_NAME)
}

/// The tokens one engine start serves: the full one and the agent-scoped one.
#[derive(Clone)]
pub struct IpcTokens {
    pub full: String,
    pub agent: String,
}

/// What a token grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The app: every call.
    Full,
    /// An agent's `onicode mcp`: only the calls marked as an agent's.
    Agent,
}

impl IpcTokens {
    /// Fresh tokens, not written anywhere (tests, in-process servers).
    pub fn generate() -> io::Result<Self> {
        Ok(Self {
            full: generate_token()?,
            agent: generate_token()?,
        })
    }
}

/// Mint both tokens and publish them at `{data_dir}/ipc-token` and
/// `{data_dir}/ipc-token-agent`.
pub fn issue_tokens(data_dir: &Path) -> io::Result<IpcTokens> {
    let agent = generate_token()?;
    write_token(&agent_token_path(data_dir), &agent)?;
    let full = issue_token(data_dir)?;
    Ok(IpcTokens { full, agent })
}

/// 32 bytes from the OS CSPRNG, lowercase hex (64 chars).
pub fn generate_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| io::Error::other(format!("no OS randomness for the IPC token: {e}")))?;
    Ok(hex(&bytes))
}

/// Mint a fresh token and publish it at `{data_dir}/ipc-token`. Called by the
/// engine on every start that serves its socket; the previous token (and
/// every client still holding it) stops working.
pub fn issue_token(data_dir: &Path) -> io::Result<String> {
    let token = generate_token()?;
    write_token(&token_path(data_dir), &token)?;
    Ok(token)
}

/// Atomically replace `path` with `token`: a uniquely named sibling created
/// exclusively (never follows a planted symlink) with mode 0600, fsynced, then
/// renamed over the target. A reader sees the old token or the new one, never
/// a torn write.
pub fn write_token(path: &Path, token: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let suffix = generate_token()?;
    let tmp = dir.join(format!(".{TOKEN_FILE_NAME}.{}.tmp", &suffix[..16]));
    let written = write_exclusive(&tmp, token).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Create `path` (which must not exist) owner-only and durably write `token`.
fn write_exclusive(path: &Path, token: &str) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    // umask can only clear bits, but be explicit about the contract.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// The token stored at `path`; `None` when the file is missing or empty.
pub fn read_token(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw.trim().to_owned()).filter(|t| !t.is_empty())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Every token a client should try, best first, deduplicated: the
/// `$ONICODE_IPC_TOKEN_FILE` file alone when it's set (an agent's MCP server),
/// else the `$ONICODE_IPC_TOKEN` value, then `{data_dir}/ipc-token`. Read at call time — after an
/// engine restart the file already holds the new token. Empty when nothing is configured: no
/// engine has started yet.
pub fn client_tokens(data_dir: Option<&Path>) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut push = |token: Option<String>| {
        if let Some(token) = token.map(|t| t.trim().to_owned())
            && !token.is_empty()
            && !tokens.contains(&token)
        {
            tokens.push(token);
        }
    };
    if let Some(file) = std::env::var_os(TOKEN_FILE_ENV).filter(|f| !f.is_empty()) {
        // The file it was given is its authority (an agent's MCP server:
        // the scoped token). Never reach past it for another token.
        push(read_logged(Path::new(&file)));
        return tokens;
    }
    push(std::env::var(TOKEN_ENV).ok());
    if let Some(dir) = data_dir {
        push(read_logged(&token_path(dir)));
    }
    tokens
}

/// The tokens an agent's MCP server tries: the `$ONICODE_IPC_TOKEN_FILE` file
/// when set, else `{data_dir}/ipc-token-agent`. Never the full token.
pub fn agent_client_tokens(data_dir: Option<&Path>) -> Vec<String> {
    let file = std::env::var_os(TOKEN_FILE_ENV)
        .filter(|f| !f.is_empty())
        .map(PathBuf::from)
        .or_else(|| data_dir.map(agent_token_path));
    file.and_then(|file| read_logged(&file))
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .into_iter()
        .collect()
}

fn read_logged(path: &Path) -> Option<String> {
    read_token(path).unwrap_or_else(|err| {
        tracing::warn!(path = %path.display(), error = %err, "rpc: cannot read the IPC token");
        None
    })
}

/// What `presented` grants against `tokens`, `None` for neither. Both compared, always, in
/// constant time (the length is not secret: every token is 64 hex chars).
pub(crate) fn token_scope(tokens: &IpcTokens, presented: &[u8]) -> Option<Scope> {
    let presented = presented.trim_ascii();
    let full = constant_time_eq(presented, tokens.full.as_bytes());
    let agent = constant_time_eq(presented, tokens.agent.as_bytes());
    match (full, agent) {
        (true, _) => Some(Scope::Full),
        (false, true) => Some(Scope::Agent),
        _ => None,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_fresh_32_byte_hex() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn issue_replaces_the_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let first = issue_token(dir.path()).unwrap();
        assert_eq!(
            read_token(&token_path(dir.path())).unwrap(),
            Some(first.clone())
        );
        let second = issue_token(dir.path()).unwrap();
        assert_ne!(first, second, "every engine start rotates the token");
        assert_eq!(read_token(&token_path(dir.path())).unwrap(), Some(second));
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "token file must be owner-only");
        }
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![TOKEN_FILE_NAME.to_string()]);
    }

    #[test]
    fn issue_replaces_a_planted_symlink_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        std::os::unix::fs::symlink(&victim, token_path(dir.path())).unwrap();
        issue_token(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(
            !std::fs::symlink_metadata(token_path(dir.path()))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn missing_or_empty_token_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_token(&token_path(dir.path())).unwrap(), None);
        std::fs::write(token_path(dir.path()), "\n").unwrap();
        assert_eq!(read_token(&token_path(dir.path())).unwrap(), None);
    }

    #[test]
    fn token_matching_is_exact() {
        let tokens = IpcTokens {
            full: "ab".repeat(32),
            agent: "cd".repeat(32),
        };
        assert_eq!(
            token_scope(&tokens, "ab".repeat(32).as_bytes()),
            Some(Scope::Full)
        );
        assert_eq!(
            token_scope(&tokens, format!(" {} ", "cd".repeat(32)).as_bytes()),
            Some(Scope::Agent)
        );
        assert_eq!(token_scope(&tokens, b""), None);
        assert_eq!(token_scope(&tokens, "ab".repeat(31).as_bytes()), None);
        assert_eq!(
            token_scope(&tokens, format!("{}cd", "ab".repeat(31)).as_bytes()),
            None
        );
        assert_eq!(
            token_scope(&tokens, format!("Bearer {}", "ab".repeat(32)).as_bytes()),
            None
        );
    }
}
