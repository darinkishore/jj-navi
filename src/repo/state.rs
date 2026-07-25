use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::WorkspaceName;

use super::config::repo_state_path;

#[derive(Default)]
pub(crate) struct RepoStateStore {
    path: PathBuf,
    previous_workspace: Option<WorkspaceName>,
    divergent_baseline: Option<usize>,
}

#[derive(Default, Deserialize, Serialize)]
struct RepoStateFile {
    #[serde(default)]
    switch: SwitchStateFile,
    #[serde(default)]
    health: HealthStateFile,
}

#[derive(Default, Deserialize, Serialize)]
struct HealthStateFile {
    /// Last observed count of divergent changes; the tripwire warns when
    /// the live count exceeds this.
    #[serde(default)]
    divergent_baseline: Option<usize>,
}

#[derive(Default, Deserialize, Serialize)]
struct SwitchStateFile {
    #[serde(default)]
    previous_workspace: Option<String>,
}

impl RepoStateStore {
    pub(crate) fn load(repo_storage_path: &Path) -> Result<Self> {
        let path = repo_state_path(repo_storage_path);
        if !path.is_file() {
            return Ok(Self {
                path,
                previous_workspace: None,
                divergent_baseline: None,
            });
        }

        let contents = fs::read_to_string(&path)?;
        let file = toml::from_str::<RepoStateFile>(&contents).map_err(|error| {
            Error::InvalidRepoState {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;

        Ok(Self {
            path: path.clone(),
            previous_workspace: parse_workspace_name(file.switch.previous_workspace, &path)?,
            divergent_baseline: file.health.divergent_baseline,
        })
    }

    pub(crate) fn previous_workspace(&self) -> Option<&WorkspaceName> {
        self.previous_workspace.as_ref()
    }

    pub(crate) fn divergent_baseline(&self) -> Option<usize> {
        self.divergent_baseline
    }

    pub(crate) fn set_divergent_baseline(&mut self, count: usize) {
        self.divergent_baseline = Some(count);
    }

    pub(crate) fn save_previous_workspace(
        repo_storage_path: &Path,
        workspace: &WorkspaceName,
    ) -> Result<()> {
        let mut store = Self::load(repo_storage_path).unwrap_or_else(|_| Self {
            path: repo_state_path(repo_storage_path),
            previous_workspace: None,
            divergent_baseline: None,
        });
        store.previous_workspace = Some(workspace.clone());
        store.save()
    }

    pub(crate) fn save(&self) -> Result<()> {
        let file = RepoStateFile {
            switch: SwitchStateFile {
                previous_workspace: self
                    .previous_workspace
                    .as_ref()
                    .map(|workspace| workspace.as_str().to_owned()),
            },
            health: HealthStateFile {
                divergent_baseline: self.divergent_baseline,
            },
        };
        let contents = toml::to_string_pretty(&file).map_err(|error| Error::InvalidRepoState {
            path: self.path.clone(),
            message: error.to_string(),
        })?;
        super::storage::save_atomic(&self.path, &contents)?;
        Ok(())
    }
}

fn parse_workspace_name(value: Option<String>, path: &Path) -> Result<Option<WorkspaceName>> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| {
            WorkspaceName::new(value).map_err(|error| Error::InvalidRepoState {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
        })
        .transpose()
}
