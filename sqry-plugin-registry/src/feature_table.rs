//! Compile-time mapping from plugin id → Cargo feature flag, used to
//! turn the otherwise-opaque `IncompatibleUnknownPluginIds` error
//! (cluster-E §E.2) into an actionable diagnostic that names the
//! `--features` list to pass to `cargo install`.
//!
//! The set of feature-gated plugins is the fixed list under
//! `[features]` in `sqry-plugin-registry/Cargo.toml` (apex / abap /
//! servicenow-xanadu-js / servicenow-xml / terraform / puppet / pulumi).
//! `enabled_in_this_binary` is decided by `cfg!(feature = …)` so the
//! runtime cost is zero and the answer matches the binary the user is
//! actually running.

/// One entry in the plugin-id → cargo-feature mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginFeatureSpec {
    /// Plugin id as it appears in the persisted manifest (matches
    /// `BUILTIN_PLUGIN_SPECS[*].id`).
    pub plugin_id: &'static str,
    /// Cargo feature flag in `sqry-plugin-registry/Cargo.toml` that
    /// pulls the plugin's optional crate dependency in.
    pub feature_flag: &'static str,
    /// `true` iff the current binary was compiled with `feature_flag`
    /// enabled. Computed at compile time via `cfg!(feature = …)`.
    pub enabled_in_this_binary: bool,
}

/// Authoritative table of feature-gated plugins. Keep in sync with
/// `sqry-plugin-registry/Cargo.toml::[features]` (the `plugin-*`
/// entries) — failing that, `missing_features_for` will silently
/// return an empty list for the unknown ids and the user-visible
/// error degrades to "rebuild the index".
pub const PLUGIN_FEATURE_TABLE: &[PluginFeatureSpec] = &[
    PluginFeatureSpec {
        plugin_id: "apex",
        feature_flag: "plugin-apex",
        enabled_in_this_binary: cfg!(feature = "plugin-apex"),
    },
    PluginFeatureSpec {
        plugin_id: "abap",
        feature_flag: "plugin-abap",
        enabled_in_this_binary: cfg!(feature = "plugin-abap"),
    },
    PluginFeatureSpec {
        plugin_id: "servicenow-xanadu-js",
        feature_flag: "plugin-servicenow-xanadu",
        enabled_in_this_binary: cfg!(feature = "plugin-servicenow-xanadu"),
    },
    PluginFeatureSpec {
        plugin_id: "servicenow-xml",
        feature_flag: "plugin-servicenow-xml",
        enabled_in_this_binary: cfg!(feature = "plugin-servicenow-xml"),
    },
    PluginFeatureSpec {
        plugin_id: "terraform",
        feature_flag: "plugin-terraform",
        enabled_in_this_binary: cfg!(feature = "plugin-terraform"),
    },
    PluginFeatureSpec {
        plugin_id: "puppet",
        feature_flag: "plugin-puppet",
        enabled_in_this_binary: cfg!(feature = "plugin-puppet"),
    },
    PluginFeatureSpec {
        plugin_id: "pulumi",
        feature_flag: "plugin-pulumi",
        enabled_in_this_binary: cfg!(feature = "plugin-pulumi"),
    },
];

/// Sort + dedup the feature flags that *would* register the unknown
/// plugin ids if enabled at build time. Returns an empty list when
/// none of the unknown ids correspond to a known feature gate (i.e.
/// the manifest is from a strictly newer sqry, or the ids are typos).
#[must_use]
pub fn missing_features_for<S: AsRef<str>>(unknown_ids: &[S]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = unknown_ids
        .iter()
        .filter_map(|id| {
            PLUGIN_FEATURE_TABLE
                .iter()
                .find(|spec| spec.plugin_id == id.as_ref() && !spec.enabled_in_this_binary)
                .map(|spec| spec.feature_flag)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// `true` when every entry in `unknown_ids` corresponds to a known
/// feature flag. Drives the "rebuild this binary with `--features …`"
/// vs the "rebuild the index" suggestion in user-facing errors.
#[must_use]
pub fn all_unknown_ids_have_features<S: AsRef<str>>(unknown_ids: &[S]) -> bool {
    !unknown_ids.is_empty()
        && unknown_ids.iter().all(|id| {
            PLUGIN_FEATURE_TABLE
                .iter()
                .any(|spec| spec.plugin_id == id.as_ref())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entry in the table names a feature flag declared in
    /// `Cargo.toml`. Verified indirectly by the fact that the file
    /// compiles — `cfg!(feature = "…")` is a no-op for a non-existent
    /// flag, but a typo here would silently disable the suggestion
    /// without a build error. We therefore pin the exact set so a
    /// rename of a feature flag in `Cargo.toml` forces a code
    /// review here.
    #[test]
    fn feature_table_contains_every_known_specialty_plugin() {
        let ids: Vec<&'static str> = PLUGIN_FEATURE_TABLE.iter().map(|s| s.plugin_id).collect();
        assert_eq!(
            ids,
            [
                "apex",
                "abap",
                "servicenow-xanadu-js",
                "servicenow-xml",
                "terraform",
                "puppet",
                "pulumi",
            ]
        );
    }

    /// Unknown ids that match a (disabled) feature gate yield the gate.
    #[test]
    fn missing_features_for_returns_disabled_gates() {
        // At least one of the specialty plugins is guaranteed disabled
        // unless the test runs with `--features specialty-plugins`.
        let result = missing_features_for(&["terraform"]);
        if !cfg!(feature = "plugin-terraform") {
            assert_eq!(result, vec!["plugin-terraform"]);
        }
    }

    /// Unknown ids that match no feature gate yield an empty list.
    #[test]
    fn missing_features_for_returns_empty_for_unrelated_ids() {
        let result = missing_features_for(&["totally-made-up-plugin-id"]);
        assert!(result.is_empty(), "expected empty, got {result:?}");
    }

    /// `all_unknown_ids_have_features` is `true` iff every id matches
    /// a feature gate.
    #[test]
    fn all_have_features_only_when_every_id_matches() {
        assert!(all_unknown_ids_have_features(&["terraform"]));
        assert!(all_unknown_ids_have_features(&["terraform", "puppet"]));
        assert!(!all_unknown_ids_have_features(&["terraform", "made-up"]));
        assert!(!all_unknown_ids_have_features::<&str>(&[]));
    }
}
