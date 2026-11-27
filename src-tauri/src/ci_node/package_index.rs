//! Where a curated Python registry entry installs from — and, in test builds
//! only, the overlay that lets a test register an entry of its own.
//!
//! Temporary copy while qontinui-runner#1995 is open: #1995 deletes this file
//! with `tools.rs`.
//!
//! The index used to be a literal `--index-url https://pypi.org/simple` inside
//! `tools::install_python_package`. It is a per-entry field so the end-to-end
//! test `child_env::tests::provisioning_children_see_only_allowlisted_names`
//! can provision a probe distribution from a local wheel directory
//! (`--no-index --find-links`, no network) through the PRODUCTION
//! `tools::provision`, the function the executor calls. A shipped runner has
//! exactly one value, [`PackageIndex::PyPi`] (pinned by a literal test below):
//! the local variant and the overlay exist only under `#[cfg(test)]`, so the
//! registry a runner serves stays closed.

#[cfg(test)]
use super::tools::ToolSpec;

/// Where pip resolves a registry entry's distribution from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageIndex {
    /// The public Python Package Index. Every curated entry.
    PyPi,
    /// A directory of wheels and nothing else. Test builds only.
    #[cfg(test)]
    LocalWheels(&'static std::path::Path),
}

impl PackageIndex {
    /// The pip arguments that select this index.
    ///
    /// Explicit on every install, so where a distribution comes from is a
    /// property of the registry entry rather than of whatever pip
    /// configuration the host carries — belt and braces with
    /// `child_env::PIP_CONFIG_FILE`, which makes pip load no config at all.
    pub(crate) fn pip_args(&self) -> Vec<String> {
        match self {
            Self::PyPi => vec![
                "--index-url".to_string(),
                "https://pypi.org/simple".to_string(),
            ],
            #[cfg(test)]
            Self::LocalWheels(dir) => vec![
                "--no-index".to_string(),
                "--find-links".to_string(),
                dir.to_string_lossy().into_owned(),
            ],
        }
    }
}

/// Entries tests registered on top of the closed registry. `tools::lookup`
/// consults this (an entry here can never share a curated name);
/// `tools::known_tool_names` never lists one.
#[cfg(test)]
static TEST_TOOLS: std::sync::Mutex<Vec<&'static ToolSpec>> = std::sync::Mutex::new(Vec::new());

/// Register a tool this test process can provision through `tools::provision`.
/// Refuses a curated name — an overlay entry may never shadow a reviewed one —
/// and a second registration of the same name.
#[cfg(test)]
pub(crate) fn register_test_tool(spec: ToolSpec) -> &'static ToolSpec {
    assert!(
        !super::tools::known_tool_names().contains(&spec.name),
        "test tool {:?} would shadow a curated registry entry",
        spec.name
    );
    let mut tools = TEST_TOOLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        tools.iter().all(|t| t.name != spec.name),
        "test tool {:?} is already registered",
        spec.name
    );
    let spec: &'static ToolSpec = Box::leak(Box::new(spec));
    tools.push(spec);
    spec
}

/// The registered test tool called `name`, if any.
#[cfg(test)]
pub(crate) fn test_tool(name: &str) -> Option<&'static ToolSpec> {
    TEST_TOOLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|t| t.name == name)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_node::tools::{self, Installer};
    use std::path::Path;

    /// The only production value, spelled out: every curated Python entry
    /// installs from PyPI's simple index.
    #[test]
    fn every_curated_python_entry_installs_from_pypi() {
        let mut checked = Vec::new();
        for name in tools::known_tool_names() {
            let spec = tools::lookup(name).expect("a listed name resolves");
            if let Installer::PythonPackage { index, .. } = &spec.install {
                assert_eq!(*index, PackageIndex::PyPi, "{name}");
                assert_eq!(
                    index.pip_args(),
                    vec!["--index-url", "https://pypi.org/simple"],
                    "{name}"
                );
                checked.push(name);
            }
        }
        // Not vacuous: all three Python entries were looked at.
        assert_eq!(checked, vec!["poetry", "twine", "datamodel-code-generator"]);
    }

    #[test]
    fn a_local_wheel_directory_installs_with_no_index_at_all() {
        assert_eq!(
            PackageIndex::LocalWheels(Path::new("/wheels")).pip_args(),
            vec!["--no-index", "--find-links", "/wheels"]
        );
    }

    /// An overlay entry is found by `lookup`, which is how `provision` reaches
    /// it, and is never listed as part of the registry.
    #[test]
    fn an_overlay_entry_is_provisionable_but_never_listed() {
        const NAME: &str = "qontinui-overlay-unit-test";
        let spec = register_test_tool(ToolSpec {
            name: NAME,
            verify_args: &["--version"],
            install: Installer::PythonPackage {
                distribution: NAME,
                module: "qontinui_overlay_unit_test",
                command: NAME,
                index: PackageIndex::LocalWheels(Path::new("/wheels")),
            },
        });
        let found = tools::lookup(NAME).expect("lookup finds a registered test tool");
        assert!(std::ptr::eq(found, spec));
        assert!(!tools::known_tool_names().contains(&NAME));
    }

    #[test]
    #[should_panic(expected = "would shadow a curated registry entry")]
    fn an_overlay_entry_cannot_shadow_a_curated_one() {
        register_test_tool(ToolSpec {
            name: "node",
            verify_args: &["--version"],
            install: Installer::PythonPackage {
                distribution: "node",
                module: "node",
                command: "node",
                index: PackageIndex::PyPi,
            },
        });
    }
}
