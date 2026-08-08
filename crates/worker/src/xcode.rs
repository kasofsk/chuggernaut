//! Node-side Xcode discovery and `xcode:<version>` resolution (design #322 W4).
//!
//! accepts: the directory a Mac keeps its Xcodes in, and the version a launch's
//! `runtime.env` names; emits: the `DEVELOPER_DIR` that launch is given, the
//! `xcode:` references the node advertises in `NodeCapabilities`, and a named
//! refusal for every version it cannot serve unambiguously; guarantees: the
//! toolchain is selected per task through the environment and never through
//! `xcode-select`, discovery reads the installed bundles rather than operator
//! config, and no launch ever falls through to whatever `xcode-select` points at
//! (design #322 §3).

use std::path::{Path, PathBuf};

/// The scheme a host-mode environment reference carries when the toolchain is
/// Xcode (design #322 §3); `nix:` is [`crate::nix::NIX_ENV_PREFIX`].
pub const XCODE_ENV_PREFIX: &str = "xcode:";

/// Where a Mac keeps its Xcodes. The bundle name is free-form past `Xcode`, so
/// discovery reads each bundle's version rather than trusting the name.
pub const INSTALL_ROOT: &str = "/Applications";

/// Task-side variable naming the Xcode a host task builds against. Per-process,
/// so two tasks on different Xcodes never fight the way `xcode-select -s` would.
pub const DEVELOPER_DIR_VAR: &str = "DEVELOPER_DIR";

/// Iteration cap on one discovery scan (docs/reference/style.md Tier 2 rule 3): an
/// `/Applications` holding more than this is read up to the bound rather than
/// scanned unboundedly at boot.
const ENTRIES_MAX: usize = 4096;

/// The bundle key discovery reads. `Contents/version.plist` is XML on every
/// Xcode, where `Contents/Info.plist` is a binary plist this cannot parse.
const VERSION_KEY: &str = "CFBundleShortVersionString";

/// The build the bundle reports, carried for the record rather than for
/// selection: two bundles can share a version and differ here.
const BUILD_KEY: &str = "ProductBuildVersion";

/// One installed Xcode as discovery found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XcodeInstall {
    /// `CFBundleShortVersionString`, matched against a reference **exactly**:
    /// `xcode:26` does not select `26.5`.
    pub version: String,
    /// `ProductBuildVersion`, or empty when the bundle names none. Never
    /// selected on — it is what a same-version collision is reported with.
    pub build: String,
    /// The `.app` bundle this was read out of, so a refusal names a path an
    /// operator can act on.
    pub bundle: PathBuf,
    /// `{bundle}/Contents/Developer`, the value exported as
    /// [`DEVELOPER_DIR_VAR`].
    pub developer_dir: PathBuf,
}

impl XcodeInstall {
    /// The directory whose `bin` the workspace bootstrap puts at the head of
    /// `PATH` ([`container::RUNTIME_ENV_PATH_VAR`]): `xcodebuild`, `xcrun` and
    /// `xcode-select` live in `Developer/usr/bin`.
    pub fn env_path(&self) -> PathBuf {
        self.developer_dir.join("usr")
    }
}

/// What this node discovered it can serve, and where it looked. A node property
/// assembled at boot from the machine itself — it never rides the wire and is
/// never operator-typed (design #322 §3).
#[derive(Debug, Clone, Default)]
pub struct Xcodes {
    root: PathBuf,
    installs: Vec<XcodeInstall>,
}

impl Xcodes {
    /// Every Xcode under [`INSTALL_ROOT`], as a host-capable node discovers them
    /// at boot.
    pub fn discover() -> Self {
        Self::discover_in(Path::new(INSTALL_ROOT))
    }

