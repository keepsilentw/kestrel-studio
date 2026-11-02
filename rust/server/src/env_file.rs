use std::path::{Path, PathBuf};

/// What a load attempt did, so callers and tests can assert without logging.
#[derive(Debug, PartialEq, Eq)]
pub struct EnvFileLoadResult {
    pub path: PathBuf,
    /// False when the file does not exist, which is the normal case in a container.
    pub present: bool,
    /// Keys written into the process environment.
    pub applied: usize,
    /// Lines dropped: no key, no `=`, or the key was already in the environment.
    pub skipped: usize,
}

fn unquote(value: &str) -> String {
    let bytes: Vec<char> = value.chars().collect();
    let quoted = bytes.len() >= 2
        && ((bytes[0] == '"' && bytes[bytes.len() - 1] == '"')
            || (bytes[0] == '\'' && bytes[bytes.len() - 1] == '\''));
    if quoted {
        bytes[1..bytes.len() - 1].iter().collect()
    } else {
        value.to_string()
    }
}

/// Overlays `./.env` onto the process environment before anything reads it.
///
/// Rules, in the order that matters:
///   * the real environment wins — an existing variable is never overwritten, so
///     the container's `env_file` and a shell export both take precedence;
///   * a missing file is not an error (CI, the image, a fresh clone);
///   * only `KEY=VALUE` lines count, split at the first `=`, and the value keeps
///     everything after it — including a `#`, so a password with one survives.
pub fn load_env_file(directory: &Path) -> EnvFileLoadResult {
    let path = directory.join(".env");
    let text = match std::fs::read_to_string(&path) {
        Ok(value) => value,
        Err(_) => {
            return EnvFileLoadResult {
                path,
                present: false,
                applied: 0,
                skipped: 0,
            }
        }
    };

    let mut applied = 0usize;
    let mut skipped = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let separator = match trimmed.find('=') {
            Some(index) if index > 0 => index,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let key = trimmed[..separator].trim();
        if key.is_empty() {
            skipped += 1;
            continue;
        }
        if std::env::var_os(key).is_some() {
            skipped += 1;
            continue;
        }
        let value = unquote(trimmed[separator + 1..].trim());
        // Edition 2021: no `unsafe` block required. Single-threaded at this point
        // in the process, before the runtime is built.
        std::env::set_var(key, value);
        applied += 1;
    }

    EnvFileLoadResult {
        path,
        present: true,
        applied,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_not_an_error() {
        let result = load_env_file(Path::new("/nonexistent-dir-for-test"));
        assert!(!result.present);
        assert_eq!(result.applied, 0);
    }
}
