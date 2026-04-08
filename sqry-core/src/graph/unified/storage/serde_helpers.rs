//! Serde helpers for deterministic serialization of graph storage types.
//!
//! Rust's `HashMap` uses random hash seeds, so iteration order varies between
//! runs. This module provides serde adapters that serialize maps in sorted
//! order, ensuring bit-for-bit identical output for the same logical content.
//! This is critical for deterministic `postcard` snapshots.

/// Serde adapter for `HashMap<Arc<str>, u32>` that serializes entries sorted
/// by key (lexicographic order).
///
/// Without this adapter, the same interner contents could produce different
/// `postcard` bytes depending on `HashMap` iteration order, which varies
/// due to random hash seeds.
///
/// # Usage
///
/// ```rust,ignore
/// #[derive(Serialize, Deserialize)]
/// struct MyStruct {
///     #[serde(with = "sorted_arc_str_map")]
///     lookup: HashMap<Arc<str>, u32>,
/// }
/// ```
pub mod sorted_arc_str_map {
    use std::collections::HashMap;
    use std::sync::Arc;

    use serde::de::Deserializer;
    use serde::ser::Serializer;
    use serde::{Deserialize, Serialize};

    /// Serializes a `HashMap<Arc<str>, u32>` as a sorted sequence of `(str, u32)` pairs.
    ///
    /// Entries are sorted lexicographically by key (Rust `str::cmp`, which is
    /// Unicode scalar value / byte order, not locale-dependent) to ensure
    /// deterministic output.
    ///
    /// # Errors
    ///
    /// Returns the serializer error if the sorted entries cannot be encoded.
    pub fn serialize<S, H>(
        map: &HashMap<Arc<str>, u32, H>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        H: std::hash::BuildHasher,
    {
        let mut entries: Vec<(&str, u32)> = map.iter().map(|(k, &v)| (k.as_ref(), v)).collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        entries.serialize(serializer)
    }

    /// Deserializes a sorted sequence of `(String, u32)` pairs back into a
    /// `HashMap<Arc<str>, u32>`.
    ///
    /// # Errors
    ///
    /// Returns the deserializer error if the encoded entry sequence is invalid.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<Arc<str>, u32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries: Vec<(String, u32)> = Vec::deserialize(deserializer)?;
        let mut map = HashMap::with_capacity(entries.len());
        for (key, value) in entries {
            let arc_key: Arc<str> = Arc::from(key.as_str());
            map.insert(arc_key, value);
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestWrapper {
        #[serde(with = "super::sorted_arc_str_map")]
        map: HashMap<Arc<str>, u32>,
    }

    #[test]
    fn test_sorted_serialization_deterministic() {
        // Build two maps with the same entries but inserted in different order.
        // Due to HashMap's random hash seeds, their iteration order may differ.
        // The serde adapter should produce identical bytes regardless.
        let mut map1 = HashMap::new();
        map1.insert(Arc::from("alpha"), 1);
        map1.insert(Arc::from("beta"), 2);
        map1.insert(Arc::from("gamma"), 3);

        let mut map2 = HashMap::new();
        map2.insert(Arc::from("gamma"), 3);
        map2.insert(Arc::from("alpha"), 1);
        map2.insert(Arc::from("beta"), 2);

        let wrapper1 = TestWrapper { map: map1 };
        let wrapper2 = TestWrapper { map: map2 };

        let bytes1 = postcard::to_stdvec(&wrapper1).expect("serialize wrapper1");
        let bytes2 = postcard::to_stdvec(&wrapper2).expect("serialize wrapper2");

        assert_eq!(
            bytes1, bytes2,
            "sorted_arc_str_map must produce identical bytes regardless of insertion order"
        );
    }

    #[test]
    fn test_roundtrip() {
        let mut map = HashMap::new();
        map.insert(Arc::from("foo"), 10);
        map.insert(Arc::from("bar"), 20);
        map.insert(Arc::from("baz"), 30);

        let wrapper = TestWrapper { map };

        let bytes = postcard::to_stdvec(&wrapper).expect("serialize");
        let deserialized: TestWrapper = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(wrapper, deserialized);
    }

    #[test]
    fn test_empty_map() {
        let wrapper = TestWrapper {
            map: HashMap::new(),
        };

        let bytes = postcard::to_stdvec(&wrapper).expect("serialize");
        let deserialized: TestWrapper = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(wrapper, deserialized);
        assert!(deserialized.map.is_empty());
    }

    /// Verify that the sorted Vec<(key, value)> wire format is compatible with
    /// postcard's default `HashMap` serialization. In postcard, both maps and
    /// sequences of tuples use the same length-prefixed encoding, so our
    /// sorted adapter produces bytes that can round-trip through the default
    /// `HashMap` deserializer and vice versa.
    #[test]
    fn test_wire_format_compatible_with_default_hashmap_serde() {
        // Struct using our sorted adapter
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct WithAdapter {
            #[serde(with = "super::sorted_arc_str_map")]
            map: HashMap<Arc<str>, u32>,
        }

        // Struct using default HashMap serde (what old snapshots use)
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct WithoutAdapter {
            map: HashMap<Arc<str>, u32>,
        }

        let mut map = HashMap::new();
        map.insert(Arc::from("alpha"), 1);

        // Serialize with the OLD format (default HashMap)
        let old_format = WithoutAdapter { map: map.clone() };
        let old_bytes = postcard::to_stdvec(&old_format).expect("serialize old format");

        // Deserialize old bytes using NEW adapter
        let deserialized: WithAdapter =
            postcard::from_bytes(&old_bytes).expect("new adapter must read old format");
        let alpha_key: Arc<str> = Arc::from("alpha");
        assert_eq!(deserialized.map.get(&alpha_key), Some(&1));

        // Serialize with NEW adapter
        let new_format = WithAdapter { map };
        let new_bytes = postcard::to_stdvec(&new_format).expect("serialize new format");

        // Deserialize new bytes using OLD format
        let deserialized_old: WithoutAdapter =
            postcard::from_bytes(&new_bytes).expect("old format must read new adapter bytes");
        assert_eq!(deserialized_old.map.get(&alpha_key), Some(&1));
    }

    #[test]
    fn test_unicode_keys() {
        let mut map = HashMap::new();
        map.insert(Arc::from("日本語"), 1);
        map.insert(Arc::from("中文"), 2);
        map.insert(Arc::from("한국어"), 3);

        let wrapper = TestWrapper { map };

        let bytes = postcard::to_stdvec(&wrapper).expect("serialize");
        let deserialized: TestWrapper = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(wrapper, deserialized);
    }
}