    /// The same scan against an arbitrary root, which is what makes discovery
    /// testable against a fixture tree instead of the machine's real
    /// `/Applications`. An unreadable root is an empty set, never a panic.
    pub fn discover_in(root: &Path) -> Self {
        let mut installs = Vec::new();
        let mut scanned = 0usize;
        if let Ok(dir) = std::fs::read_dir(root) {
            for entry in dir.take(ENTRIES_MAX) {
                scanned += 1;
                let Ok(entry) = entry else { continue };
                if let Some(install) = read_bundle(&entry.path()) {
                    installs.push(install);
                }
            }
        }
        if scanned == ENTRIES_MAX {
            tracing::warn!(
                root = %root.display(),
                entries_max = ENTRIES_MAX,
                "xcode discovery stopped at its scan bound; an Xcode past it is advertised nowhere \
                 and refused at launch"
            );
        }
        installs.sort_by(|a, b| (&a.version, &a.bundle).cmp(&(&b.version, &b.bundle)));
        debug_assert!(
            installs.iter().all(|i| i.developer_dir.starts_with(root)),
            "discovery reports only what it scanned"
        );
        Self {
            root: root.to_path_buf(),
            installs,
        }
    }

    /// The `runtime.env` references this node advertises: one per version it can
    /// resolve **unambiguously**, so the capability list never promises a version
    /// [`Self::resolve`] would refuse.
    pub fn envs(&self) -> Vec<String> {
        self.installs
            .iter()
            .filter(|install| self.matching(&install.version).len() == 1)
            .map(|install| format!("{XCODE_ENV_PREFIX}{}", install.version))
            .collect()
    }

    /// The Xcode a version reference selects, or the refusal an operator reads.
    /// A version this node does not install, and one two bundles both claim, are
    /// both hard refusals naming what **is** installed (design #322 §3).
    pub fn resolve(&self, node: &str, version: &str) -> Result<&XcodeInstall, String> {
        match self.matching(version).as_slice() {
            [] => Err(self.unknown(node, version)),
            [install] => {
                debug_assert_eq!(install.version, version, "resolution matches exactly");
                Ok(install)
            }
            [one, other, ..] => Err(self.ambiguous(node, one, other)),
        }
    }

    /// Every install claiming `version`, exactly. More than one is the collision
    /// [`Self::resolve`] refuses and [`Self::envs`] withholds.
    fn matching(&self, version: &str) -> Vec<&XcodeInstall> {
        self.installs
            .iter()
            .filter(|install| install.version == version)
            .collect()
    }

    /// The refusal for a version this node does not install, naming what it does
    /// — the whole point being that the launch never falls through to whatever
    /// `xcode-select` points at.
    fn unknown(&self, node: &str, version: &str) -> String {
        let installed = if self.installs.is_empty() {
            "no Xcode at all".to_string()
        } else {
            format!(
                "{:?}",
                self.installs
                    .iter()
                    .map(|i| format!("{XCODE_ENV_PREFIX}{}", i.version))
                    .collect::<Vec<_>>()
            )
        };
        format!(
            "launch declares runtime.env {XCODE_ENV_PREFIX}{version} and node {node} installs \
             {installed} (scanned {}) — refused rather than built against whatever xcode-select \
             points at (design #322 §3)",
            self.root.display()
        )
    }

    /// The refusal for a version two bundles both claim: they can differ in
    /// build, so picking one would be the silent wrong-toolchain build this
    /// scheme exists to prevent.
    fn ambiguous(&self, node: &str, one: &XcodeInstall, other: &XcodeInstall) -> String {
        format!(
            "node {node} has two bundles reporting Xcode {} — {} (build {:?}) and {} (build {:?}) \
             — so {XCODE_ENV_PREFIX}{} names no single toolchain; it is advertised nowhere and \
             refused here, and removing or renaming one bundle resolves it (design #322 §3)",
            one.version,
            one.bundle.display(),
            one.build,
            other.bundle.display(),
            other.build,
            one.version
        )
    }
}

