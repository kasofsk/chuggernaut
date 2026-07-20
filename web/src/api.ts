// Typed client for the §6.2 HTTP surface. Cookies ride automatically
// (same-origin); a 401 anywhere bounces to /login via the caller.

export type JobState =
  | 'Frozen'
  | 'Blocked'
  | 'Ready'
  | 'Work'
  | 'Evaluation'
  | 'WrapUp'
  | 'Escalated'
  | 'Stalled'
  | 'Done'
  | 'Revoked'

/** Derived (never stored): the pending human task a job is waiting on, if any. */
export interface AwaitingHuman {
  task_id: number
  kind: 'work' | 'eval' | 'escalation'
}

export interface Evaluator {
  name: string
  type: 'command' | 'agent' | 'human'
  image?: string | null
  run?: string | null
  prompt?: string | null
  provider?: 'claude' | 'codex' | null
  model?: string | null
  secrets?: string[]
  /** default true; false = advisory */
  required?: boolean | null
}

/** GET .../jobs/{seq}/criteria — the criteria the job is judged against. */
export interface JobCriteria {
  /** the ref the criteria were resolved at (base_ref, or default HEAD before Ready) */
  ref: string
  /** wrap-up mode from the job type; null when the type failed to load */
  wrap_up: 'merge' | 'none' | null
  evaluators: (Evaluator & { source: 'type' | 'job' })[]
  errors: string[]
}

/** GET .../job-types — the type picker's vocabulary. */
export interface JobTypeSummary {
  /** file stem — the wire identifier (jobs/{name}.yaml) */
  name: string
  /** human-facing name; falls back to the stem */
  display_name: string
  /** one-line summary; may be empty */
  description: string
}

/** GET .../job-types/{name} — one job type in full, for the library. */
export interface JobTypeDetail {
  name: string
  /** default-branch HEAD the definition was read at */
  ref: string
  /** the file as authored */
  yaml: string
  /** parsed, with jobs/_defaults.yaml merged — what actually runs; null on parse failure */
  job_type: {
    name: string
    display_name: string | null
    description: string | null
    image: string | null
    wrap_up: { type: 'merge' | 'none' }
    work: {
      type: 'agent' | 'command' | 'human'
      prompt: string | null
      run: string | null
      provider: 'claude' | 'codex' | null
      model: string | null
      review: { prompt: string; iterations?: number | null } | null
      secrets: string[]
    }
    resources: { cpu: number | null; memory: string | null; task_timeout: string | null } | null
    job_deadline: string | null
    work_retries: number | null
    eval_retries: number | null
    rework_budget: number | null
    eval: Evaluator[]
    knowledge: string[]
    vars: string[]
  } | null
  errors: string[]
}

export interface Job {
  id: number
  project: string
  type: string
  /** ticket-style identity: what this run is for (may be empty) */
  title: string
  description: string
  /** upstream job ids that must be Done before this job starts */
  deps: number[]
  state: JobState
  branch: string
  base_ref: string | null
  knowledge_tags: string[]
  /** additive per-job evaluators, layered on top of the type's list */
  eval: Evaluator[]
  factory: string | null
  created_at: string
  ready_at: string | null
  /** derived server-side from the task log; null when no human action is pending */
  awaiting_human?: AwaitingHuman | null
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
  /// Agent tasks only: names the captured session transcript.
  session_id: string | null
  result: Record<string, unknown> | null
  created_at: string
  started_at: string | null
  completed_at: string | null
}

export type ArtifactKind = 'session.jsonl' | 'stdout.log'

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
  // abort (evaluator tasks only): "not satisfiable by rework" — skips the
  // remaining rework budget and escalates.
  | { kind: 'Fail'; structured: unknown; abort?: boolean }
  | { kind: 'Escalation'; action: 'Retry' | 'Resolve' | 'Revoke'; structured: unknown | null }

/** GET .../origin — linked-origin state (404 on classic projects). */
export interface OriginStatus {
  origin: { url: string; main_branch: string; github_repo: string | null } | null
  release: {
    number: number
    pr_number: number
    pr_url: string
    base_main_sha: string
    integration_sha: string
    status: 'open' | 'merged' | 'closed'
  } | null
  release_counter: number
  origin_main_sha: string | null
  integration_sha: string | null
  /** commits on integration not yet on origin main — the unreleased backlog */
  ahead_by: number
  /** merge queue held by an open release PR */
  held: boolean
}

