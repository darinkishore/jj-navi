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
}

#[derive(Default, Deserialize, Serialize)]
struct LaneFile {
    #[serde(default, rename = "lane")]
    lanes: Vec<LaneRecordFile>,
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
                records: Vec::new(),
            });
        }

        let contents = fs::read_to_string(&path)?;
        let file =
            toml::from_str::<LaneFile>(&contents).map_err(|error| Error::InvalidLaneRegistry {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let records = file
            .lanes
            .into_iter()
            .map(|record| parse_record(record, &path))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { path, records })
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
        let parent = self.path.parent().ok_or_else(|| Error::InvalidLaneRegistry {
            path: self.path.clone(),
            message: String::from("lane registry path has no parent"),
        })?;
        fs::create_dir_all(parent)?;

        let file = LaneFile {
            lanes: self
                .records
                .iter()
                .map(|record| serialize_record(record, &self.path))
                .collect::<Result<Vec<_>>>()?,
        };

        let contents =
            toml::to_string_pretty(&file).map_err(|error| Error::InvalidLaneRegistry {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        fs::write(&self.path, contents)?;
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