/// One `/Applications` entry as an install, or `None` when it is not an Xcode
/// bundle this node can serve. Existence, identity and provenance are separate
/// questions (docs/reference/style.md Tier 2 rule 7): the name is not enough, so the
/// toolchain directory a task is actually pointed at must be there and the
/// bundle must report its own version.
fn read_bundle(bundle: &Path) -> Option<XcodeInstall> {
    let name = bundle.file_name()?.to_str()?;
    if !name.starts_with("Xcode") || !name.ends_with(".app") {
        return None;
    }
    let developer_dir = bundle.join("Contents").join("Developer");
    if !developer_dir.join("usr").join("bin").is_dir() {
        tracing::warn!(bundle = %bundle.display(), "skipping an Xcode bundle with no Contents/Developer/usr/bin");
        return None;
    }
    let plist = std::fs::read_to_string(bundle.join("Contents").join("version.plist")).ok()?;
    let version = plist_string(&plist, VERSION_KEY).filter(|v| is_version(v))?;
    debug_assert!(!version.is_empty(), "a discovered version names something");
    Some(XcodeInstall {
        version,
        build: plist_string(&plist, BUILD_KEY).unwrap_or_default(),
        bundle: bundle.to_path_buf(),
        developer_dir,
    })
}

/// One `<key>`'s `<string>` value out of an XML plist, trimmed. Hand-read rather
/// than parsed, because two keys out of one well-known file do not earn a
/// dependency (docs/reference/style.md Tier 3).
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let at = plist.find(&format!("<key>{key}</key>"))?;
    let rest = &plist[at..];
    let open = rest.find("<string>")? + "<string>".len();
    let close = rest[open..].find("</string>")?;
    Some(rest[open..open + close].trim().to_string())
}

