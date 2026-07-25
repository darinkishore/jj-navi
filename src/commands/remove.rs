use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::Error;
use crate::Result;
use crate::repo::NaviWorkspace;
use crate::types::WorkspaceName;

/// Run the `remove` command.
///
/// # Errors
///
/// Returns an error if workspace validation, discovery, confirmation,
/// `jj workspace forget`, or directory deletion fails.
pub fn run_remove(path: &Path, workspace: &str, yes: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let workspace = repo.resolve_workspace_alias(workspace)?;
    let target_root = repo.resolve_removable_workspace_path(&workspace)?;

    // Snapshot (best-effort, bounded) so the archive reflects on-disk work,
    // then archive any working-copy diff before it is destroyed. `remove`
    // deletes the same way `lane abandon` does, so it preserves the same way.
    let _ = crate::repo::snapshot_working_copy_at(&target_root);
    let archive = repo.archive_workspace_diff(&workspace)?;

    if !yes {
        confirm_remove(&workspace, &target_root, archive.as_deref())?;
    }

    let removed = repo.forget_workspace(&workspace)?;
    fs::remove_dir_all(&target_root).map_err(|source| {
        Error::WorkspaceDirectoryDeleteAfterForgetFailed {
            workspace: removed.as_str().to_owned(),
            path: target_root.display().to_string(),
            source,
        }
    })?;

    println!("forgot workspace '{removed}'");
    println!("deleted workspace directory '{}'", target_root.display());
    if let Some(archive) = &archive {
        println!("archived working-copy diff to '{}'", archive.display());
    }
    if json {
        #[derive(serde::Serialize)]
        struct RemoveResult<'a> {
            workspace: &'a WorkspaceName,
            removed_directory: &'a Path,
            archive: Option<&'a Path>,
        }
        println!(
            "{}",
            crate::output::render_json_envelope(
                "remove",
                &RemoveResult {
                    workspace: &removed,
                    removed_directory: &target_root,
                    archive: archive.as_deref(),
                }
            )?
        );
    }
    Ok(())
}

/// Interactive destructive-action confirmation. The prompt goes to stderr:
/// stdout is reserved for machine output.
fn confirm_remove(
    workspace: &WorkspaceName,
    target_root: &Path,
    archive: Option<&Path>,
) -> Result<()> {
    eprintln!(
        "This will permanently remove workspace '{}'.",
        workspace.as_str()
    );
    eprintln!("Directory to delete: {}", target_root.display());
    match archive {
        Some(archive) => eprintln!("Unlanded changes archived to: {}", archive.display()),
        None => eprintln!("The workspace has no working-copy changes."),
    }
    eprint!("Type 'yes' to continue: ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(Error::RemoveCancelled)
    }
}
