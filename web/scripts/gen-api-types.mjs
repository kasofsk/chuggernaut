// Generate `src/api/types.gen.ts` from the committed `.chug/schemas/api.schema.json`
// (emitted by `chuggernaut schema api`, spec §6.2) — the TypeScript half of the
// generated wire contract, NORTH-STAR §2.
//
//   node scripts/gen-api-types.mjs           # write the file
//   node scripts/gen-api-types.mjs --check   # fail if the committed file is stale
//
// The `--check` mode is the TS mirror of the Rust `committed_schemas_are_current`
// test: a generated file that is committed but not regenerated is exactly the
// drift this whole track exists to eliminate, so CI regenerates and compares
// rather than trusting whoever last touched a Rust type.
//
// Why json-schema-to-typescript: the bundle is plain JSON Schema (2020-12) with
// `$defs` + `$ref`, which is the format this generator takes natively — no
// OpenAPI wrapper document to fabricate (as `openapi-typescript` would need),
// no inference from samples (as `quicktype` does), and no runtime validator
// layer we would not use (`json-schema-to-zod`). It also carries the Rust doc
// comments through as TSDoc, which is most of the value of the hand-mirrored
// file it replaces.

import { readFile, writeFile, mkdir } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { compile } from 'json-schema-to-typescript'
import * as prettier from 'prettier'

const webRoot = join(dirname(fileURLToPath(import.meta.url)), '..')
const schemaPath = join(webRoot, '..', '.chug', 'schemas', 'api.schema.json')
const outPath = join(webRoot, 'src', 'api', 'types.gen.ts')
const samplesPath = join(webRoot, 'src', 'api', 'wire-samples.json')
const samplesOutPath = join(webRoot, 'src', 'api', 'wire-samples.gen.ts')

const BANNER = `/**
 * GENERATED — DO NOT EDIT.
 *
 * The §6.2 HTTP surface as TypeScript, generated from .chug/schemas/api.schema.json
 * (itself generated from the Rust \`types\` crate by \`chuggernaut schema api\`).
 * Regenerate with \`npm run codegen\`; \`npm run codegen:check\` fails CI when
 * this file is stale.
 *
 * The shapes that stay hand-written are in ./envelopes.ts: the replies the
 * dispatcher assembles with \`serde_json::json!\`, for which no Rust type — and
 * so no schema — exists. ../api.ts re-exports both halves.
 */`

// Name of the synthetic root the generator compiles: every `$defs` entry
// referenced from one object, so all 51 named types are reachable in a single
// pass. Its own interface is stripped from the output below.
const ROOT_TITLE = 'GeneratedApiSchemaRoot'

// Annotation keywords schemars puts *beside* a `$ref` (a field documented at
// its use site, a serde default). JSON Schema allows the pairing, but the
// generator reads `$ref` + siblings as a schema of its own and emits a second,
// identically-shaped interface for it (`WrapUpSpec` next to `WrapUpSpec1` —
// a copy-paste-gate failure, and a coin flip over which one a consumer
// imports). Since these keywords are annotations — they constrain nothing —
// the reference reduces to the bare `$ref`. Anything else beside a `$ref`
// would be a real constraint we would be silently discarding, so it fails the
// run instead.
const REF_ANNOTATIONS = new Set(['description', 'default', 'title', 'examples', 'deprecated'])

// Bounded because everything is: a schema is a finite tree, so a walk that
// reaches this depth has found a cycle the rest of this script would hang on.
const REF_DEPTH_MAX = 32

function bareRefs(node, path = '#', depth = 0) {
  if (depth > REF_DEPTH_MAX) throw new Error(`${path}: schema nests deeper than ${REF_DEPTH_MAX}`)
  const recurse = (v, k) => bareRefs(v, `${path}/${k}`, depth + 1)
  if (Array.isArray(node)) return node.map(recurse)
  if (node === null || typeof node !== 'object') return node
  if (typeof node.$ref === 'string' && Object.keys(node).length > 1) {
    const extra = Object.keys(node).filter((k) => k !== '$ref' && !REF_ANNOTATIONS.has(k))
    if (extra.length > 0) {
      throw new Error(`${path}: $ref carries non-annotation keyword(s) ${extra.join(', ')}`)
    }
    return { $ref: node.$ref }
  }
  return Object.fromEntries(Object.entries(node).map(([k, v]) => [k, recurse(v, k)]))
}

function rootSchemaFor(bundle) {
  const names = Object.keys(bundle.$defs ?? {})
  if (names.length === 0) throw new Error(`${schemaPath} has no $defs to generate from`)
  return {
    $schema: bundle.$schema,
    title: ROOT_TITLE,
    type: 'object',
    properties: Object.fromEntries(names.map((n) => [n, { $ref: `#/$defs/${n}` }])),
    required: names,
    additionalProperties: false,
    $defs: bareRefs(bundle.$defs, '#/$defs'),
  }
}

