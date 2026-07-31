//! `chuggernaut schema` — emit the JSON Schema for the repo-authored YAML
//! files (spec §1.1), derived from the same serde types the platform parses
//! with, so the schema can never drift from the code.
//!
//! Intended use in a project repo:
//!
//! ```sh
//! chuggernaut schema job-type > .chug/jobs/.job-type.schema.json
//! chuggernaut schema defaults > .chug/jobs/.defaults.schema.json
//! ```
//!
//! then point yaml-language-server at it from each file:
//!
//! ```yaml
//! # yaml-language-server: $schema=.job-type.schema.json
//! ```
//!
//! Shape errors surface in the editor as you type; the semantic rules JSON
//! Schema cannot express (§1.1 field-rules matrices, prompt-file existence)
//! remain `chuggernaut validate` / release validation.
//!
//! The third kind describes the **§6.2 HTTP surface** instead of a file:
//!
//! ```sh
//! chuggernaut schema api > .chug/schemas/api.schema.json
//! ```
//!
//! It is the machine-readable half of the wire contract, and the source the
//! operator UI's TypeScript is generated from (NORTH-STAR §2, `web/src/api/`):
//! one bundle document whose `$defs` holds a named schema per type the API
//! serializes, generated from the same serde types that serve it. Emission
//! stays on stdout like the other kinds — the CLI also runs inside consumer
//! project repos, where a hardcoded output path would be meaningless.
//!
//! A fourth kind emits example *payloads* rather than a schema:
//!
//! ```sh
//! chuggernaut schema api-samples > web/src/api/wire-samples.json
//! ```
//!
//! One serialized value per covered response type, so the web round-trip test
//! parses bytes serde actually produced instead of bytes a TypeScript author
//! imagined (see `web/src/api/roundtrip.test.ts`). Both files are held current
//! by `committed_schemas_are_current`.

use clap::{Parser, ValueEnum};

