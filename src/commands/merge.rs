use std::path::Path;

use crate::Result;
use crate::output::render_merge_outcome;
use crate::repo::NaviWorkspace;

/// Run the `merge` command.
///
/// # Errors
///
/// Returns an error if source or target resolution fails, if either workspace
/// is unhealthy, or if `jj duplicate`/`jj rebase` fails.
pub fn run_merge(
    path: &Path,
    from: Option<&str>,
    into: Option<&str>,
    revisions: Option<&str>,
    json: bool,
) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let source = from
        .map(|workspace| repo.resolve_workspace_alias(workspace))
        .transpose()?;
    let target = into
        .map(|workspace| repo.resolve_workspace_alias(workspace))
        .transpose()?;
    let outcome = repo.merge_workspace(source.as_ref(), target.as_ref(), revisions)?;

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
            source: Option<&'a str>,
            target: &'a str,
            revset: &'a str,
            revisions: Vec<MergeRevisionJson<'a>>,
            duplicated_roots: &'a [String],
            duplicated_heads: &'a [String],
        }
        let merge = &outcome.merge;
        println!(
            "{}",
            crate::output::render_json_envelope(
                "merge",
                &MergeResult {
                    source: merge
                        .source
                        .as_ref()
                        .map(|source| source.snapshot.name.as_str()),
                    target: merge.target.snapshot.name.as_str(),
                    revset: &merge.revset,
                    revisions: merge
                        .revisions
                        .iter()
                        .map(|revision| MergeRevisionJson {
                            commit_id: &revision.commit_id,
                            change_id: &revision.change_id,
                            message: &revision.message,
                        })
                        .collect(),
                    duplicated_roots: &outcome.duplicated_roots,
                    duplicated_heads: &outcome.duplicated_heads,
                }
            )?
        );
    }

    Ok(())
}
