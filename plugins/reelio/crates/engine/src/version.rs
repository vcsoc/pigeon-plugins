//! Compiled identity of the engine (former `app_version`).

/// Package revision suffix. The reference Arch recipe published Reelio 0.2.0 as revision 6
/// (`pkgrel=6`); the plugin keeps that identity until its own release numbering is set.
pub const PACKAGE_REVISION: &str = "6";

/// `<cargo version>-<package revision>`, e.g. `0.2.0-6`.
pub fn app_version() -> String {
    format!("{}-{}", env!("CARGO_PKG_VERSION"), PACKAGE_REVISION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_package_revision_after_the_cargo_version() {
        let version = app_version();
        let (base, revision) = version.rsplit_once('-').unwrap();
        assert_eq!(base, env!("CARGO_PKG_VERSION"));
        assert!(!revision.is_empty() && revision.bytes().all(|byte| byte.is_ascii_digit()));
    }
}
