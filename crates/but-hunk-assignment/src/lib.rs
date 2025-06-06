//!
//!
//! Hunk - File, range
//!
//! HunkAssignment - None or Some(Stack in workspace)
//!
//! reconcile_assignments - takes worktree changes (Vec<TreeChange>) + current assignments (Vec<HunkAssignment>)
//! returns updated assignments (Vec<HunkAssignment>)
//!
//! set_assignments

mod reconcile;
mod state;

use anyhow::Result;
use bstr::{BString, ByteSlice};
use but_core::UnifiedDiff;
use but_hunk_dependency::ui::{HunkLock, hunk_dependencies_for_workspace_changes_by_worktree_dir};
use but_workspace::{HunkHeader, StackId};
use gitbutler_command_context::CommandContext;
use gitbutler_stack::VirtualBranchesHandle;
use itertools::Itertools;
use reconcile::MultipleOverlapping;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HunkAssignment {
    /// A stable identifier for the hunk assignment.
    ///   - When a new hunk is first observed (from the uncommitted changes), it is assigned a new id.
    ///   - If a hunk is modified (i.e. it has gained or lost lines), the UUID remains the same.
    ///   - If two or more hunks become merged (due to edits causing the contexts to overlap), the id of the hunk with the most lines is adopted.
    pub id: Option<Uuid>,
    /// The hunk that is being assigned. Together with path_bytes, this identifies the hunk.
    /// If the file is binary, or too large to load, this will be None and in this case the path name is the only identity.
    pub hunk_header: Option<HunkHeader>,
    /// The file path of the hunk.
    pub path: String,
    /// The file path of the hunk in bytes.
    pub path_bytes: BString,
    /// The stack to which the hunk is assigned. If None, the hunk is not assigned to any stack.
    pub stack_id: Option<StackId>,
    /// The dependencies(locks) that this hunk has. This determines where the hunk can be assigned.
    /// This field is ignored when HunkAssignment is passed by the UI to create a new assignment.
    pub hunk_locks: Vec<HunkLock>,
}

impl TryFrom<but_db::HunkAssignment> for HunkAssignment {
    type Error = anyhow::Error;
    fn try_from(value: but_db::HunkAssignment) -> Result<Self, Self::Error> {
        let header = value
            .hunk_header
            .as_ref()
            .and_then(|h| serde_json::from_str(h).ok());
        let hunk_locks: Vec<HunkLock> = serde_json::from_str(&value.hunk_locks)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize hunk_locks: {}", e))?;
        let stack_id = value
            .stack_id
            .as_ref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(StackId::from);
        Ok(HunkAssignment {
            id: value.id.map(|id| Uuid::parse_str(&id)).transpose()?,
            hunk_header: header,
            path: value.path,
            path_bytes: value.path_bytes.into(),
            stack_id,
            hunk_locks,
        })
    }
}

impl TryFrom<HunkAssignment> for but_db::HunkAssignment {
    type Error = anyhow::Error;
    fn try_from(value: HunkAssignment) -> Result<Self, Self::Error> {
        let header = value
            .hunk_header
            .map(|h| {
                serde_json::to_string(&h)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize hunk_header: {}", e))
            })
            .transpose()?;
        Ok(but_db::HunkAssignment {
            id: value.id.map(|id| id.to_string()),
            hunk_header: header,
            path: value.path,
            path_bytes: value.path_bytes.into(),
            stack_id: value.stack_id.map(|id| id.to_string()),
            hunk_locks: serde_json::to_string(&value.hunk_locks)?,
        })
    }
}

