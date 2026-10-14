//! Disk events: external edits of `qrec.json` (A§7.4, A§7.7) and finished
//! purges (A§5.6). Owner: history. Stub: signatures only.
#![allow(clippy::todo, unused_variables)]

use crate::apply::Outcome;
use crate::ctx::Ctx;
use crate::ids::JobId;
use crate::model::Project;
use crate::state::State;

/// `Ok`: merge (`history::external`) and commit an "External edit" entry;
/// `Err`: set `runtime.disk_invalid` (a later valid read clears it).
pub fn project_changed_on_disk(
    state: &State,
    ctx: &Ctx,
    project: Result<Box<Project>, String>,
) -> Outcome {
    todo!("history: ProjectChangedOnDisk")
}

/// The purge job finished: drop the job, reply to its requester.
pub fn take_dirs_deleted(
    state: &State,
    ctx: &Ctx,
    job: JobId,
    deleted: Vec<String>,
    errors: Vec<String>,
) -> Outcome {
    todo!("history: TakeDirsDeleted")
}
