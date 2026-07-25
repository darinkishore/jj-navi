use std::path::Path;

use crate::Result;
use crate::output::render_merge_outcome;
use crate::repo::NaviWorkspace;
use crate::types::WorkspaceName;

/// Run the `merge` command.
///
/// # Errors
///
/// Returns an error if source or target resolution fails, if either workspace
/// is unhealthy, or if `jj duplicate`/`jj rebase` fails.
pub fn run_merge(path: &Path, from: &str, into: Option<&str>, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let source = WorkspaceName::new(from.to_owned())?;
    let target = into
        .map(|workspace| WorkspaceName::new(workspace.to_owned()))
        .transpose()?;
    let outcome = repo.merge_workspace(&source, target.as_ref())?;

    eprint!("{}", render_merge_outcome(&outcome));
    if json {
        #[derive(serde::Serialize)]
        struct MergeRevisionJson<'a> {
            commit_id: &'a str,
            change_id: &'a str,
            message: &'a str,
        }
        #[derive(serde::Serialize)]
        struct MergeResult<'a> {
            source: &'a str,
            target: &'a str,
            revisions: Vec<MergeRevisionJson<'a>>,
            duplicated_root_change_id: &'a str,
            duplicated_head_change_id: &'a str,
        }
        let merge = &outcome.merge;
        println!(
            "{}",
            crate::output::render_json_envelope(
                "merge",
                &MergeResult {
                    source: merge.source.snapshot.name.as_str(),
                    target: merge.target.snapshot.name.as_str(),
                    revisions: merge
                        .revisions
                        .iter()
                        .map(|revision| MergeRevisionJson {
                            commit_id: &revision.commit_id,
                            change_id: &revision.change_id,
                            message: &revision.message,
                        })
                        .collect(),
                    duplicated_root_change_id: &outcome.duplicated_root_change_id,
                    duplicated_head_change_id: &outcome.duplicated_head_change_id,
                }
            )?
        );
    }

    Ok(())
}
