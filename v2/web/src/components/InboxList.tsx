import { Link } from 'react-router-dom'
import type { Job, Task, TaskResolution } from '../api'
import { ResolveForm } from './ResolveForm'

// The pending-Human-task list — the operator's primary surface (Part 11).
// Rendered inline on the desktop Jobs page and full-page on the Inbox tab.
export function InboxList({
  owner,
  project,
  jobs,
  pending,
  onResolve,
}: {
  owner: string
  project: string
  jobs: Job[]
  pending: Task[]
  onResolve: (task: Task, r: TaskResolution) => void
}) {
  const jobBySeq = new Map(jobs.map((j) => [j.id, j]))
  return (
    <>
      {pending.map((t) => (
        <div className="inbox-task" key={`${t.job_seq}:${t.id}`}>
          <div className="inbox-head">
            <Link to={`/p/${owner}/${project}/jobs/${t.job_seq}`}>#{t.job_seq}</Link>
            <span className="dim">
              {' '}
              · task {t.id} · {t.phase}
              {t.evaluator ? ` · ${t.evaluator}` : ''}
            </span>
            {jobBySeq.get(t.job_seq)?.title && (
              <span className="dim"> · {jobBySeq.get(t.job_seq)!.title}</span>
            )}
          </div>
          {t.kind.kind === 'Human' && <pre className="prompt">{t.kind.prompt}</pre>}
          <ResolveForm
            escalation={jobBySeq.get(t.job_seq)?.state === 'Escalated'}
            evaluator={t.phase === 'Evaluation'}
            onResolve={(r) => onResolve(t, r)}
          />
        </div>
      ))}
    </>
  )
}
