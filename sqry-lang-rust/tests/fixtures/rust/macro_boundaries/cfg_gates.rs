#[cfg(test)]
fn test_only() {}

#[cfg(feature = "serde")]
struct SerdeStruct {}

#[cfg(not(test))]
fn production_only() {}

#[cfg(all(unix, feature = "io"))]
fn unix_io() {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_or_macos() {}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct MaybeSerializable {
    data: String,
}

#[cfg_attr(test, cfg_attr(feature = "verbose", derive(Debug)))]
struct NestedCfgAttr {}

#[cfg(target_arch = "x86_64")]
fn x86_only() {}

#[cfg(debug_assertions)]
fn debug_only() {}
