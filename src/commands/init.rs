//! `navi init`: one idempotent setup verb for a repo.
//!
//! Writes the self-documenting config scaffold, keeps `.jj/` out of git in
//! colocated repos, and (with `--target`) switches the repo to bookmark
//! landings from day one, creating the bookmark if needed.

use std::fs;
use std::path::Path;

use crate::repo::NaviWorkspace;
use crate::{Error, Result};

/// Run `navi init`.
///
/// # Errors
///
/// Returns an error if the repo cannot be opened or any setup write fails.
pub fn run_init(path: &Path, target: Option<&str>, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;

    // 1. Config scaffold. Never overwrite an existing config: init is
    // setup, not migration.
    let config_path = repo.repo_config_file_path();
    let config_written = if config_path.is_file() {
        if let Some(bookmark) = target
            && repo.repo_config().lane.target.as_deref() != Some(bookmark)
        {
            return Err(Error::Engine {
                message: format!(
                    "config already exists at {}\nhint: set [lane] target = {bookmark:?} there yourself; init never rewrites an existing config",
                    config_path.display()
                ),
            });
        }
        eprintln!("config: already exists at {}", config_path.display());
        false
    } else {
        let written = repo.write_config_scaffold(target)?;
        eprintln!("config: wrote scaffold at {}", written.display());
        true
    };

    // 2. Bookmark target: create it at the current working copy's parent if
    // it does not exist yet.
    let mut bookmark_created = false;
    if let Some(bookmark) = target {
        let jj = repo.main_jj_client();
        let existing = jj.revisions(&format!(
            "present({})",
            crate::repo::quote_revset_string(bookmark)
        ))?;
        if existing.is_empty() {
            jj.bookmark_create(bookmark, "@-")?;
            bookmark_created = true;
            eprintln!("bookmark: created '{bookmark}' at @-");
        } else {
            eprintln!("bookmark: '{bookmark}' already exists");
        }
    }

    // 3. Colocated repos: keep jj's state out of git.
    let gitignore_updated = ensure_jj_gitignored(repo.workspace_root())?;
    match gitignore_updated {
        Some(true) => eprintln!("gitignore: added .jj/"),
        Some(false) => eprintln!("gitignore: .jj/ already ignored"),
        None => {}
    }

    eprintln!();
    eprintln!("initialized; next: navi skill, then navi lane open <name> -p <path>");

    if json {
        #[derive(serde::Serialize)]
        struct InitResult<'a> {
            config_path: &'a Path,
            config_written: bool,
            target: Option<&'a str>,
            bookmark_created: bool,
            gitignore_updated: Option<bool>,
        }
        println!(
            "{}",
            crate::output::render_json_envelope(
                "init",
                &InitResult {
                    config_path: &config_path,
                    config_written,
                    target,
                    bookmark_created,
                    gitignore_updated,
                }
            )?
        );
    }
    Ok(())
}

/// In a colocated repo (`.git` beside `.jj`), make sure `.jj/` is
/// gitignored. Returns `None` when the repo is not colocated, otherwise
/// whether a line was added.
fn ensure_jj_gitignored(workspace_root: &Path) -> Result<Option<bool>> {
    if !workspace_root.join(".git").exists() {
        return Ok(None);
    }
    let gitignore = workspace_root.join(".gitignore");
    let contents = if gitignore.is_file() {
        fs::read_to_string(&gitignore)?
    } else {
        String::new()
    };
    let already = contents
        .lines()
        .map(str::trim)
        .any(|line| matches!(line, ".jj" | ".jj/" | "/.jj" | "/.jj/"));
    if already {
        return Ok(Some(false));
    }
    let mut updated = contents;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(".jj/\n");
    fs::write(&gitignore, updated)?;
    Ok(Some(true))
}
