//! Compile-time build identity exposed consistently by logs, MCP, and doctor.

use serde::{Deserialize, Serialize};

/// Owned build metadata suitable for process boundaries and persisted reports.
///
/// Keeping this as a typed value prevents the HTTP adapter, control daemon, and
/// guardians from each inventing a subtly different version payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BuildIdentity {
    pub package_version: String,
    pub display: String,
    pub revision: String,
    pub dirty: bool,
}

impl BuildIdentity {
    /// Capture the identity compiled into the current executable.
    pub fn current() -> Self {
        Self {
            package_version: package_version().to_string(),
            display: display_version().to_string(),
            revision: revision().to_string(),
            dirty: dirty(),
        }
    }

    /// Whether two cooperating processes were built from the same source state.
    pub fn is_compatible_build(&self, other: &Self) -> bool {
        self.display == other.display
    }
}

/// Cargo package version.
pub const fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Source revision captured by `build.rs`, or `package` outside a Git checkout.
pub const fn revision() -> &'static str {
    env!("WINX_BUILD_REVISION")
}

/// Whether tracked source files differed from the captured revision at build time.
pub fn dirty() -> bool {
    env!("WINX_BUILD_DIRTY") == "true"
}

/// Unambiguous version string advertised over MCP and in startup logs.
pub const fn display_version() -> &'static str {
    env!("WINX_BUILD_IDENTITY")
}

#[cfg(test)]
mod tests {
    #[test]
    fn display_identity_contains_package_and_revision() {
        let identity = super::display_version();
        assert!(identity.starts_with(super::package_version()));
        assert!(identity.contains(super::revision()));
    }

    #[test]
    fn owned_identity_matches_compile_time_values() {
        let identity = super::BuildIdentity::current();
        assert_eq!(identity.package_version, super::package_version());
        assert_eq!(identity.display, super::display_version());
        assert_eq!(identity.revision, super::revision());
        assert_eq!(identity.dirty, super::dirty());
        assert!(identity.is_compatible_build(&identity));
    }
}