impl From<HunkAssignment> for but_workspace::DiffSpec {
    fn from(value: HunkAssignment) -> Self {
        let hunk_headers = if let Some(header) = value.hunk_header {
            vec![but_workspace::HunkHeader {
                old_start: header.old_start,
                old_lines: header.old_lines,
                new_start: header.new_start,
                new_lines: header.new_lines,
            }]
        } else {
            vec![]
        };
        but_workspace::DiffSpec {
            previous_path: None, // TODO
            path: value.path_bytes.clone(),
            hunk_headers,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
/// Indicates that the assignment request was rejected due to locking - the hunk depends on a commit in the stack it is currently in.
pub struct AssignmentRejection {
    /// The request that was rejected.
    request: HunkAssignmentRequest,
    /// The locks that caused the rejection.
    locks: Vec<HunkLock>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
/// A request to update a hunk assignment.
/// If a a file has multiple hunks, the UI client should send a list of assignment requests with the appropriate hunk headers.
pub struct HunkAssignmentRequest {
    /// The hunk that is being assigned. Together with path_bytes, this identifies the hunk.
    /// If the file is binary, or too large to load, this will be None and in this case the path name is the only identity.
    /// If the file has hunk headers, then header info MUST be provided.
    pub hunk_header: Option<HunkHeader>,
    /// The file path of the hunk in bytes.
    pub path_bytes: BString,
    /// The stack to which the hunk is assigned. If set to None, the hunk is set as "unassigned".
    /// If a stack id is set, it must be one of the applied stacks.
    pub stack_id: Option<StackId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Same as `but_core::ui::WorktreeChanges`, but with the addition of hunk assignments.
pub struct WorktreeChanges {
    #[serde(flatten)]
    pub worktree_changes: but_core::ui::WorktreeChanges,
    pub assignments: Vec<HunkAssignment>,
    pub assignments_error: Option<serde_error::Error>,
}

impl From<but_core::ui::WorktreeChanges> for WorktreeChanges {
    fn from(worktree_changes: but_core::ui::WorktreeChanges) -> Self {
        WorktreeChanges {
            worktree_changes,
            assignments: vec![],
            assignments_error: None,
        }
    }
}

impl From<but_core::WorktreeChanges> for WorktreeChanges {
    fn from(worktree_changes: but_core::WorktreeChanges) -> Self {
        let ui_changes: but_core::ui::WorktreeChanges = worktree_changes.into();
        ui_changes.into()
    }
}

impl HunkAssignmentRequest {
    pub fn matches_assignment(&self, assignment: &HunkAssignment) -> bool {
        self.path_bytes == assignment.path_bytes && self.hunk_header == assignment.hunk_header
    }
}

impl PartialEq for HunkAssignment {
    fn eq(&self, other: &Self) -> bool {
        self.hunk_header == other.hunk_header && self.path_bytes == other.path_bytes
    }
}

impl HunkAssignment {
    /// Whether there is overlap between the two hunks.
    fn intersects(&self, other: HunkAssignment) -> bool {
        if self == &other {
            return true;
        }
        if self.path_bytes != other.path_bytes {
            return false;
        }
        if self.hunk_header == other.hunk_header {
            return true;
        }
        if let (Some(header), Some(other_header)) = (self.hunk_header, other.hunk_header) {
            if header.new_start >= other_header.new_start
                && header.new_start <= other_header.new_start + other_header.new_lines
            {
                return true;
            }
            if other_header.new_start >= header.new_start
                && other_header.new_start <= header.new_start + header.new_lines
            {
                return true;
            }
        }
        false
    }
}

/// Sets the assignment for a hunk. It must be already present in the current assignments, errors out if it isn't.
/// If the stack is not in the list of applied stacks, it errors out.
/// Returns the updated assignments list.
pub fn assign(
    ctx: &mut CommandContext,
    requests: Vec<HunkAssignmentRequest>,
) -> Result<Vec<AssignmentRejection>> {
    let previous_assignments = state::assignments(ctx)?;
    let vb_state = VirtualBranchesHandle::new(ctx.project().gb_dir());
    let applied_stacks = vb_state
        .list_stacks_in_workspace()?
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();
    let new_assignments = set_assignment(
        &applied_stacks,
        previous_assignments.clone(),
        requests.clone(),
    )?;
    let deps_assignments = hunk_dependency_assignments(ctx, None)?;
    let assignments_considering_deps = reconcile::assignments(
        new_assignments,
        &deps_assignments,
        &applied_stacks,
        MultipleOverlapping::SetNone, // If there is double locking, move the hunk to the Uncommitted section
        false, // Dependencies should not update the assignment request if it is attemptint to set to None
    )?;
    state::set_assignments(ctx, assignments_considering_deps.clone())?;

    // Request where the stack_id is different from the outcome are considered rejections - this is due to locking
    // Collect all the rejected requests together with the locks that caused the rejection
    let mut rejections = vec![];
    for req in requests {
        let locks = assignments_considering_deps
            .iter()
            .filter(|assignment| {
                req.matches_assignment(assignment) && req.stack_id != assignment.stack_id
            })
            .flat_map(|assignment| assignment.hunk_locks.clone())
            .collect_vec();
        if !locks.is_empty() {
            rejections.push(AssignmentRejection {
                request: req.clone(),
                locks,
            });
        }
    }
    Ok(rejections)
}

/// Same as the `reconcile_with_worktree_and_locks` function, but if the operation produces an error, it will create a fallback set of assignments from the worktree changes alone.
/// An optional error is returned alongside the assignments, which will be `None` if the operation was successful and it will be set if the operation failed and a fallback was used.
///
/// The fallback can of course also fail, in which case the tauri operation will error out.
pub fn assignments_with_fallback(
    ctx: &mut CommandContext,
    set_assignment_from_locks: bool,
    worktree_changes: Option<impl IntoIterator<Item = impl Into<but_core::TreeChange>>>,
) -> Result<(Vec<HunkAssignment>, Option<anyhow::Error>)> {
    let repo = &ctx.gix_repo()?;
    let worktree_changes: Vec<but_core::TreeChange> = match worktree_changes {
        Some(wtc) => wtc.into_iter().map(Into::into).collect(),
        None => but_core::diff::worktree_changes(repo)?.changes,
    };
    if worktree_changes.is_empty() {
        return Ok((vec![], None));
    }
    let mut worktree_assignments = vec![];
    for change in &worktree_changes {
        let diff = change.unified_diff(repo, ctx.app_settings().context_lines)?;
        worktree_assignments.extend(diff_to_assignments(diff, change.path.clone()));
    }
    let reconciled = reconcile_with_worktree_and_locks(
        ctx,
        set_assignment_from_locks,
        &worktree_changes,
        &worktree_assignments,
    )
    .map(|a| (a, None))
    .unwrap_or_else(|e| (worktree_assignments, Some(e)));
    Ok(reconciled)
}

/// Returns the current hunk assignments for the workspace.
///
/// Reconciles the current hunk assignments with the current worktree changes.
/// It takes  the current hunk assignments as well as the current worktree changes, producing a new set of hunk assignments.
///
/// Inside, fuzzy matching is performed, such that hunks that were previously assigned
/// and now in they have been modified in the worktree are still assigned to the same stack.
///
/// If a hunk was deleted in the worktree, it is removed from the assignments list.
/// If a completely new hunk is added it is accounted for with a HunkAssignment with stack_id None.
///
/// If a stack is no longer present in the workspace (either unapplied or deleted), any assignments to it are removed.
///
/// If a hunk has a dependency on a particular stack and it has been previously assigned to another stack, the assignment is updated to reflect that dependency.
/// If a hunk has a dependency but it has not been previously assigned to any stack, it is left unassigned (stack_id is None). This is so that the hunk assignment workflow can remain optional.
///
/// This needs to be ran only after the worktree has changed.
///
/// If `worktree_changes` is `None`, they will be fetched automatically.
#[instrument(skip(ctx), err(Debug))]
fn reconcile_with_worktree_and_locks(
    ctx: &mut CommandContext,
    set_assignment_from_locks: bool,
    worktree_changes: &Vec<but_core::TreeChange>,
    worktree_assignments: &Vec<HunkAssignment>,
) -> Result<Vec<HunkAssignment>> {
    let vb_state = VirtualBranchesHandle::new(ctx.project().gb_dir());
    let applied_stacks = vb_state
        .list_stacks_in_workspace()?
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>();

    let persisted_assignments = state::assignments(ctx)?;
    let with_worktree = reconcile::assignments(
        worktree_assignments.clone(),
        &persisted_assignments,
        &applied_stacks,
        MultipleOverlapping::SetMostLines,
        true,
    )?;

    let lock_assignments = hunk_dependency_assignments(ctx, Some(worktree_changes.clone()))?;
    let with_locks = reconcile::assignments(
        with_worktree,
        &lock_assignments,
        &applied_stacks,
        MultipleOverlapping::SetNone,
        set_assignment_from_locks,
    )?;

    Ok(with_locks)
}

fn hunk_dependency_assignments(
    ctx: &CommandContext,
    worktree_changes: Option<Vec<but_core::TreeChange>>,
) -> Result<Vec<HunkAssignment>> {
    // NB(Performance): This will do some extra work - in particular, worktree_changes will be fetched again, but that is a fast operation.
    // Furthermore, this call will compute unified_diff for each change. While this is a slower operation, it is invoked with zero context lines,
    // and that seems appropriate for limiting locking to only real overlaps.
    let deps = hunk_dependencies_for_workspace_changes_by_worktree_dir(
        ctx,
        &ctx.project().path,
        &ctx.project().gb_dir(),
        worktree_changes,
    )?
    .diffs;
    let mut assignments = vec![];
    for (path, hunk, locks) in deps {
        // If there are locks towards more than one stack, this means double locking and the assignment None - the user can resolve this by partial committing.
        let locked_to_stack_ids_count = locks
            .iter()
            .map(|lock| lock.stack_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let stack_id = if locked_to_stack_ids_count == 1 {
            Some(locks[0].stack_id)
        } else {
            None
        };
        let assignment = HunkAssignment {
            id: None,
            hunk_header: Some(hunk.into()),
            path: path.clone(),
            path_bytes: path.into(),
            stack_id,
            hunk_locks: locks,
        };
        assignments.push(assignment);
    }
    Ok(assignments)
}

/// This also generates a UUID for the assignment
fn diff_to_assignments(diff: UnifiedDiff, path: BString) -> Vec<HunkAssignment> {
    let path_str = path.to_str_lossy();
    match diff {
        but_core::UnifiedDiff::Binary => vec![HunkAssignment {
            id: Some(Uuid::new_v4()),
            hunk_header: None,
            path: path_str.into(),
            path_bytes: path,
            stack_id: None,
            hunk_locks: vec![],
        }],
        but_core::UnifiedDiff::TooLarge { .. } => vec![HunkAssignment {
            id: Some(Uuid::new_v4()),
            hunk_header: None,
            path: path_str.into(),
            path_bytes: path,
            stack_id: None,
            hunk_locks: vec![],
        }],
        but_core::UnifiedDiff::Patch {
            hunks,
            is_result_of_binary_to_text_conversion,
            ..
        } => {
            // If there are no hunks, then the assignment is for the whole file
            if is_result_of_binary_to_text_conversion || hunks.is_empty() {
                vec![HunkAssignment {
                    id: Some(Uuid::new_v4()),
                    hunk_header: None,
                    path: path_str.into(),
                    path_bytes: path,
                    stack_id: None,
                    hunk_locks: vec![],
                }]
            } else {
                hunks
                    .iter()
                    .map(|hunk| HunkAssignment {
                        id: Some(Uuid::new_v4()),
                        hunk_header: Some(hunk.into()),
                        path: path_str.clone().into(),
                        path_bytes: path.clone(),
                        stack_id: None,
                        hunk_locks: vec![],
                    })
                    .collect()
            }
        }
    }
}

fn set_assignment(
    applied_stacks: &[StackId],
    mut previous_assignments: Vec<HunkAssignment>,
    new_assignments: Vec<HunkAssignmentRequest>,
) -> Result<Vec<HunkAssignment>> {
    for new_assignment in new_assignments {
        if let Some(stack_id) = new_assignment.stack_id {
            if !applied_stacks.contains(&stack_id) {
                return Err(anyhow::anyhow!("No such stack in the workspace"));
            }
        }
        if let Some(found) = previous_assignments
            .iter_mut()
            .find(|previous| new_assignment.matches_assignment(previous))
        {
            found.stack_id = new_assignment.stack_id;
        } else {
            return Err(anyhow::anyhow!("Hunk not found in current assignments"));
        }
    }
    Ok(previous_assignments)
}

#[cfg(test)]
mod tests {
    use crate::reconcile::MultipleOverlapping;

    use super::*;
    use bstr::BString;
    use but_workspace::{HunkHeader, StackId};

    impl HunkAssignment {
        pub fn new(
            path: &str,
            start: u32,
            end: u32,
            stack_id: Option<usize>,
            id: Option<usize>,
        ) -> HunkAssignment {
            HunkAssignment {
                id: id.map(id_seq),
                hunk_header: Some(HunkHeader {
                    old_start: 0,
                    old_lines: 0,
                    new_start: start,
                    new_lines: end,
                }),
                path: path.to_string(),
                path_bytes: BString::from(path),
                stack_id: stack_id.map(stack_id_seq),
                hunk_locks: vec![],
            }
        }
    }

    impl HunkAssignmentRequest {
        pub fn new(
            path: &str,
            start: u32,
            end: u32,
            stack_id: Option<usize>,
        ) -> HunkAssignmentRequest {
            HunkAssignmentRequest {
                hunk_header: Some(HunkHeader {
                    old_start: 0,
                    old_lines: 0,
                    new_start: start,
                    new_lines: end,
                }),
                path_bytes: BString::from(path),
                stack_id: stack_id.map(stack_id_seq),
            }
        }
    }

    fn stack_id_seq(num: usize) -> StackId {
        StackId::from(id_seq(num))
    }

    fn id_seq(num: usize) -> Uuid {
        assert!(num < 10);
        uuid::Uuid::parse_str(&format!("00000000-0000-0000-0000-00000000000{}", num % 10)).unwrap()
    }

    fn deep_eq(a: &HunkAssignment, b: &HunkAssignment) -> bool {
        a.id == b.id
            && a.hunk_header == b.hunk_header
            && a.path == b.path
            && a.path_bytes == b.path_bytes
            && a.stack_id == b.stack_id
            && a.hunk_locks == b.hunk_locks
    }
    fn assert_eq(a: Vec<HunkAssignment>, b: Vec<HunkAssignment>) {
        assert!(
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| deep_eq(x, y)),
            "HunkAssignment vectors are not deeply equal.\nLeft: {:#?}\nRight: {:#?}",
            a,
            b
        );
    }

    #[test]
    fn test_intersection() {
        assert!(
            !HunkAssignment::new("foo.rs", 10, 5, None, None)
                .intersects(HunkAssignment::new("foo.rs", 16, 5, None, None))
        );
        assert!(
            HunkAssignment::new("foo.rs", 10, 6, None, None)
                .intersects(HunkAssignment::new("foo.rs", 16, 5, None, None))
        );
        assert!(
            HunkAssignment::new("foo.rs", 10, 7, None, None)
                .intersects(HunkAssignment::new("foo.rs", 16, 5, None, None))
        );
    }

    #[test]
    fn test_reconcile_exact_match_and_no_intersection() {
        let previous_assignments = vec![HunkAssignment::new("foo.rs", 10, 5, Some(1), Some(1))];
        let worktree_assignments = vec![
            HunkAssignment::new("foo.rs", 10, 5, None, None),
            HunkAssignment::new("foo.rs", 20, 5, None, None),
        ];
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetMostLines,
            true,
        )
        .unwrap();
        assert_eq(
            result,
            vec![
                HunkAssignment::new("foo.rs", 10, 5, Some(1), Some(1)),
                HunkAssignment::new("foo.rs", 20, 5, None, None),
            ],
        );
    }

    #[test]
    fn test_reconcile_exact_match_unapplied_branch_unassigns() {
        let previous_assignments = vec![HunkAssignment::new("foo.rs", 10, 5, Some(1), Some(1))];
        let worktree_assignments = vec![HunkAssignment::new("foo.rs", 10, 5, None, Some(1))];
        let applied_stacks = vec![stack_id_seq(2)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetMostLines,
            true,
        )
        .unwrap();
        assert_eq(
            result,
            vec![HunkAssignment::new("foo.rs", 10, 5, None, Some(1))],
        );
    }

    #[test]
    fn test_reconcile_with_overlap_preserves_assignment() {
        let previous_assignments = vec![HunkAssignment::new("foo.rs", 10, 5, Some(1), Some(1))];
        let worktree_assignments = vec![HunkAssignment::new("foo.rs", 12, 7, None, Some(1))];
        let applied_stacks = vec![stack_id_seq(1)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetMostLines,
            true,
        )
        .unwrap();
        assert_eq(
            result,
            vec![HunkAssignment::new("foo.rs", 12, 7, Some(1), Some(1))],
        );
    }

    #[test]
    fn test_double_overlap_picks_the_bigger_previous_assignment() {
        let previous_assignments = vec![
            HunkAssignment::new("foo.rs", 1, 15, Some(1), Some(1)),
            HunkAssignment::new("foo.rs", 17, 20, Some(2), Some(2)),
        ];
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let worktree_assignments = vec![HunkAssignment::new("foo.rs", 5, 18, None, None)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetMostLines,
            true,
        )
        .unwrap();
        assert_eq(
            result,
            vec![HunkAssignment::new("foo.rs", 5, 18, Some(2), Some(2))],
        );
    }

    #[test]
    fn test_double_overlap_unassigns() {
        let previous_assignments = vec![
            HunkAssignment::new("foo.rs", 5, 15, Some(1), Some(1)),
            HunkAssignment::new("foo.rs", 17, 25, Some(2), Some(2)),
        ];
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let worktree_assignments = vec![HunkAssignment::new("foo.rs", 5, 18, None, None)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetNone,
            true,
        )
        .unwrap();
        assert_eq(
            result,
            vec![HunkAssignment::new("foo.rs", 5, 18, None, Some(2))],
        );
    }

    #[test]
    fn test_reconcile_not_updating_unassigned() {
        let previous_assignments = vec![HunkAssignment::new("foo.rs", 10, 15, Some(1), None)];
        let worktree_assignments = vec![HunkAssignment::new("foo.rs", 12, 17, Some(2), None)];
        let applied_stacks = vec![stack_id_seq(1)];
        let result = reconcile::assignments(
            worktree_assignments,
            &previous_assignments,
            &applied_stacks,
            MultipleOverlapping::SetMostLines,
            false,
        )
        .unwrap();
        // TODO: This is actually broken
        assert_eq!(
            result,
            vec![HunkAssignment::new("foo.rs", 12, 17, Some(1), None)]
        );
    }

    #[test]
    fn test_set_assignment_success() {
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let previous_assignments = vec![
            HunkAssignment::new("foo.rs", 10, 15, None, None),
            HunkAssignment::new("bar.rs", 20, 25, Some(1), None),
        ];

        // Assign foo.rs:10 to stack 2
        let new_assignment_req = HunkAssignmentRequest::new("foo.rs", 10, 15, Some(2));
        let new_assignment = HunkAssignment::new("foo.rs", 10, 15, Some(2), None);
        let updated = set_assignment(
            &applied_stacks,
            previous_assignments.clone(),
            vec![new_assignment_req],
        )
        .unwrap();

        // Should update the stack_id for foo.rs:10
        let found = updated.iter().find(|h| **h == new_assignment).unwrap();
        assert_eq!(found.stack_id, Some(stack_id_seq(2)));

        // Other assignments should remain unchanged
        let other = updated
            .iter()
            .find(|h| **h == HunkAssignment::new("bar.rs", 20, 25, Some(1), None))
            .unwrap();
        assert_eq!(other.stack_id, Some(stack_id_seq(1)));
    }

    #[test]
    fn test_set_assignment_stack_not_applied() {
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let previous_assignments = vec![
            HunkAssignment::new("foo.rs", 10, 15, None, None),
            HunkAssignment::new("bar.rs", 20, 25, Some(1), None),
        ];

        // Assign foo.rs:10 to stack 3 (not applied)
        let new_assignment = HunkAssignmentRequest::new("foo.rs", 10, 15, Some(3));
        let result = set_assignment(
            &applied_stacks,
            previous_assignments.clone(),
            vec![new_assignment.clone()],
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_set_assignment_hunk_not_found() {
        let applied_stacks = vec![stack_id_seq(1), stack_id_seq(2)];
        let previous_assignments = vec![
            HunkAssignment::new("foo.rs", 10, 15, None, None),
            HunkAssignment::new("bar.rs", 20, 25, Some(1), None),
        ];

        // Assign baz.rs:30 to stack 2 (not found)
        let new_assignment = HunkAssignmentRequest::new("baz.rs", 30, 35, Some(2));
        let result = set_assignment(
            &applied_stacks,
            previous_assignments.clone(),
            vec![new_assignment.clone()],
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_hunk_assignment_partial_eq() {
        let hunk1 = HunkAssignment::new("foo.rs", 10, 15, Some(1), Some(3));
        let hunk2 = HunkAssignment::new("foo.rs", 10, 15, Some(2), Some(4));
        assert_eq!(hunk1, hunk2);
    }

    #[test]
    fn test_hunk_assignment_partial_eq_different_path() {
        let hunk1 = HunkAssignment::new("foo.rs", 10, 15, Some(1), None);
        let hunk2 = HunkAssignment::new("bar.rs", 10, 15, Some(2), None);
        assert_ne!(hunk1, hunk2);
    }
}