/** GET .../config — read-only project settings overview. */
export interface ProjectConfig {
  vars: { name: string; value: string }[]
  /** secret NAMES only — values never leave the dispatcher */
  secrets: string[]
  /** the linked git origin, or null on a classic self-hosted project */
  origin: { url: string; main_branch: string; github_repo: string | null } | null
  /** presence of the reserved origin-credential secrets */
  origin_credentials: { deploy_key: boolean; pat: boolean }
}

export interface WorkerNode {
  name: string
  endpoint: string
  slots: number
}

/** The dispatcher's runtime config snapshot (fleet + defaults + paths). */
export interface DispatcherSnapshot {
  nodes: WorkerNode[]
  agent_provider_default: string
  agent_model_default: string | null
  repos_root: string
  repo_url_base: string
  nats_url: string
  nats_url_container: string | null
  channel_binary: string | null
  hook_bin: string | null
  secrets_encryption: boolean
}

/** GET /platform/config — read-only platform settings (admins only). */
export interface PlatformConfig {
  /** null when the dispatcher hasn't published a snapshot (offline/older) */
  dispatcher: DispatcherSnapshot | null
  /** global/agents secret NAMES injected into every agent container */
  agent_secrets: string[]
  vapid_public: boolean
}

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
  projects: () => req<string[]>('GET', '/api/v1/projects'),
  /** Platform admins only: bare repo + hook + Code starter template. */
  createProject: (owner: string, name: string) =>
    req<{ project: string }>('POST', '/api/v1/projects', { owner, name }),

  /** Linked-origin state; throws 404 on classic projects. */
  origin: (owner: string, project: string) =>
    req<OriginStatus>('GET', `/api/v1/projects/${owner}/${project}/origin`),
  originRelease: (owner: string, project: string) =>
    req<unknown>('POST', `/api/v1/projects/${owner}/${project}/origin/release`),
  originSync: (owner: string, project: string) =>
    req<OriginStatus>('POST', `/api/v1/projects/${owner}/${project}/origin/sync`),

  /** Read-only project settings: vars, secret names, origin link + creds. */
  projectConfig: (owner: string, project: string) =>
    req<ProjectConfig>('GET', `/api/v1/projects/${owner}/${project}/config`),
  /** Read-only platform settings (admins only): fleet, defaults, agent creds. */
  platformConfig: () => req<PlatformConfig>('GET', '/api/v1/platform/config'),

  jobTypes: (owner: string, project: string) =>
    req<JobTypeSummary[]>('GET', `/api/v1/projects/${owner}/${project}/job-types`),
  tree: (owner: string, project: string) =>
    req<{ branch: string; ref: string; entries: { path: string; type: string; size: number | null }[] }>(
      'GET',
      `/api/v1/projects/${owner}/${project}/tree`,
    ),
  file: (owner: string, project: string, path: string) =>
    req<{ path: string; ref: string; content: string }>(
      'GET',
      `/api/v1/projects/${owner}/${project}/file?path=${encodeURIComponent(path)}`,
    ),
  tags: (owner: string, project: string) =>
    req<string[]>('GET', `/api/v1/projects/${owner}/${project}/tags`),
  jobType: (owner: string, project: string, name: string) =>
    req<JobTypeDetail>('GET', `/api/v1/projects/${owner}/${project}/job-types/${encodeURIComponent(name)}`),
  jobs: (owner: string, project: string) =>
    req<Job[]>('GET', `/api/v1/projects/${owner}/${project}/jobs`),
  job: (owner: string, project: string, seq: number) =>
    req<Job>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}`),
  createJob: (owner: string, project: string, body: { type: string; title?: string; description?: string; deps?: number[]; knowledge_tags?: string[]; eval?: Evaluator[] }) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs`, body),
  criteria: (owner: string, project: string, seq: number) =>
    req<JobCriteria>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}/criteria`),
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

  artifacts: (owner: string, project: string, seq: number, taskId: number) =>
    req<{ artifacts: ArtifactKind[] }>(
      'GET',
      `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/artifacts`,
    ),
  // Artifacts are bytes (JSONL / plain text), not JSON, so they bypass req().
  artifactUrl: (owner: string, project: string, seq: number, taskId: number, kind: ArtifactKind) =>
    `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/artifacts/${kind}`,
  artifactText: async (
    owner: string,
    project: string,
    seq: number,
    taskId: number,
    kind: ArtifactKind,
  ): Promise<string> => {
    const res = await fetch(
      api.artifactUrl(owner, project, seq, taskId, kind),
      { credentials: 'same-origin' },
    )
    if (!res.ok) throw new ApiError(res.status, null)
    return res.text()
  },
}
