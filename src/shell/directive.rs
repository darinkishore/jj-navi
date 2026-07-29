use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

/// Environment variable used by shell integration to pass a directive file.
pub const DIRECTIVE_FILE_ENV_VAR: &str = "NAVI_DIRECTIVE_FILE";

/// Environment variable a host harness (e.g. Oh My Pi) sets — typically
/// per tool call — to receive the raw absolute path of a workspace
/// switch. Unlike the shell directive (shell-syntax lines, appended for
/// the wrapper to source), the harness sink gets the bare path,
/// overwritten: last writer wins within one command chain, which is the
/// final intent.
pub const HARNESS_CD_FILE_ENV_VAR: &str = "OMP_CD_FILE";

/// Write a shell-safe `cd` directive if shell integration is active, and
/// mirror the raw target path to a host harness when one is listening.
///
/// Returns `true` if a shell directive was written (the harness sink
/// never affects the return value: stdout fallback behavior belongs to
/// shell integration alone).
///
/// # Errors
///
/// Returns an error if the shell directive file path is invalid or
/// writing it fails.
pub fn write_cd_directive(path: &Path) -> crate::Result<bool> {
    write_harness_cd_file(path);

    let Ok(directive_file) = std::env::var(DIRECTIVE_FILE_ENV_VAR) else {
        return Ok(false);
    };

    if directive_file.trim().is_empty() {
        return Ok(false);
    }

    let escaped_path = escape_shell_single_quotes(
        path.to_str()
            .ok_or(crate::Error::ShellDirectivePathNotUtf8)?,
    );
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directive_file)?;
    writeln!(file, "cd -- '{escaped_path}'")?;
    Ok(true)
}

/// Best-effort mirror of the raw switch target for a host harness.
///
/// Failures warn on stderr and never fail the verb: the harness only
/// acts when it reads a valid path afterwards, so a missed write
/// degrades to "the session does not follow" with no false-success
/// window — the model only believes a re-root happened when the harness
/// appends its own notice.
fn write_harness_cd_file(path: &Path) {
    let Ok(harness_file) = std::env::var(HARNESS_CD_FILE_ENV_VAR) else {
        return;
    };
    if harness_file.trim().is_empty() {
        return;
    }
    let Some(target) = path.to_str() else {
        eprintln!("warning: harness cd target is not valid UTF-8; skipped: {path:?}");
        return;
    };
    if let Err(error) = std::fs::write(&harness_file, format!("{target}\n")) {
        eprintln!("warning: failed to write harness cd file ({harness_file}): {error}");
    }
}

/// Escape single quotes for POSIX shell single-quoted strings.
#[must_use]
pub fn escape_shell_single_quotes(value: &str) -> String {
    value.replace('\'', "'\\''")
}
