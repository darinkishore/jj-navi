use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::{LaneConfig, LanePath, RepoConfig, WorkspaceName, WorkspaceTemplate};

const NAVI_DIR: &str = "navi";
const CONFIG_FILE: &str = "config.toml";
const STATE_FILE: &str = "state.toml";

#[derive(Deserialize, Serialize)]
struct RepoConfigFile {
    workspace_template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lane: Option<LaneConfigFile>,
}

#[derive(Default, Deserialize, Serialize)]
struct LaneConfigFile {
    #[serde(default)]
    trunk: Option<String>,
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
        gate: lane_file.gate.filter(|gate| !gate.trim().is_empty()),
        sparse: lane_file.sparse.unwrap_or(lane_defaults.sparse),
        context_paths: lane_file
            .context_paths
            .into_iter()
            .map(LanePath::new)
            .collect::<Result<Vec<_>>>()
            .map_err(|error| config_error(error.to_string()))?,
    };

    Ok(RepoConfig {
        workspace_template,
        lane,
    })
}

pub(crate) fn ensure_repo_config(repo_storage_path: &Path, config: &RepoConfig) -> Result<PathBuf> {
    let navi_dir = navi_dir_path(repo_storage_path);
    fs::create_dir_all(&navi_dir)?;

    let path = repo_config_path(repo_storage_path);
    if !path.exists() {
        let file = RepoConfigFile {
            workspace_template: config.workspace_template.as_str().to_owned(),
            lane: None,
        };
        let contents = toml::to_string_pretty(&file).map_err(|error| Error::InvalidRepoConfig {
            path: path.clone(),
            message: error.to_string(),
        })?;
        fs::write(&path, contents)?;
    }

    Ok(path)
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