// Two kinds of generator boilerplate that carry no information for a reader:
// its own "DO NOT MODIFY" preamble (replaced by BANNER, which names the real
// source and the regeneration command), and a provenance paragraph repeated on
// every one of the 51 types.
function stripBoilerplate(code) {
  const withoutBackref = code.replace(
    /^ \* (This interface was referenced by .*|via the `definition` .*)\n/gm,
    '',
  )
  // The paragraph above may have been a whole doc comment, or the tail of one;
  // either way it leaves a dangling `*` line or an empty `/** */` behind.
  return withoutBackref.replace(/^\/\*\*\n \*\n \*\/\n/gm, '').replace(/^ \*\n \*\//gm, ' */')
}

// The synthetic root is scaffolding, not contract: drop its interface (the
// braces are balanced and the body is one property per line, so the closing
// `}` at column 0 terminates it).
function stripRootInterface(code) {
  const start = code.indexOf(`export interface ${ROOT_TITLE} {`)
  if (start < 0) throw new Error(`generated output has no ${ROOT_TITLE} interface to strip`)
  const end = code.indexOf('\n}\n', start)
  if (end < 0) throw new Error(`generated ${ROOT_TITLE} interface is unterminated`)
  return code.slice(0, start) + code.slice(end + 3)
}

async function generate(bundle) {
  const raw = await compile(rootSchemaFor(bundle), ROOT_TITLE, {
    // Unset `additionalProperties` means "tolerated" in JSON Schema, which the
    // generator spells as an `[k: string]: unknown` index signature — and an
    // index signature turns every typo into a valid property access. The wire
    // contract is what the schema names; unknown fields are ignored, not typed.
    additionalProperties: false,
    bannerComment: '',
    declareExternallyReferenced: true,
    // Formatting is done below with the workspace prettier, so the output
    // matches `npm run format:check` rather than whichever prettier the
    // generator resolves.
    format: false,
  })
  const code = `${BANNER}\n\n${stripRootInterface(stripBoilerplate(raw)).trimStart()}`
  return prettier.format(code, { parser: 'typescript' })
}

const SAMPLES_BANNER = `/**
 * GENERATED — DO NOT EDIT.
 *
 * The example payloads in wire-samples.json (serialized from real Rust values
 * by \`chuggernaut schema api-samples\`), each restated as a TypeScript literal
 * that \`satisfies\` the generated type for its schema name.
 *
 * That \`satisfies\` is the round trip: \`tsc\` checks bytes serde actually wrote
 * against the types the UI compiles with, and — because each payload is a fresh
 * literal — a field serde emits that the type does not declare is an excess-
 * property error rather than a silent extra key. Importing the JSON directly
 * could not do this: TypeScript widens every string in a JSON module to
 * \`string\`, so no discriminated union in the contract would be checked at all.
 *
 * Regenerate with \`npm run codegen\`. The assertions that exercise these values
 * at runtime live in roundtrip.test.ts.
 */`

// The samples module: one `satisfies`-checked literal per covered type, keyed
// by the `$defs` name the Rust emitter used.
async function generateSamples(bundle) {
  const samples = JSON.parse(await readFile(samplesPath, 'utf8'))
  const names = Object.keys(samples).sort()
  for (const name of names) {
    if (!bundle.$defs?.[name]) {
      throw new Error(`wire-samples.json has a sample for \`${name}\`, which is not in $defs`)
    }
  }
  const imports = `import type {\n${names.map((n) => `  ${n},`).join('\n')}\n} from './types.gen'`
  const entries = names
    .map((n) => `  ${n}: ${JSON.stringify(samples[n], null, 2)} satisfies ${n},`)
    .join('\n')
  const code = `${SAMPLES_BANNER}\n\n${imports}\n\nexport const wireSamples = {\n${entries}\n}\n`
  return prettier.format(code, { parser: 'typescript' })
}

const bundle = JSON.parse(await readFile(schemaPath, 'utf8'))
const outputs = [
  [outPath, await generate(bundle)],
  [samplesOutPath, await generateSamples(bundle)],
]

if (process.argv.includes('--check')) {
  let stale = false
  for (const [path, wanted] of outputs) {
    const committed = await readFile(path, 'utf8').catch(() => null)
    if (committed === wanted) continue
    console.error(
      `!!! ${path} is stale — regenerate with \`npm run codegen\` and commit the result.`,
    )
    stale = true
  }
  if (stale) {
    console.error(
      '!!! (the committed schema or samples changed without the generated client following.)',
    )
    process.exit(1)
  }
  console.log(`codegen:check — ${outputs.length} generated file(s) current`)
} else {
  for (const [path, wanted] of outputs) {
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, wanted)
    console.log(`wrote ${path}`)
  }
}
