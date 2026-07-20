import { Link } from 'react-router-dom'
import type { Evaluator } from '../api'

/**
 * Evaluation criteria as a table — the tasks the evaluation phase will run.
 * Used by the job detail criteria card (with source column) and the library.
 */
export function EvaluatorTable({
  owner,
  project,
  evaluators,
  showSource,
}: {
  owner: string
  project: string
  evaluators: (Evaluator & { source?: 'type' | 'job' })[]
  showSource?: boolean
}) {
  if (evaluators.length === 0) {
    return <div className="dim">no evaluators — evaluation auto-passes</div>
  }
  return (
    <table className="jobs">
      <thead>
        <tr>
          <th>name</th>
          <th>task</th>
          <th>action</th>
          <th>gate</th>
          {showSource && <th>source</th>}
        </tr>
      </thead>
      <tbody>
        {evaluators.map((e, i) => (
          <tr key={`${e.source ?? ''}:${e.name}:${i}`}>
            <td>{e.name}</td>
            <td>{e.type}</td>
            <td className="dim">
              {e.type === 'command' ? (
                <code>{e.run}</code>
              ) : e.prompt ? (
                <Link to={`/p/${owner}/${project}/files?path=${encodeURIComponent(e.prompt)}`}>
                  {e.prompt} ↗
                </Link>
              ) : null}
              {e.model ? ` · ${e.model}` : ''}
            </td>
            <td>
              {e.required === false ? (
                <span className="badge badge-gray">advisory</span>
              ) : (
                <span className="badge badge-blue">required</span>
              )}
            </td>
            {showSource && (
              <td>
                {e.source === 'job' ? (
                  <span className="badge badge-purple">this job</span>
                ) : (
                  <span className="dim">type</span>
                )}
              </td>
            )}
          </tr>
        ))}
      </tbody>
    </table>
  )
}
