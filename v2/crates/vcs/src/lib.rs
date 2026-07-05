//! Bare repo management and git operations (spec Part 5, §12.2).
//!
//! All git operations shell out to the `git` CLI against bare repos at
//! `{repos_root}/{owner}/{project}.git`. TODO: implement.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VcsError {
    #[error("git command failed: {0}")]
    Git(String),
    #[error("repo not found: {0}")]
    RepoNotFound(String),
    #[error("merge conflict")]
    MergeConflict,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct RepoManager {
    pub repos_root: PathBuf,
}

impl RepoManager {
    pub fn new(repos_root: impl Into<PathBuf>) -> Self {
        Self {
            repos_root: repos_root.into(),
        }
    }

    // TODO (spec §5.1, §12.2, §3.2, §4.3, §6.2):
    // - create_project(owner, project, default_branch): init bare repo, HEAD symref,
    //   initial empty commit
    // - default_branch(owner, project): read HEAD symref
    // - create_branch / delete_branch / hard_reset
    // - squash_merge(owner, project, seq, job_type, summary) -> Ok | MergeConflict
    // - conflict_context(old_base_ref, new_base_ref): status + log + diff-stat block
    // - diff_for_job(job) with per-state behavior incl. Done-state `git log --grep`
    // - tree(ref) / blob(ref, path) / log(ref, limit)
    // - read_file_at(ref, path): job type + prompt resolution at base_ref
}
