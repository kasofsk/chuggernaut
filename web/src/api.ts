
import type {
  ArtifactKind,
  Attachment,
  AwaitingHuman,
  DiffPage,
  DiffResponse,
  EvaluatorInput,
  HealthStatus,
  JobCriteria,
  JobPatch,
  JobTypeDetail,
  JobTypeSummary,
  NodeCapacityAck,
  OriginStatus,
  PlatformConfig,
  ProjectConfig,
  TaskOutput,
} from './api/envelopes'
import type {
  ChannelUpdate,
  DesignEntry,
  DispatcherConfigSnapshot,
  GroupEntry,
  FleetStatus,
  Identity,
  Job as JobRecord,
  JobSummary,
  QueueSnapshot,
  Task,
  TaskResolution,
  TaskResult,
} from './api/types.gen'

export type {
  CapacitySource,
  CapacityState,
  ChannelOrigin,
  ChannelUpdate,
  DeployLeg,
  DeployReport,
  DesignEntry,
  Escalation,
  EscalationAction,
  Evaluator,
  EvaluatorType,
  FleetNode,
  FleetStatus,
  GroupEntry,
  GroupJob,
  GroupRollup,
  Identity,
  IdentityKind,
  Input,
  InputKind,
  JobState,
  JobType,
  LegStatus,
  OriginLink,
  PendingReason,
  Performer,
  Placement,
  ProjectRole,
  Provider,
  QueueEntry,
  QueueSnapshot,
  RefreshOutcome,
  RefreshResult,
  ReleaseState,
  ReleaseStatus,
  Resources,
  ReviewSpec,
  ReworkReason,
  SlotOccupant,
  Task,
  TaskKind,
  TaskPhase,
  TaskResolution,
  TaskResult,
  TaskState,
  TokenUsage,
  WorkSpec,
  WorkType,
  WorkerNode,
  WrapUpMode,
  WrapUpSpec,
} from './api/types.gen'

export type {
  ArtifactKind,
  Attachment,
  AwaitingHuman,
  DiffPage,
  DiffResponse,
  EvalStructured,
  EvaluatorInput,
  HealthStatus,
  JobCriteria,
  JobPatch,
  JobTypeDetail,
  JobTypeSummary,
  NodeCapacityAck,
  OriginStatus,
  PlatformConfig,
  ProjectConfig,
  ReviewFinding,
  TaskOutput,
  WorkStructured,
} from './api/envelopes'

/**
 * A job as the **list** endpoint serves it (`api.jobs`) — `types::JobSummary`,
 * every field of the stored record except the two heavy prose ones plus the
 * latest channel post. `description` and `cover_html` were 78% of the list
 * payload and no list consumer reads them, so they live on {@link JobFull}.
 *
 * This is the shape nearly the whole app works with, so it keeps the plain
 * name. Reaching for `description` on a list row is a type error rather than
 * an `undefined` at runtime — which is the point of the split.
 */
export type Job = JobSummary

/**
 * A job as the **single-job** endpoint serves it (`api.job`): the stored record
 * (`types::Job` — prose included, no channel post) plus one derived field the
 * dispatcher adds to the JSON envelope.
 */
export type JobFull = JobRecord & {
  /** derived server-side from the task log; null when no human action is
   *  pending. Hand-mirrored: `handlers::jobs_reply` inserts it into the
   *  serialized job rather than declaring it on `types::Job`, so no schema
   *  covers it. */
  awaiting_human?: AwaitingHuman | null
}

/** The latest channel post on a live job (spec §4.2), carried on the jobs
 *  *list* so the table can show a progress line without opening the event
 *  stream's history. Absent on terminal jobs and jobs that never posted. */
export type JobChannel = ChannelUpdate

/** The dispatcher's runtime config snapshot (fleet + defaults + paths). */
export type DispatcherSnapshot = DispatcherConfigSnapshot

/** One variant of a task's closing report, addressed by its `kind` tag. The
 *  generated {@link TaskResult} is the union; these name its arms so a
 *  component can take exactly the one it renders. */
