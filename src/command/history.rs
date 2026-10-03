//! Undo/redo history. Entries are project snapshots (cheap: sections are shared until modified).
//! Persisted under `.cadlab/history/` so undo works across CLI invocations.

use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{ModelError, Project};
use serde::{Deserialize, Serialize};

/// Maximum number of undo steps kept.
pub const HISTORY_LIMIT: usize = 100;

const INDEX_FILE: &str = "index.json";

#[derive(Clone, Debug)]
enum Snapshot {
    Loaded(Project),
    OnDisk,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    seq: u64,
    label: String,
    snapshot: Snapshot,
}

/// One step in the history, as listed by `history.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryItem {
    /// What the step did (command name).
    pub label: String,
}

#[derive(Serialize, Deserialize)]
struct Index {
    next_seq: u64,
    undo: Vec<IndexEntry>,
    redo: Vec<IndexEntry>,
}

#[derive(Serialize, Deserialize)]
struct IndexEntry {
    seq: u64,
    label: String,
}

/// Undo and redo stacks.
#[derive(Clone, Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    next_seq: u64,
    dir: Option<PathBuf>,
}

impl History {
    /// Empty history, persisted under `dir` when saved.
    pub fn new(dir: Option<PathBuf>) -> Self {
        History {
            dir,
            ..Default::default()
        }
    }

    /// Loads the index from `dir`; snapshots are read lazily on undo/redo.
    pub fn load(dir: PathBuf) -> Result<Self, ModelError> {
        let index_path = dir.join(INDEX_FILE);
        let text = match fs::read_to_string(&index_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(History::new(Some(dir)));
            }
            Err(e) => {
                return Err(ModelError::Io {
                    path: index_path,
                    source: e,
                });
            }
        };
        let index: Index = serde_json::from_str(&text).map_err(|e| ModelError::Invalid {
            path: index_path.clone(),
            message: e.to_string(),
        })?;
        let conv = |v: Vec<IndexEntry>| {
            v.into_iter()
                .map(|e| Entry {
                    seq: e.seq,
                    label: e.label,
                    snapshot: Snapshot::OnDisk,
                })
                .collect()
        };
        Ok(History {
            undo: conv(index.undo),
            redo: conv(index.redo),
            next_seq: index.next_seq,
            dir: Some(dir),
        })
    }

    /// Records `before` as the state to return to when undoing `label`. Clears redo.
    pub fn record(&mut self, label: impl Into<String>, before: Project) {
        let e = self.entry(label.into(), before);
        self.undo.push(e);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Clears both stacks.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Undo depth.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Redo depth.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Undo entries, most recent last.
    pub fn undo_items(&self) -> Vec<HistoryItem> {
        self.undo
            .iter()
            .map(|e| HistoryItem { label: e.label.clone() })
            .collect()
    }

    /// Redo entries, next to redo last.
    pub fn redo_items(&self) -> Vec<HistoryItem> {
        self.redo
            .iter()
            .map(|e| HistoryItem { label: e.label.clone() })
            .collect()
    }

    /// Steps back: returns the previous state and its label, saving `current` for redo.
    pub fn undo(&mut self, current: Project) -> Result<Option<(String, Project)>, ModelError> {
        let Some(e) = self.undo.pop() else {
            return Ok(None);
        };
        let before = self.materialize(&e)?;
        let redo = self.entry(e.label.clone(), current);
        self.redo.push(redo);
        Ok(Some((e.label, before)))
    }

    /// Steps forward again.
    pub fn redo(&mut self, current: Project) -> Result<Option<(String, Project)>, ModelError> {
        let Some(e) = self.redo.pop() else {
            return Ok(None);
        };
        let after = self.materialize(&e)?;
        let undo = self.entry(e.label.clone(), current);
        self.undo.push(undo);
        Ok(Some((e.label, after)))
    }

    /// Writes new snapshots and the index; removes snapshot files no longer referenced.
    pub fn save(&mut self) -> Result<(), ModelError> {
        let Some(dir) = self.dir.clone() else {
            return Ok(());
        };
        fs::create_dir_all(&dir).map_err(|e| ModelError::Io {
            path: dir.clone(),
            source: e,
        })?;
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if let Snapshot::Loaded(p) = &e.snapshot {
                let path = snapshot_path(&dir, e.seq);
                if !path.exists() {
                    write(&path, &p.to_packed_string())?;
                }
            }
        }
        let index = Index {
            next_seq: self.next_seq,
            undo: self
                .undo
                .iter()
                .map(|e| IndexEntry {
                    seq: e.seq,
                    label: e.label.clone(),
                })
                .collect(),
            redo: self
                .redo
                .iter()
                .map(|e| IndexEntry {
                    seq: e.seq,
                    label: e.label.clone(),
                })
                .collect(),
        };
        let text = serde_json::to_string_pretty(&index).expect("index serializes");
        write(&dir.join(INDEX_FILE), &text)?;
        // Garbage-collect unreferenced snapshots.
        let live: std::collections::BTreeSet<u64> = self.undo.iter().chain(self.redo.iter()).map(|e| e.seq).collect();
        if let Ok(rd) = fs::read_dir(&dir) {
            for f in rd.flatten() {
                let name = f.file_name();
                let name = name.to_string_lossy();
                if let Some(seq) = name.strip_suffix(".json").and_then(|s| s.parse::<u64>().ok())
                    && !live.contains(&seq)
                {
                    let _ = fs::remove_file(f.path());
                }
            }
        }
        Ok(())
    }

    fn entry(&mut self, label: String, p: Project) -> Entry {
        let seq = self.next_seq;
        self.next_seq += 1;
        Entry {
            seq,
            label,
            snapshot: Snapshot::Loaded(p),
        }
    }

    fn materialize(&self, e: &Entry) -> Result<Project, ModelError> {
        match &e.snapshot {
            Snapshot::Loaded(p) => Ok(p.clone()),
            Snapshot::OnDisk => {
                let dir = self.dir.as_ref().expect("on-disk snapshot implies a directory");
                let path = snapshot_path(dir, e.seq);
                let text = fs::read_to_string(&path).map_err(|err| ModelError::Io {
                    path: path.clone(),
                    source: err,
                })?;
                Project::from_packed_str(&text)
            }
        }
    }
}

fn snapshot_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{seq}.json"))
}

fn write(path: &Path, content: &str) -> Result<(), ModelError> {
    fs::write(path, content).map_err(|e| ModelError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}
