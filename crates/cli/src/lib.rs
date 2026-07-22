//! Platform init and admin CLI (spec Part 12).
//!
//! Command definitions and runners; the `chuggernaut` bin mounts them as the
//! `init` and `admin` subcommands. Still TODO: ingest tokens (§13.2), user
//! role set/unset, secret key rotation, fixture seeding (testing.md).

pub mod admin;
pub mod init;
pub mod keygen;
pub mod schema;
pub mod sshcert;
pub mod sshfront;
pub mod validate;

pub use admin::AdminArgs;
pub use init::InitArgs;
pub use schema::SchemaArgs;
pub use sshcert::SshCertArgs;
pub use sshfront::SshShellArgs;
pub use validate::ValidateArgs;