export type WorkResult = Extract<TaskResult, { kind: 'Work' }>
export type CommandResult = Extract<TaskResult, { kind: 'Command' }>
export type EvalResult = Extract<TaskResult, { kind: 'Agent' }>
export type HumanResult = Extract<TaskResult, { kind: 'Human' }>
export type TriageResult = Extract<TaskResult, { kind: 'Triage' }>

/** Server-side cap on a single attachment (crates/api MAX_ATTACHMENT_BYTES):
 *  16 MiB. Mirrored here so the UI can reject an over-cap file before the PUT
 *  and word the error the same way the API does (413). */
export const MAX_ATTACHMENT_BYTES = 16 * 1024 * 1024

/** Page ceiling for one diff read (`api.diff`): 64 pages of ~256KB, so ~16MB.
 *  Past it the read throws instead of looping forever on a wedged cursor. */
const DIFF_PAGES_MAX = 64

/** Restart ceiling for one diff read (`api.diff`). The dispatcher regenerates
 *  the diff per page, so a running job's branch can move mid-read; a diff that
 *  keeps moving throws rather than splicing pages of different diffs. */
const DIFF_RESTARTS_MAX = 3

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

/** Progress hooks for a paged diff read: `onPartial` sees every prefix as it
 *  lands, and a true `cancelled()` between pages abandons the rest. */
export interface DiffReadHooks {
  /** the diff read so far, plus whether the read reached the end */
  onPartial?: (partial: DiffResponse, done: boolean) => void
  /** polled between pages; true stops the read at the prefix already read */
  cancelled?: () => boolean
}

/** One cursor pass for {@link api.diff}, or null when the diff changed under
 *  the cursor — every page carries a digest of the whole diff, and pages of two
 *  different diffs must never be concatenated. */
async function diffPagesRead(
  base: string,
  seq: number,
  hooks?: DiffReadHooks,
): Promise<DiffResponse | null> {
  let files: DiffResponse['files'] = []
  const chunks: string[] = []
  let since = 0
  let digest = ''
  for (let page = 0; page < DIFF_PAGES_MAX; page++) {
    const p = await req<DiffPage>('GET', `${base}?since=${since}`)
    if (page === 0) {
      files = p.files
      digest = p.digest
    } else if (p.digest !== digest) {
      return null
    }
    chunks.push(p.data)
    const read = { files, diff: chunks.join('') }
    hooks?.onPartial?.(read, p.done)
    if (p.done || hooks?.cancelled?.()) return read
    if (p.offset <= since) throw new Error(`diff for job ${seq} stopped advancing at byte ${since}`)
    since = p.offset
  }
  throw new Error(`diff for job ${seq} exceeds ${DIFF_PAGES_MAX} pages`)
}

