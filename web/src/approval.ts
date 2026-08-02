/**
 * The per-job approval gate (spec §1.1 require-approval), UI side.
 *
 * The gate is not a new mechanism: it is one synthesized required Human
 * evaluator, reserved under the name `approval` and staged after every other
 * criterion, so it resolves through the ordinary task inbox. This module holds
 * the one name and the one state rule the UI needs, so no page hard-codes them.
 */
import type { Job, Task } from './api'

/** The reserved evaluator name the dispatcher synthesizes the gate under. */
export const APPROVAL_EVALUATOR = 'approval'

/** True when `t` is the synthesized approval gate rather than a declared task. */
export function isApprovalTask(t: Task): boolean {
  return t.phase === 'Evaluation' && t.evaluator === APPROVAL_EVALUATOR
}

/** The states in which the dispatcher still accepts an approval-gate edit —
 *  everything before Work entry, where criteria are resolved (422 after). */
export function approvalIsEditable(job: Job): boolean {
  return ['Draft', 'Frozen', 'Blocked', 'Ready', 'Stalled'].includes(job.state)
}
