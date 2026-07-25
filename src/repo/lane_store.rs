use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::types::{LaneLifecycle, LanePath, WorkspaceName};

use super::config::navi_dir_path;

const LANES_FILE: &str = "lanes.toml";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneLandRecord {
    pub(crate) head_commit: String,
    pub(crate) at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaneRecord {
    pub(crate) name: WorkspaceName,
    pub(crate) paths: Vec<LanePath>,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) lifecycle: LaneLifecycle,
    pub(crate) closed_at: Option<OffsetDateTime>,
    pub(crate) last_land: Option<LaneLandRecord>,
}

#[derive(Default)]
pub(crate) struct LaneStore {
    path: PathBuf,
    records: Vec<LaneRecord>,
    /// Raw records that failed validation, preserved verbatim on save so a
    /// single bad entry degrades to a warning instead of bricking every
    /// lane command.
    quarantined: Vec<toml::Value>,
    warnings: Vec<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct LaneFile {
    #[serde(default, rename = "lane")]
    lanes: Vec<toml::Value>,
}

#[derive(Deserialize, Serialize)]
struct LaneRecordFile {
    name: String,
    paths: Vec<String>,
    created_at: String,
    lifecycle: String,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    last_land_head: Option<String>,
    #[serde(default)]
    last_land_at: Option<String>,
}

impl LaneStore {
    pub(crate) fn load(repo_storage_path: &Path) -> Result<Self> {
        let path = lane_store_path(repo_storage_path);
        if !path.is_file() {
            return Ok(Self {
                path,
                ..Self::default()
            });
        }

        let contents = fs::read_to_string(&path)?;
        let file =
            toml::from_str::<LaneFile>(&contents).map_err(|error| Error::InvalidLaneRegistry {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let mut records = Vec::new();
        let mut quarantined = Vec::new();
        let mut warnings = Vec::new();
        for value in file.lanes {
            let parsed = value
                .clone()
                .try_into::<LaneRecordFile>()
                .map_err(|error| error.to_string())
                .and_then(|record| {
                    parse_record(record, &path).map_err(|error| match error {
                        Error::InvalidLaneRegistry { message, .. } => message,
                        other => other.to_string(),
                    })
                });
            match parsed {
                Ok(record) => records.push(record),
                Err(message) => {
                    let name = value
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("<unnamed>");
                    warnings.push(format!(
                        "skipped invalid lane record '{name}' in {}: {message}",
                        path.display()
                    ));
                    quarantined.push(value);
                }
            }
        }

        Ok(Self {
            path,
            records,
            quarantined,
            warnings,
        })
    }

    /// Warnings produced while loading (for example quarantined records).
    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn get(&self, name: &WorkspaceName) -> Option<&LaneRecord> {
        self.records.iter().find(|record| record.name == *name)
    }

    pub(crate) fn open_lanes(&self) -> Vec<&LaneRecord> {
        self.records
            .iter()
            .filter(|record| record.lifecycle == LaneLifecycle::Open)
            .collect()
    }

    pub(crate) fn all_lanes(&self) -> &[LaneRecord] {
        &self.records
    }

    pub(crate) fn insert(&mut self, record: LaneRecord) -> Result<()> {
        if let Some(existing) = self.get(&record.name) {
            if existing.lifecycle == LaneLifecycle::Open {
                return Err(Error::LaneExists(record.name.as_str().to_owned()));
            }
            // Reopening a terminal lane name replaces the old record; jj
            // history remains the durable archive of the previous lane.
            self.records.retain(|other| other.name != record.name);
        }
        self.records.push(record);
        self.records
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    /// Remove a record entirely (used to roll back a failed `lane open`).
    pub(crate) fn remove(&mut self, name: &WorkspaceName) {
        self.records.retain(|record| record.name != *name);
    }

    pub(crate) fn set_lifecycle(
        &mut self,
        name: &WorkspaceName,
        lifecycle: LaneLifecycle,
        at: OffsetDateTime,
    ) -> Result<()> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.name == *name)
            .ok_or_else(|| Error::LaneNotFound(name.as_str().to_owned()))?;
        record.lifecycle = lifecycle;
        record.closed_at = match lifecycle {
            LaneLifecycle::Open => None,
            LaneLifecycle::Closed | LaneLifecycle::Abandoned => Some(at),
        };
        Ok(())
    }

    pub(crate) fn replace_paths(
        &mut self,
        name: &WorkspaceName,
        paths: Vec<LanePath>,
    ) -> Result<()> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.name == *name)
            .ok_or_else(|| Error::LaneNotFound(name.as_str().to_owned()))?;
        record.paths = paths;
        Ok(())
    }

