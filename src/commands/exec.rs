//! `navi exec`: run a raw `jj` command under navi's umbrella.
//!
//! The working copy is made current first (staleness is weather, not the
//! caller's problem), the repo mutation lock is held for the duration so
//! concurrent navi verbs serialize instead of racing, and the command runs
//! in the resolved workspace root.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use crate::repo::NaviWorkspace;
use crate::types::WorkspaceName;
use crate::{Error, Result};

/// Run `navi exec [-w workspace] -- <jj args...>`.
///
/// # Errors
///
/// Returns an error if the workspace cannot be resolved or the command
/// cannot be spawned. The child's exit code is propagated.
pub fn run_exec(path: &Path, workspace: Option<&str>, args: &[OsString]) -> Result<ExitCode> {
    if args.is_empty() {
        return Err(Error::Engine {
            message: String::from("exec requires a command: navi exec -- <jj args...>"),
        });
    }

    let repo = NaviWorkspace::open(path)?;
    let target_root = match workspace {
        None => repo.workspace_root().to_path_buf(),
        Some(name) => {
            let name = WorkspaceName::new(name.to_owned())?;
            let resolved = repo.resolve_workspace_path(&name)?;
            if !resolved.is_switchable() {
                return Err(Error::WorkspaceDirectoryUnavailable {
                    workspace: name.as_str().to_owned(),
                    path: resolved.path.display().to_string(),
                });
            }
            resolved.path
        }
    };

    let exit = repo.with_mutation_lock(|| {
        // Umbrella: recover staleness before the user's command sees it.
        let _ = crate::repo::snapshot_working_copy_at(&target_root);

        let jj_bin = std::env::var_os("NAVI_JJ_BIN")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("jj"));
        let status = std::process::Command::new(jj_bin)
            .args(args)
            .current_dir(&target_root)
            .status()?;
        let code = status.code().unwrap_or(1);
        Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
    })?;
    repo.divergence_tripwire();
    Ok(exit)
}
