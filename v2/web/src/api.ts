// Typed client for the §6.2 HTTP surface. Cookies ride automatically
// (same-origin); a 401 anywhere bounces to /login via the caller.

export type JobState =
  | 'Frozen'
  | 'Blocked'
  | 'Ready'
  | 'Work'
  | 'Evaluation'
  | 'Escalated'
  | 'Done'
  | 'Revoked'

export interface Job {
  id: number
  project: string
  type: string
  inputs: Record<string, number>
  state: JobState
  branch: string
  base_ref: string | null
  knowledge_tags: string[]
  factory: string | null
  created_at: string
  ready_at: string | null
}

export type TaskKind =
  | { kind: 'Command'; run: string }
  | { kind: 'Agent'; provider: string; model: string | null; prompt: string }
  | { kind: 'Human'; prompt: string }

export interface Task {
  id: number
  job_seq: number
  project: string
  phase: 'Work' | 'Evaluation' | 'MergeGate'
  cycle: number
  kind: TaskKind
  state: 'Pending' | 'Running' | 'Done' | 'Failed'
  attempt: number
  evaluator: string | null
  container_id: string | null
  result: Record<string, unknown> | null
  created_at: string
  started_at: string | null
  completed_at: string | null
}

export interface Identity {
  sub: string
  kind: 'user' | 'dispatcher'
  project_roles: Record<string, string>
  platform_admin: boolean
}

export interface DiffResponse {
  files: { path: string; additions: number; deletions: number }[]
  diff: string
}

export type TaskResolution =
  | { kind: 'Pass'; structured: unknown | null }
  | { kind: 'Fail'; structured: unknown }
  | { kind: 'Escalation'; action: 'Retry' | 'Resolve' | 'Revoke'; structured: unknown | null }

export class ApiError extends Error {
  status: number
  body: unknown
  constructor(status: number, body: unknown) {
    super(typeof body === 'object' && body && 'error' in body ? String((body as { error: unknown }).error) : `HTTP ${status}`)
    this.status = status
    this.body = body
  }
}

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })
  const text = await res.text()
  const json = text ? JSON.parse(text) : null
  if (!res.ok) throw new ApiError(res.status, json)
  return json as T
}

export const api = {
  login: (email: string, password: string) =>
    req<Identity>('POST', '/auth/login', { email, password }),
  logout: () => req<unknown>('POST', '/auth/logout'),
  me: () => req<Identity>('GET', '/auth/me'),

  jobs: (owner: string, project: string) =>
    req<Job[]>('GET', `/api/v1/projects/${owner}/${project}/jobs`),
  job: (owner: string, project: string, seq: number) =>
    req<Job>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}`),
  createJob: (owner: string, project: string, body: { type: string; inputs?: Record<string, number>; knowledge_tags?: string[] }) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs`, body),
  release: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/release`),
  revoke: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/revoke`),

  pendingTasks: (owner: string, project: string) =>
    req<Task[]>('GET', `/api/v1/projects/${owner}/${project}/tasks/pending`),
  tasks: (owner: string, project: string, seq: number) =>
    req<Task[]>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks`),
  resolve: (owner: string, project: string, seq: number, taskId: number, resolution: TaskResolution) =>
    req<unknown>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/resolve`, resolution),

  diff: (owner: string, project: string, seq: number) =>
    req<DiffResponse>('GET', `/api/v1/projects/${owner}/${project}/diff/${seq}`),
}
