//! `chuggernaut schema` — emit the JSON Schema for the repo-authored YAML
//! files (spec §1.1), derived from the same serde types the platform parses
//! with, so the schema can never drift from the code.
//!
//! Intended use in a project repo:
//!
//! ```sh
//! chuggernaut schema job-type > jobs/.job-type.schema.json
//! chuggernaut schema defaults > jobs/.defaults.schema.json
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

use clap::{Parser, ValueEnum};

#[derive(Parser)]
pub struct SchemaArgs {
    /// Which repo-authored file the schema describes.
    #[arg(value_enum)]
    pub kind: SchemaKind,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SchemaKind {
    /// jobs/{type}.yaml
    JobType,
    /// jobs/_defaults.yaml
    Defaults,
}

pub fn generate(kind: SchemaKind) -> String {
    let schema = match kind {
        SchemaKind::JobType => schemars::schema_for!(types::JobType),
        SchemaKind::Defaults => schemars::schema_for!(types::ProjectDefaults),
    };
    let mut pretty = serde_json::to_string_pretty(&schema).expect("schema serializes");
    pretty.push('\n');
    pretty
}

pub fn run(args: SchemaArgs) -> anyhow::Result<()> {
    print!("{}", generate(args.kind));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed schemas (v2/schemas/) must match what the current types
    /// generate. Regenerate with:
    ///   cargo run -p chuggernaut -- schema job-type > schemas/job-type.schema.json
    ///   cargo run -p chuggernaut -- schema defaults > schemas/defaults.schema.json
    #[test]
    fn committed_schemas_are_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
        for (kind, file) in [
            (SchemaKind::JobType, "job-type.schema.json"),
            (SchemaKind::Defaults, "defaults.schema.json"),
        ] {
            let committed = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("read schemas/{file}: {e} — regenerate (see test doc)"));
            assert_eq!(
                committed,
                generate(kind),
                "schemas/{file} is stale — regenerate (see test doc)"
            );
        }
    }

    /// Spot-check the schema actually encodes the contract: unknown fields
    /// rejected, finalize enumerates merge|none, doc comments surface.
    #[test]
    fn job_type_schema_shape() {
        let v: serde_json::Value = serde_json::from_str(&generate(SchemaKind::JobType)).unwrap();
        assert_eq!(v["additionalProperties"], serde_json::json!(false));
        let finalize = &v["$defs"]["Finalize"];
        let variants: Vec<&str> = finalize["oneOf"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["const"].as_str())
            .collect();
        assert_eq!(variants, ["merge", "none"], "{finalize}");
    }
}