export const api = {
  /** Dispatcher liveness (unauthenticated). Resolves the parsed body on 200;
   *  throws ApiError on 503 so the footer can name the failing component. */
  health: () => req<HealthStatus>('GET', '/api/v1/health'),
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
  /** Live fleet occupancy snapshot (admins only): per-node slot usage + queue
   *  depth (spec §3.1). Empty before the dispatcher publishes; 403 for non-admins. */
  fleet: () => req<FleetStatus>('GET', '/api/v1/platform/fleet'),
  /** Set one worker node's **desired** slot count (admins only; design #293 §3).
   *  Answers 202, not 200 — the dispatcher records the intent and starts the
   *  `set_slots` push without waiting on the node RPC, so convergence shows up in
   *  the next fleet snapshot rather than in this reply. 404 unknown node, 409 a
   *  docker-endpoint node (`DOCKER_NODES` owns those), 422 above the node's
   *  reported maximum. */
  setNodeCapacity: (node: string, slots: number) =>
    req<NodeCapacityAck>('PUT', `/api/v1/platform/fleet/${encodeURIComponent(node)}/capacity`, {
      slots,
    }),

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
  /** knowledge tags, each with the path it resolved to — read a tag's contents
   *  back by that path; `file` reads verbatim and both layouts exist (spec §1.1) */
  tags: (owner: string, project: string) =>
    req<{ name: string; path: string }[]>('GET', `/api/v1/projects/${owner}/${project}/tags`),
  /** Every document under `docs/design/` at default-branch HEAD, joined to the
   *  `design/{slug}` group its jobs carry (design #321 Decision 7). Repo-derived,
   *  so a design nobody has filed a job against is a row with an empty roll-up —
   *  the row `GET .../groups` cannot represent. The doc *body* is deliberately
   *  not joined in: read it back through {@link api.file} at the row's `path`. */
  designs: (owner: string, project: string) =>
    req<DesignEntry[]>('GET', `/api/v1/projects/${owner}/${project}/designs`),
  /** Every group a job in this project carries, with its roll-up (design #321
   *  Decision 7). Names come from the jobs and nowhere else, so a group with no
   *  members does not exist — the design registry above is what lists a design
   *  nobody has ticketed yet. */
  groups: (owner: string, project: string) =>
    req<GroupEntry[]>('GET', `/api/v1/projects/${owner}/${project}/groups`),
  jobType: (owner: string, project: string, name: string) =>
    req<JobTypeDetail>('GET', `/api/v1/projects/${owner}/${project}/job-types/${encodeURIComponent(name)}`),
  jobs: (owner: string, project: string) =>
    req<Job[]>('GET', `/api/v1/projects/${owner}/${project}/jobs`),
  job: (owner: string, project: string, seq: number) =>
    req<JobFull>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}`),
  /** `inputs` carries the values the job supplies for its type's declared
   *  `inputs:` (spec §1.1). Send it only when non-empty — a type declaring no
   *  inputs must produce the body it produces today, byte for byte. */
  createJob: (owner: string, project: string, body: { type: string; title?: string; description?: string; deps?: number[]; knowledge_tags?: string[]; groups?: string[]; eval?: EvaluatorInput[]; timeout?: string; model?: string; inputs?: Record<string, string>; draft?: boolean }) =>
    req<JobFull>('POST', `/api/v1/projects/${owner}/${project}/jobs`, body),
  /** Full-field replace of an editable Draft job; 409 once it has left Draft. */
  patchJob: (owner: string, project: string, seq: number, body: JobPatch) =>
    req<JobFull>('PATCH', `/api/v1/projects/${owner}/${project}/jobs/${seq}`, body),
  criteria: (owner: string, project: string, seq: number) =>
    req<JobCriteria>('GET', `/api/v1/projects/${owner}/${project}/jobs/${seq}/criteria`),
  release: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/release`),
  /** Finalize an edited Draft back to Frozen (#166): validation runs, then it parks re-batchable; 409 outside Draft. */
  finalize: (owner: string, project: string, seq: number) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/finalize`),
  /** Edit a Draft batch's member list while composing it (§2.1 draft batches):
   *  `{ add?, remove? }` couple/uncouple candidate jobs; returns the updated
   *  batch. 409 unless the job is a Draft batch; 422 on an ineligible add. */
  members: (owner: string, project: string, seq: number, body: { add?: number[]; remove?: number[] }) =>
    req<Job>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/members`, body),
  /** Add/remove a job's group labels (design #321 Decision 5). Accepted in
   *  **every** state, terminal included — a group is an operator annotation,
   *  inert to execution, so the primary case is annotating a job that already
   *  finished. Add/remove rather than a whole-list replace, so two operators
   *  grouping the same job from two tabs both land. 422 on a malformed name. */
  jobGroups: (owner: string, project: string, seq: number, body: { add?: string[]; remove?: string[] }) =>
    req<Job>('PUT', `/api/v1/projects/${owner}/${project}/jobs/${seq}/groups`, body),
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
  /** Capacity launch-queue snapshot for the "queued" badge (spec §3.5). */
  queue: (owner: string, project: string) =>
    req<QueueSnapshot>('GET', `/api/v1/projects/${owner}/${project}/queue`),
  resolve: (owner: string, project: string, seq: number, taskId: number, resolution: TaskResolution) =>
    req<unknown>('POST', `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/resolve`, resolution),

  /** A job's whole diff, read as {@link DiffPage} chunks until `done` and
   *  re-read from the start when the job branch moves mid-read; bounded by
   *  {@link DIFF_PAGES_MAX} and {@link DIFF_RESTARTS_MAX}. `hooks` render a
   *  long diff progressively — each prefix reaches `onPartial`, and a true
   *  `cancelled()` ends the read at the prefix read so far. */
  diff: async (
    owner: string,
    project: string,
    seq: number,
    hooks?: DiffReadHooks,
  ): Promise<DiffResponse> => {
    const base = `/api/v1/projects/${owner}/${project}/diff/${seq}`
    for (let attempt = 0; attempt < DIFF_RESTARTS_MAX; attempt++) {
      const whole = await diffPagesRead(base, seq, hooks)
      if (whole) return whole
    }
    throw new Error(`diff for job ${seq} kept changing while it was read`)
  },

  /** A task's stdout tail from `since` (byte offset); see {@link TaskOutput}. */
  taskOutput: (owner: string, project: string, seq: number, taskId: number, since: number) =>
    req<TaskOutput>(
      'GET',
      `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/output?since=${since}`,
    ),

  artifacts: (owner: string, project: string, seq: number, taskId: number) =>
    req<{ artifacts: ArtifactKind[] }>(
      'GET',
      `/api/v1/projects/${owner}/${project}/jobs/${seq}/tasks/${taskId}/artifacts`,
    ),
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

  /** Files uploaded to a job, sorted by name. Empty when storage is unconfigured. */
  attachments: (owner: string, project: string, seq: number) =>
    req<{ attachments: Attachment[] }>(
      'GET',
      `/api/v1/projects/${owner}/${project}/jobs/${seq}/attachments`,
    ).then((r) => r.attachments),
  /** Download URL for one attachment; served as raw bytes under its stored
   *  content type, so it bypasses req() (use it as an <img> src or href). */
  attachmentUrl: (owner: string, project: string, seq: number, name: string) =>
    `/api/v1/projects/${owner}/${project}/jobs/${seq}/attachments/${encodeURIComponent(name)}`,
  deleteAttachment: (owner: string, project: string, seq: number, name: string) =>
    req<unknown>('DELETE', api.attachmentUrl(owner, project, seq, name)),
  /**
   * Upload (or replace) an attachment. The raw file bytes are the request body
   * and the blob's MIME type rides the Content-Type header. Uses XHR (not
   * fetch) so `onProgress` can report upload fraction for the progress bar. A
   * non-2xx status rejects with {@link ApiError} carrying the parsed body — a
   * 413 is the over-cap case, surfaced verbatim.
   */
  putAttachment: (
    owner: string,
    project: string,
    seq: number,
    name: string,
    file: Blob,
    onProgress?: (fraction: number) => void,
  ): Promise<Attachment> =>
    new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest()
      xhr.open('PUT', api.attachmentUrl(owner, project, seq, name))
      xhr.responseType = 'text'
      if (file.type) xhr.setRequestHeader('Content-Type', file.type)
      xhr.upload.onprogress = (e) => {
        if (onProgress && e.lengthComputable) onProgress(e.loaded / e.total)
      }
      const parse = (): unknown => {
        try {
          return xhr.responseText ? JSON.parse(xhr.responseText) : null
        } catch {
          return null
        }
      }
      xhr.onload = () => {
        const body = parse()
        if (xhr.status >= 200 && xhr.status < 300) resolve(body as Attachment)
        else reject(new ApiError(xhr.status, body))
      }
      xhr.onerror = () => reject(new ApiError(0, null))
      xhr.send(file)
    }),
}
