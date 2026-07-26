// The part of the §6.2 wire surface that is still hand-mirrored, and why.
//
// Everything the Rust `types` crate names is generated into `types.gen.ts`.
// These shapes are not in it because no Rust type describes them: the
// dispatcher assembles each of these replies with `serde_json::json!` at the
// handler, so `chuggernaut schema api` has nothing to derive a schema from.
// They are the remaining drift surface — the compiler cannot tell you when the
// backend changes one — and the fix for any entry here is to name it in Rust
// and cover it in `cli::schema::api_bundle`, not to transcribe it more
// carefully.
//
// Where an envelope merely *wraps* a covered type, it wraps the generated one:
// `JobTypeDetail.job_type` is the generated `JobType`, `OriginStatus.origin` is
// the generated `OriginLink`.

import type { DispatcherConfigSnapshot, Evaluator, JobType, OriginLink, ReleaseState } from './types.gen'

/** Derived (never stored): the pending human task a job is waiting on, if any.
 *  `handlers::jobs_reply` inserts it into the serialized job rather than
 *  declaring it on `types::Job`. */
export interface AwaitingHuman {
  task_id: number
  kind: 'work' | 'eval' | 'escalation'
  /** the pending item is a claimed attempt — a human is actively working it,
   *  not passively awaited (spec §1.2 claims) */
  claimed?: boolean
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
  job_type: JobType | null
  errors: string[]
}

/** submit_result structured payload from a Work agent. `structured` is a
 *  `serde_json::Value` on the wire — `unknown` once generated — so its inner
 *  shapes are hand-written and must be read through a tolerant parser. */
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

export type ArtifactKind = 'session.jsonl' | 'stdout.log'

/**
 * GET .../tasks/{id}/output?since=<byte offset> — a task's stdout as a resumable
 * tail. While the container runs it returns the live tail (`running: true`);
 * after exit it serves the harvested stdout.log at the *same* byte offsets
 * (`running: false`), so a poller that keeps handing `offset` back never drops a
 * line across the container-exit transition. 404 before a container exists (a
 * parked human task, a Pending launch); 502 if the owning node is unreachable.
 */
export interface TaskOutput {
  /** the new end offset — pass as `since` on the next poll to resume gaplessly */
  offset: number
  /** bytes since the requested offset (may be empty on a quiet tail) */
  data: string
  /** true while the container runs (live tail); false once serving the artifact */
  running: boolean
}

export interface DiffResponse {
  files: { path: string; additions: number; deletions: number }[]
  diff: string
}

/** GET .../origin — linked-origin state (404 on classic projects). Typed in
 *  Rust as `forge_ingest::origin::OriginStatusResponse`, but that type lives in
 *  `dispatcher`, which the schema emitter does not depend on. */
export interface OriginStatus {
  origin: OriginLink | null
  release: ReleaseState | null
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
  origin: OriginLink | null
  /** presence of the reserved origin-credential secrets */
  origin_credentials: { deploy_key: boolean; pat: boolean }
}

/** GET /platform/config — read-only platform settings (admins only). */
export interface PlatformConfig {
  /** null when the dispatcher hasn't published a snapshot (offline/older) */
  dispatcher: DispatcherConfigSnapshot | null
  /** global/agents secret NAMES injected into every agent container */
  agent_secrets: string[]
  vapid_public: boolean
}

/**
 * PATCH .../jobs/{seq} — full-field replace of an editable Draft (spec §72).
 * Every field is sent on every call; the server rejects the PATCH with 409 once
 * the job has left Draft. `eval` round-trips unchanged (the Draft editor doesn't
 * expose it) so it isn't wiped by the full replace. The body is a
 * `serde_json::Value` server-side, so there is no Rust type behind it.
 */
export interface JobPatch {
  type: string
  title: string
  description: string
  deps: number[]
  knowledge_tags: string[]
  eval: EvaluatorInput[]
  /** duration string override, or null for the type default */
  timeout: string | null
  /** work-model override, or null for the resolved default */
  model: string | null
}

/**
 * An evaluator as a **request** carries it. Job create/patch bodies are
 * `serde_json::Value` server-side, and the dispatcher fills the same serde
 * defaults the YAML parser does — so a caller sends name/type and omits the
 * rest, where a served {@link Evaluator} always carries every field.
 */
export type EvaluatorInput = Partial<Evaluator> & Pick<Evaluator, 'name' | 'type'>

/** One operator-uploaded job attachment (spec §1.6): a screenshot or reference
 *  file. Presentational reference material — never injected into an agent
 *  prompt. `content_type` is the stored MIME type echoed on download. */
export interface Attachment {
  name: string
  content_type: string
  size: number
}

/** GET /api/v1/health — unauthenticated dispatcher liveness probe (spec §6.x).
 *  200 → `{dispatcher:'ok', version}`; 503 → `{dispatcher:'error'|…, error}`. The
 *  status vote source for the system-status footer (#163). */
export interface HealthStatus {
  dispatcher: string
  version?: string
  /** The api process's own build SHA (`CHUG_GIT_SHA`), independent of the
   *  dispatcher `version` — the cluster view shows it so an api restarted onto a
   *  different commit than the dispatcher reads as skew. Absent on local/dev. */
  api_sha?: string | null
  error?: string
}