#[derive(Parser)]
pub struct SchemaArgs {
    /// Which contract the schema describes.
    #[arg(value_enum)]
    pub kind: SchemaKind,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SchemaKind {
    /// .chug/jobs/{type}.yaml
    JobType,
    /// .chug/jobs/_defaults.yaml
    Defaults,
    /// The §6.2 HTTP surface (.chug/schemas/api.schema.json)
    Api,
    /// Example payloads for the §6.2 types (web/src/api/wire-samples.json)
    ApiSamples,
}

/// Each arm serializes its own document: `schemars::Schema` carries a custom
/// `Serialize` that emits keywords in reading order (`$schema`, `title`, …), so
/// routing everything through `serde_json::to_value` first would re-sort every
/// schema alphabetically and churn all three committed files.
pub fn generate(kind: SchemaKind) -> anyhow::Result<String> {
    let mut pretty = match kind {
        SchemaKind::JobType => {
            serde_json::to_string_pretty(&schemars::schema_for!(types::JobType))?
        }
        SchemaKind::Defaults => {
            serde_json::to_string_pretty(&schemars::schema_for!(types::ProjectDefaults))?
        }
        SchemaKind::Api => serde_json::to_string_pretty(&api_bundle()?)?,
        SchemaKind::ApiSamples => serde_json::to_string_pretty(&crate::wire_samples::bundle()?)?,
    };
    pretty.push('\n');
    Ok(pretty)
}

pub fn run(args: SchemaArgs) -> anyhow::Result<()> {
    print!("{}", generate(args.kind)?);
    Ok(())
}

/// Register each listed type as a named `$defs` entry of the api bundle.
///
/// Every covered type is named explicitly, including ones already reachable
/// from another root: the lists below *are* the statement of what the generated
/// contract covers, and re-registering a type is idempotent.
macro_rules! cover {
    ($generator:expr, $($t:ty),+ $(,)?) => {
        $( let _ = $generator.subschema_for::<$t>(); )+
    };
}

/// The §6.2 wire contract as one bundle document: `$defs` carries one named
/// schema per type the HTTP surface serializes, and the document has no root
/// type of its own — a response is one of the `$defs`, not the bundle.
///
/// Deliberately **not** covered, because no Rust type describes them: the reply
/// envelopes the dispatcher assembles with `serde_json::json!` (job criteria,
/// the job-type list/detail wrapper, tree/file, graph, project + platform
/// config, health, task output, artifact and attachment lists) and the
/// `serde_json::Value` request bodies the api forwards verbatim (job create/
/// update/members, project create/link). Naming those in Rust is its own
/// change; until then they stay hand-mirrored in TypeScript.
fn api_bundle() -> anyhow::Result<schemars::Schema> {
    let mut responses = schemars::generate::SchemaSettings::default()
        .for_serialize()
        .into_generator();
    api_bundle_job_records(&mut responses);
    api_bundle_platform_records(&mut responses);
    api_bundle_job_config(&mut responses);
    let mut requests = schemars::SchemaGenerator::default();
    api_bundle_request_bodies(&mut requests);

    let mut root = serde_json::Map::new();
    if let Some(meta_schema) = responses.settings().meta_schema.clone() {
        root.insert("$schema".into(), meta_schema.into_owned().into());
    }
    root.insert("title".into(), "chuggernaut HTTP surface".into());
    root.insert(
        "description".into(),
        "Generated by `chuggernaut schema api` (spec §6.2). Do not edit; \
         `committed_schemas_are_current` fails when this file goes stale."
            .into(),
    );
    let defs = api_bundle_defs(&mut responses, &mut requests)?;
    root.insert("$defs".into(), defs.into_iter().collect());
    Ok(root.into())
}

/// Both generators' definitions as one `$defs` map.
///
/// Sorted, so the emitted order is a property of this code rather than of
/// schemars' traversal or a downstream `preserve_order` feature — a drift test
/// that flapped would poison every unrelated job's CI.
///
/// A type reachable from both sides is only safe to emit once when the two
/// contracts agree about it (they do for the plain enums that qualify today).
/// One that disagreed would need two names on the wire, and silently keeping
/// either version would be a schema that lies to half its consumers — so it
/// fails the emission instead.
fn api_bundle_defs(
    responses: &mut schemars::SchemaGenerator,
    requests: &mut schemars::SchemaGenerator,
) -> anyhow::Result<std::collections::BTreeMap<String, serde_json::Value>> {
    let mut defs: std::collections::BTreeMap<String, serde_json::Value> =
        responses.take_definitions(true).into_iter().collect();
    for (name, schema) in requests.take_definitions(true) {
        if let Some(from_response) = defs.get(&name) {
            anyhow::ensure!(
                from_response == &schema,
                "`{name}` is reachable from both a response type and a request body, and the \
                 serialize/deserialize contracts disagree about it — give the request side its \
                 own type rather than picking one shape for both"
            );
            continue;
        }
        defs.insert(name, schema);
    }
    Ok(defs)
}

/// The job and task records the jobs/tasks/queue endpoints serve, plus the
/// channel post the jobs *list* projection carries.
fn api_bundle_job_records(generator: &mut schemars::SchemaGenerator) {
    cover!(
        generator,
        types::Job,
        types::JobSummary<'static>,
        types::JobState,
        types::Escalation,
        types::Task,
        types::TaskPhase,
        types::TaskKind,
        types::TaskState,
        types::Performer,
        types::PendingReason,
        types::ReworkReason,
        types::TaskResult,
        types::TokenUsage,
        types::EscalationAction,
        types::QueueSnapshot,
        types::QueueEntry,
        types::GroupEntry,
        types::DesignEntry,
        types::GroupRollup,
        types::GroupJob,
        types::ChannelUpdate,
        types::ChannelOrigin,
    );
}

/// Platform- and project-level records: the fleet and dispatcher snapshots, the
/// deploy report a command work task harvests, the origin link/release state,
/// and the caller's identity.
fn api_bundle_platform_records(generator: &mut schemars::SchemaGenerator) {
    cover!(
        generator,
        types::FleetStatus,
        types::FleetNode,
        types::SlotOccupant,
        types::DispatcherConfigSnapshot,
        types::WorkerNode,
        types::worker::RefreshOutcome,
        types::worker::RefreshResult,
        types::DeployReport,
        types::DeployLeg,
        types::LegStatus,
        types::OriginLink,
        types::ReleaseState,
        types::ReleaseStatus,
        types::Identity,
        types::IdentityKind,
        types::ProjectRole,
    );
}

/// The job-type definition the job-type detail endpoint serves parsed (the same
/// type `schema job-type` describes, here as one `$defs` entry among many).
fn api_bundle_job_config(generator: &mut schemars::SchemaGenerator) {
    cover!(
        generator,
        types::JobType,
        types::WorkSpec,
        types::WorkType,
        types::ReviewSpec,
        types::WrapUpSpec,
        types::WrapUpMode,
        types::Evaluator,
        types::EvaluatorType,
        types::Placement,
        types::Input,
        types::InputKind,
        types::job_type::Provider,
        types::job_type::Resources,
    );
}

/// Request bodies: the operator's task resolution (typed in `types`) and the
/// bodies typed in `api` itself.
fn api_bundle_request_bodies(generator: &mut schemars::SchemaGenerator) {
    cover!(
        generator,
        types::TaskResolution,
        api::routes::LoginBody,
        api::routes::SshCertBody,
        api::routes::MemberRoleBody,
        api::routes::FileQuery,
        api::routes::OutputQuery,
        api::routes::DiffQuery,
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The committed schemas (and the sample payloads the web round-trip test
    /// parses) must match what the current types generate. Regenerate with:
    ///   cargo run -p chuggernaut -- schema job-type > .chug/schemas/job-type.schema.json
    ///   cargo run -p chuggernaut -- schema defaults > .chug/schemas/defaults.schema.json
    ///   cargo run -p chuggernaut -- schema api > .chug/schemas/api.schema.json
    ///   cargo run -p chuggernaut -- schema api-samples > web/src/api/wire-samples.json
    ///
    /// For `api` this is the whole enforcement of the §6.2 contract: a field
    /// added to any covered type changes the generated bundle, so a change that
    /// does not re-emit it fails here rather than silently desynchronizing the
    /// generated TypeScript client from the wire. (The other half of that
    /// chain — TypeScript regenerated from the re-emitted schema — is
    /// `npm run codegen:check`, which `.chug/tasks/ci.sh` runs for any diff touching
    /// `.chug/schemas/` or `web/`.)
    #[test]
    fn committed_schemas_are_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (kind, file) in [
            (SchemaKind::JobType, ".chug/schemas/job-type.schema.json"),
            (SchemaKind::Defaults, ".chug/schemas/defaults.schema.json"),
            (SchemaKind::Api, ".chug/schemas/api.schema.json"),
            (SchemaKind::ApiSamples, "web/src/api/wire-samples.json"),
        ] {
            let committed = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("read {file}: {e} — regenerate (see test doc)"));
            assert_eq!(
                committed,
                generate(kind).unwrap(),
                "{file} is stale — regenerate (see test doc)"
            );
        }
    }

    /// Every sample names a type the bundle covers, and every sample is a JSON
    /// object: the web generator turns each into `<name> satisfies <Type>`, so
    /// a sample keyed on a name with no `$defs` entry would emit TypeScript
    /// that imports a type which does not exist.
    #[test]
    fn api_samples_name_covered_types() {
        let bundle: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::Api).unwrap()).unwrap();
        let defs = bundle["$defs"].as_object().unwrap();
        let samples: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::ApiSamples).unwrap()).unwrap();
        let samples = samples.as_object().unwrap();
        assert!(!samples.is_empty(), "no sample payloads emitted");
        for (name, payload) in samples {
            assert!(defs.contains_key(name), "sample `{name}` is not in $defs");
            assert!(payload.is_object(), "sample `{name}` is not a JSON object");
        }
        for name in ["Job", "JobSummary", "Task", "TaskResult", "FleetStatus"] {
            assert!(samples.contains_key(name), "no sample for {name}");
        }
    }

    /// A serialized [`types::Job`] carries every field the bundle marks
    /// required, and nothing the bundle does not declare.
    ///
    /// This is the Rust-side half of the round trip (the TypeScript half is
    /// `web/src/api/roundtrip.test.ts`): the api bundle describes responses
    /// under schemars' **serialize** contract, and the whole point of that
    /// choice is that `required` means "serde always writes this key". A
    /// contract that drifted from serde would hand the UI a non-null field
    /// that arrives `undefined`.
    #[test]
    fn job_sample_matches_the_schema_field_set() {
        let bundle: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::Api).unwrap()).unwrap();
        let samples: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::ApiSamples).unwrap()).unwrap();
        for name in ["Job", "JobSummary", "Task"] {
            let schema = &bundle["$defs"][name];
            let properties = schema["properties"].as_object().unwrap();
            let payload = samples[name].as_object().unwrap();
            for required in schema["required"].as_array().unwrap() {
                let key = required.as_str().unwrap();
                assert!(payload.contains_key(key), "{name}: serde omitted `{key}`");
            }
            for key in payload.keys() {
                assert!(
                    properties.contains_key(key),
                    "{name}: serde wrote `{key}`, which the schema does not declare"
                );
            }
        }
    }

    /// Emission is deterministic: two runs of the same code produce identical
    /// bytes. The drift test above gates every job's CI, so a bundle whose
    /// `$defs` order varied between runs would fail unrelated work at random.
    #[test]
    fn api_schema_is_deterministic() {
        assert_eq!(
            generate(SchemaKind::Api).unwrap(),
            generate(SchemaKind::Api).unwrap()
        );
    }

    /// Spot-check the api bundle carries the surface it claims: a record type
    /// from each covered family, the list projection that differs from the
    /// stored record, and a request body typed in `api` rather than `types`.
    /// Timestamps must schematize as `date-time` strings, not as an opaque
    /// object — the generated client's `created_at` is a string.
    #[test]
    fn api_schema_covers_the_wire_types() {
        let v: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::Api).unwrap()).unwrap();
        let defs = v["$defs"].as_object().unwrap();
        for name in [
            "Job",
            "JobSummary",
            "Task",
            "TaskResult",
            "TaskResolution",
            "QueueSnapshot",
            "FleetStatus",
            "DeployReport",
            "Identity",
            "JobType",
            "LoginBody",
        ] {
            assert!(defs.contains_key(name), "$defs is missing {name}");
        }
        assert!(defs["Job"]["properties"].get("description").is_some());
        assert!(
            defs["JobSummary"]["properties"]
                .get("description")
                .is_none()
        );
        assert_eq!(
            defs["Job"]["properties"]["created_at"]["format"],
            serde_json::json!("date-time")
        );
    }

    /// Spot-check the schema actually encodes the contract: the top level is
    /// tolerant of unknown fields (schema-evolution laxity, spec §14) while the
    /// gate-relevant `Evaluator` block stays strict; finalize enumerates
    /// merge|none.
    #[test]
    fn job_type_schema_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&generate(SchemaKind::JobType).unwrap()).unwrap();
        assert_ne!(v["additionalProperties"], serde_json::json!(false));
        assert_eq!(
            v["$defs"]["Evaluator"]["additionalProperties"],
            serde_json::json!(false)
        );
        assert_eq!(
            v["$defs"]["Input"]["additionalProperties"],
            serde_json::json!(false)
        );
        let finalize = &v["$defs"]["WrapUpMode"];
        let variants: Vec<&str> = finalize["oneOf"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["const"].as_str())
            .collect();
        assert_eq!(variants, ["merge", "none"], "{finalize}");
    }
}
