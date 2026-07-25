use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::{
    LaneConfig, LanePath, RepoConfig, ResolvePolicy, ResolveStrategy, WorkspaceName,
    WorkspaceTemplate,
};

const NAVI_DIR: &str = "navi";
const CONFIG_FILE: &str = "config.toml";
const STATE_FILE: &str = "state.toml";

#[derive(Deserialize, Serialize)]
struct RepoConfigFile {
    workspace_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lane: Option<LaneConfigFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolve: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Default, Deserialize, Serialize)]
struct LaneConfigFile {
    #[serde(default)]
    trunk: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    integration_workspace: Option<String>,
    #[serde(default)]
    gate: Option<String>,
    #[serde(default)]
    sparse: Option<bool>,
    #[serde(default)]
    context_paths: Vec<String>,
}

pub(crate) fn load_repo_config(repo_storage_path: &Path) -> Result<RepoConfig> {
    let path = repo_config_path(repo_storage_path);
    if !path.is_file() {
        return Ok(RepoConfig::default());
    }

    let contents = fs::read_to_string(&path)?;
    let file =
        toml::from_str::<RepoConfigFile>(&contents).map_err(|error| Error::InvalidRepoConfig {
            path: path.clone(),
            message: error.to_string(),
        })?;

    let config_error = |message: String| Error::InvalidRepoConfig {
        path: path.clone(),
        message,
    };

    let workspace_template = WorkspaceTemplate::new(file.workspace_template)
        .map_err(|error| config_error(error.to_string()))?;

    let lane_file = file.lane.unwrap_or_default();
    let lane_defaults = LaneConfig::default();
    let lane = LaneConfig {
        trunk: lane_file
            .trunk
            .map(WorkspaceName::new)
            .transpose()
            .map_err(|error| config_error(error.to_string()))?
            .unwrap_or(lane_defaults.trunk),
        target: lane_file.target.filter(|target| !target.trim().is_empty()),
        integration_workspace: lane_file
            .integration_workspace
            .map(WorkspaceName::new)
            .transpose()
            .map_err(|error| config_error(error.to_string()))?
            .unwrap_or(lane_defaults.integration_workspace),
        gate: lane_file.gate.filter(|gate| !gate.trim().is_empty()),
        sparse: lane_file.sparse.unwrap_or(lane_defaults.sparse),
        context_paths: lane_file
            .context_paths
            .into_iter()
            .map(LanePath::new)
            .collect::<Result<Vec<_>>>()
            .map_err(|error| config_error(error.to_string()))?,
    };

    let resolve = file
        .resolve
        .unwrap_or_default()
        .into_iter()
        .map(|(path, strategy)| {
            let parsed = ResolveStrategy::parse(&strategy).ok_or_else(|| {
                config_error(format!(
                    "unknown resolve strategy '{strategy}' for '{path}' (supported: union)"
                ))
            })?;
            Ok(ResolvePolicy {
                path,
                strategy: parsed,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(RepoConfig {
        workspace_template,
        lane,
        resolve,
    })
}

pub(crate) fn ensure_repo_config(repo_storage_path: &Path, config: &RepoConfig) -> Result<PathBuf> {
    let navi_dir = navi_dir_path(repo_storage_path);
    fs::create_dir_all(&navi_dir)?;

    let path = repo_config_path(repo_storage_path);
    if !path.exists() {
        super::storage::save_atomic(&path, &render_config_scaffold(config))?;
    }

    Ok(path)
}

/// Initial config file: current effective values plus a commented map of
/// every knob, so the file documents itself.
fn render_config_scaffold(config: &RepoConfig) -> String {
    format!(
        "# jj-navi repo configuration. Uncomment and edit to change behavior.\n\
         \n\
         # Template for planning new workspace directories.\n\
         workspace_template = {template:?}\n\
         \n\
         # [lane]\n\
         # # Workspace whose working-copy parent is the trunk head that lanes\n\
         # # sync onto and land into (legacy mode; ignored when target is set).\n\
         # trunk = \"default\"\n\
         # # Bookmark lanes land into. When set, landings advance this bookmark\n\
         # # via the integration workspace and no live working copy is ever\n\
         # # fast-forwarded. The bookmark must exist (jj bookmark create).\n\
         # target = \"main\"\n\
         # # Sparse-empty workspace the bookmark advance runs in (auto-created).\n\
         # integration_workspace = \"navi-integration\"\n\
         # # Gate command run (via sh -c, in the lane workspace) before every\n\
         # # landing; landing aborts if it fails.\n\
         # gate = \"cargo test\"\n\
         # # Create lane workspaces sparse by default (write-set + context\n\
         # # paths only). Override per lane with --sparse/--full.\n\
         # sparse = false\n\
         # # Extra read-only paths materialized into sparse lane workspaces.\n\
         # context_paths = []\n\
         \n\
         # [resolve]\n\
         # # Automatic conflict-resolution policies: navi resolve --apply (no\n\
         # # arguments) sweeps every entry. 'union' keeps every side of each\n\
         # # conflicted hunk -- right for append-only files like changelogs.\n\
         # \"CHANGELOG.md\" = \"union\"\n",
        template = config.workspace_template.as_str(),
    )
}

pub(crate) fn navi_dir_path(repo_storage_path: &Path) -> PathBuf {
    repo_storage_path.join(NAVI_DIR)
}

pub(crate) fn repo_config_path(repo_storage_path: &Path) -> PathBuf {
    navi_dir_path(repo_storage_path).join(CONFIG_FILE)
}

pub(crate) fn repo_state_path(repo_storage_path: &Path) -> PathBuf {
    navi_dir_path(repo_storage_path).join(STATE_FILE)
}