    pub(crate) fn record_land(
        &mut self,
        name: &WorkspaceName,
        head_commit: &str,
        at: OffsetDateTime,
    ) -> Result<()> {
        let record = self
            .records
            .iter_mut()
            .find(|record| record.name == *name)
            .ok_or_else(|| Error::LaneNotFound(name.as_str().to_owned()))?;
        record.last_land = Some(LaneLandRecord {
            head_commit: head_commit.to_owned(),
            at,
        });
        Ok(())
    }

    pub(crate) fn save(&self) -> Result<()> {
        let registry_error = |message: String| Error::InvalidLaneRegistry {
            path: self.path.clone(),
            message,
        };

        let mut lanes = Vec::with_capacity(self.records.len() + self.quarantined.len());
        for record in &self.records {
            let record = serialize_record(record, &self.path)?;
            lanes.push(
                toml::Value::try_from(record)
                    .map_err(|error| registry_error(error.to_string()))?,
            );
        }
        // Quarantined records ride along untouched; navi never deletes what
        // it could not parse.
        lanes.extend(self.quarantined.iter().cloned());

        let contents = toml::to_string_pretty(&LaneFile { lanes })
            .map_err(|error| registry_error(error.to_string()))?;
        super::storage::save_atomic(&self.path, &contents)?;
        Ok(())
    }
}

fn serialize_record(record: &LaneRecord, path: &Path) -> Result<LaneRecordFile> {
    let format_time = |time: &OffsetDateTime| {
        time.format(&Rfc3339)
            .map_err(|error| Error::InvalidLaneRegistry {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
    };

    Ok(LaneRecordFile {
        name: record.name.as_str().to_owned(),
        paths: record
            .paths
            .iter()
            .map(|lane_path| lane_path.as_str().to_owned())
            .collect(),
        created_at: format_time(&record.created_at)?,
        lifecycle: record.lifecycle.as_str().to_owned(),
        closed_at: record.closed_at.as_ref().map(&format_time).transpose()?,
        last_land_head: record
            .last_land
            .as_ref()
            .map(|land| land.head_commit.clone()),
        last_land_at: record
            .last_land
            .as_ref()
            .map(|land| format_time(&land.at))
            .transpose()?,
    })
}

fn parse_record(record: LaneRecordFile, path: &Path) -> Result<LaneRecord> {
    let registry_error = |message: String| Error::InvalidLaneRegistry {
        path: path.to_path_buf(),
        message,
    };
    let parse_time = |value: &str| {
        OffsetDateTime::parse(value, &Rfc3339).map_err(|error| registry_error(error.to_string()))
    };

    let lifecycle = LaneLifecycle::parse(&record.lifecycle)
        .ok_or_else(|| registry_error(format!("unknown lane lifecycle '{}'", record.lifecycle)))?;

    let last_land = match (record.last_land_head, record.last_land_at) {
        (Some(head_commit), Some(at)) => Some(LaneLandRecord {
            head_commit,
            at: parse_time(&at)?,
        }),
        (None, None) => None,
        _ => {
            return Err(registry_error(String::from(
                "last_land_head and last_land_at must be present together",
            )));
        }
    };

    Ok(LaneRecord {
        name: WorkspaceName::new(record.name)
            .map_err(|error| registry_error(error.to_string()))?,
        paths: record
            .paths
            .into_iter()
            .map(|value| LanePath::new(value).map_err(|error| registry_error(error.to_string())))
            .collect::<Result<Vec<_>>>()?,
        created_at: parse_time(&record.created_at)?,
        lifecycle,
        closed_at: record.closed_at.as_deref().map(parse_time).transpose()?,
        last_land,
    })
}

pub(crate) fn lane_store_path(repo_storage_path: &Path) -> PathBuf {
    navi_dir_path(repo_storage_path).join(LANES_FILE)
}
