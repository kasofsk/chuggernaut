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
  /** the pending item is a claimed attempt — a human is actively working it,
   *  not passively awaited (spec §1.2 claims) */
  claimed?: boolean
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
  /** per-job work-task timeout override (duration string), layering over the
   *  type's resources.task_timeout; null → the type default applies */
  timeout: string | null
  /** per-job Work agent model override; wins over the job type, project
   *  (jobs/_defaults.yaml) and platform defaults; null → those apply */
  model: string | null
  /** a human has claimed the next work attempt (spec §1.2 claims);
   *  cleared when the attempt parks */
  claim_next?: boolean
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

/** Token accounting on agent-run results (Work, Agent eval, Triage). */
export interface TokenUsage {
  input_tokens: number
  output_tokens: number
  cache_read_tokens?: number | null
  cache_write_tokens?: number | null
}

/** submit_result structured payload from a Work agent. */
export interface WorkStructured {
  files_changed?: string[]
  notes?: string
}

/** One issue a review evaluator raised (structured.findings on a fail). */
export interface ReviewFinding {
  file?: string
  issue?: string
  suggestion?: string
}

/** Agent evaluator structured payload: notes on a pass, findings on a fail. */
export interface EvalStructured {
  notes?: string
  findings?: ReviewFinding[]
}

/** Closing report of a Work-phase task (agent, or a human-claimed attempt). */
export interface WorkResult {
  kind: 'Work'
  summary?: string | null
  structured?: WorkStructured | null
  token_usage?: TokenUsage | null
}

/** Verdict of an agent evaluator (e.g. `review`): pass/fail + structured. */
export interface EvalResult {
  kind: 'Agent'
  pass: boolean
  /** "not satisfiable by rework" — implies pass:false, skips remaining budget. */
  abort?: boolean
  structured?: EvalStructured | null
  token_usage?: TokenUsage | null
}

/** Verdict of a command/CI evaluator: exit code + (possibly long) output. */
export interface CommandResult {
  kind: 'Command'
  pass: boolean
  exit_code: number
  output: string
  structured?: Record<string, unknown> | null
}

/** An operator's resolution of a human task, mirrored back as the result. */
export interface HumanResult {
  kind: 'Human'
  pass: boolean
  abort?: boolean
  /** Optional operator note; backend persistence is landing separately. */
  summary?: string | null
  structured?: Record<string, unknown> | null
  action?: 'Retry' | 'Resolve' | 'Revoke' | null
  operator?: string
  resolved_at?: string
}

/** An advisory triage agent's written assessment (its own JobDetail section). */
export interface TriageResult {
  kind: 'Triage'
  assessment: string
  token_usage?: TokenUsage | null
}

/**
 * A task's closing report (types::TaskResult, §6.2). Discriminated on `kind`;
 * every field is optional-tolerant so shape drift never crashes the UI, and an
 * unrecognized `kind` falls through to a raw-JSON render at the call site.
 */
export type TaskResult =
  | WorkResult
  | EvalResult
  | CommandResult
  | HumanResult
  | TriageResult

export interface Task {
  id: number
  job_seq: number
  project: string
  /** the lifecycle phase that spawned the task. The named values are the ones
   *  the UI treats specially; the `string` arm keeps a future phase (e.g. a
   *  WrapUp command task, job #63) from being a type error — it renders as-is. */
  phase: 'Work' | 'Evaluation' | 'MergeGate' | 'Triage' | (string & {})
  cycle: number
  kind: TaskKind
  state: 'Pending' | 'Running' | 'Done' | 'Failed'
  attempt: number
  evaluator: string | null
  /** set on claimed attempts: the declared kind stays, a human performed it */
  performed_by?: 'human' | null
  container_id: string | null
  /// Agent tasks only: names the captured session transcript.
  session_id: string | null
  result: TaskResult | null
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
  // summary (work tasks only): the human's completion summary — flows into
  // the squash-merge commit body like an agent's submit_result.
  | { kind: 'Pass'; structured: unknown | null; summary?: string | null }
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
  /** Node health at snapshot time; false → unreachable, excluded from placement. */
  available: boolean
}

/** The dispatcher's runtime config snapshot (fleet + defaults + paths). */
export interface DispatcherSnapshot {
  nodes: WorkerNode[]
  agent_provider_default: string
  agent_model_default: string | null
  /** platform image for operator-dispatched triage agents; null → unavailable */
  triage_image: string | null
  repos_root: string
  repo_url_base: string
  nats_url: string
  nats_url_container: string | null
  channel_binary: string | null
  hook_bin: string | null
  secrets_encryption: boolean
  /** Whether the New Job "job wizard" LLM chat is configured; false → the UI
   *  falls back to manual title/description entry. */
  wizard_available?: boolean
}

/** One message in the job-wizard conversation. */
export interface WizardMessage {
  role: 'user' | 'assistant'
  content: string
}

/** A ticket draft the wizard proposes once it has enough to go on. */
export interface TicketDraft {
  title: string
  description: string
}

/** POST .../wizard — one wizard turn: the chat reply, and (once ready) the
 *  drafted ticket to pre-fill the form. */
export interface WizardTurn {
  reply: string
  draft: TicketDraft | null
  done: boolean
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
  createJob: (owner: string, project: string, body: { type: string; title?: string; description?: string; deps?: number[]; knowledge_tags?: string[]; eval?: Evaluator[]; timeout?: string; model?: string }) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs`, body),
  /** One turn of the New Job job-wizard chat: send the conversation so far,
   *  get the assistant's reply and (once ready) a ticket draft. */
  wizard: (owner: string, project: string, messages: WizardMessage[]) =>
    req<WizardTurn>('POST', `/api/v1/projects/${owner}/${project}/wizard`, { messages }),
  criteria: (owner: string, project: string, seq: number) =>
    req<JobCriteria>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}/criteria`),
  release: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/release`),
  revoke: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/revoke`),
  /** Claim the next work attempt for a human (spec §1.2 claims); 409 while an attempt is in flight. */
  claim: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/claim`),
  /** Clear a pending claim that has not materialized into a parked task yet. */
  unclaim: (owner: string, project: string, seq: number) =>
    req<Job>('DELETE', `/api/v1/projects/${owner}/${project}/jobs/${seq}/claim`),
  /** Dispatch an advisory triage agent over an Escalated/Stalled job (§1.2). */
  triage: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/triage`),

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