/// Whether a reported version can name an environment: non-empty, bounded, and
/// nothing that would not survive being written into a `runtime.env` reference.
fn is_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// One Xcode bundle as a machine has it — a `Contents/Developer` tree and the
/// XML `version.plist` discovery reads. Every test of this scheme runs against a
/// fixture tree rather than the machine's own `/Applications`, so the suite says
/// the same thing on Linux as on a Mac.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test fixture that cannot be built fails the test"
)]
pub fn install_fixture(root: &Path, bundle: &str, version: &str, build: &str) -> PathBuf {
    let path = root.join(bundle);
    std::fs::create_dir_all(
        path.join("Contents")
            .join("Developer")
            .join("usr")
            .join("bin"),
    )
    .unwrap();
    std::fs::write(
        path.join("Contents").join("version.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t\
             <key>BuildVersion</key>\n\t<string>3</string>\n\t<key>{VERSION_KEY}</key>\n\t\
             <string>{version}</string>\n\t<key>{BUILD_KEY}</key>\n\t<string>{build}</string>\n\
             </dict>\n</plist>\n"
        ),
    )
    .unwrap();
    path
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chug-xcode-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Discovery is a scan of the installed bundles, not operator config
    /// (design #322 §3): each bundle's own `version.plist` names it, the
    /// `Developer` directory is what a task is pointed at, and everything in
    /// `/Applications` that is not an Xcode is ignored.
    #[test]
    fn discovery_reads_each_bundle_and_ignores_everything_else() {
        let root = temp_dir("discover");
        install_fixture(&root, "Xcode.app", "26.5", "17F42");
        install_fixture(&root, "Xcode-16.4.app", "16.4", "16F6");
        install_fixture(&root, "Safari.app", "26.0", "26A1");
        std::fs::create_dir_all(root.join("Xcode-broken.app").join("Contents")).unwrap();
        std::fs::write(root.join("Xcode-notabundle.app"), b"").unwrap();

        let xcodes = Xcodes::discover_in(&root);
        assert_eq!(xcodes.envs(), vec!["xcode:16.4", "xcode:26.5"]);

        let install = xcodes.resolve("air", "26.5").unwrap();
        assert_eq!(install.bundle, root.join("Xcode.app"));
        assert_eq!(
            install.developer_dir,
            root.join("Xcode.app").join("Contents").join("Developer")
        );
        assert_eq!(install.env_path(), install.developer_dir.join("usr"));
        assert_eq!(install.build, "17F42");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An unknown version is a hard refusal **naming what is installed** — the
    /// property that keeps a typo from becoming a build against the wrong
    /// toolchain — and a node with no Xcode at all says so.
    #[test]
    fn an_unknown_version_is_refused_naming_what_is_installed() {
        let root = temp_dir("unknown");
        install_fixture(&root, "Xcode.app", "26.5", "17F42");
        let xcodes = Xcodes::discover_in(&root);

        let err = xcodes.resolve("air", "16.4").unwrap_err();
        assert!(err.contains("xcode:16.4"), "{err}");
        assert!(
            err.contains("xcode:26.5"),
            "the refusal names what IS there: {err}"
        );
        assert!(err.contains(&root.display().to_string()), "{err}");
        assert!(
            xcodes.resolve("air", "26").is_err(),
            "a version prefix selects nothing — the match is exact"
        );

        let empty = Xcodes::discover_in(&root.join("nowhere"));
        assert!(empty.envs().is_empty(), "a node with none advertises none");
        let err = empty.resolve("air", "26.5").unwrap_err();
        assert!(err.contains("no Xcode at all"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Two bundles claiming one version can differ in build, so that version is
    /// advertised nowhere and refused here, naming both bundles — while every
    /// other version on the node stays servable.
    #[test]
    fn a_version_two_bundles_claim_is_neither_advertised_nor_resolved() {
        let root = temp_dir("collision");
        install_fixture(&root, "Xcode.app", "16.4", "16F6");
        install_fixture(&root, "Xcode-rc.app", "16.4", "16F5");
        install_fixture(&root, "Xcode-26.app", "26.5", "17F42");
        let xcodes = Xcodes::discover_in(&root);

        assert_eq!(
            xcodes.envs(),
            vec!["xcode:26.5"],
            "an ambiguous version is never promised"
        );
        let err = xcodes.resolve("air", "16.4").unwrap_err();
        assert!(
            err.contains("Xcode.app") && err.contains("Xcode-rc.app"),
            "{err}"
        );
        assert!(err.contains("16F6") && err.contains("16F5"), "{err}");
        assert!(
            xcodes.resolve("air", "26.5").is_ok(),
            "the rest still serves"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The plist reader takes the value that follows its key, and a bundle whose
    /// version it cannot read is not an install — an unnamed toolchain is exactly
    /// what this scheme refuses to serve.
    #[test]
    fn the_plist_reader_takes_the_value_after_its_key() {
        let plist = format!(
            "<dict><key>SourceVersion</key><string>0</string><key>{VERSION_KEY}</key>\
             <string> 26.5 </string></dict>"
        );
        assert_eq!(plist_string(&plist, VERSION_KEY).as_deref(), Some("26.5"));
        assert_eq!(plist_string(&plist, "SourceVersion").as_deref(), Some("0"));
        assert_eq!(plist_string(&plist, BUILD_KEY), None);
        assert_eq!(plist_string("<dict>", VERSION_KEY), None);

        assert!(is_version("26.5") && is_version("16.4-beta"));
        for bad in ["", " ", "26.5#a", "26/5", &"9".repeat(33)] {
            assert!(!is_version(bad), "must refuse {bad:?}");
        }

        let root = temp_dir("unreadable");
        let bundle = root.join("Xcode.app");
        std::fs::create_dir_all(bundle.join("Contents").join("Developer")).unwrap();
        assert_eq!(
            read_bundle(&bundle),
            None,
            "a bundle whose Developer/usr/bin is absent could not serve a launch's PATH"
        );
        std::fs::create_dir_all(
            bundle
                .join("Contents")
                .join("Developer")
                .join("usr")
                .join("bin"),
        )
        .unwrap();
        assert_eq!(read_bundle(&bundle), None, "no version.plist, no install");
        std::fs::write(bundle.join("Contents").join("version.plist"), b"<dict/>").unwrap();
        assert_eq!(read_bundle(&bundle), None, "no version key, no install");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
