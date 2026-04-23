//! Integration tests for `ShardedCache` and `CachedResult` validation.

use smallvec::SmallVec;
use sqry_core::graph::unified::file::id::FileId;
use sqry_db::cache::{CachedResult, ShardedCache};
use sqry_db::dependency::FileDep;
use sqry_db::input::{FileInput, FileInputStore};
use sqry_db::query::QueryKey;

#[test]
fn cache_insert_and_retrieve_across_shards() {
    let cache = ShardedCache::new(64);

    // Insert into different shards
    for shard in 0..64 {
        let key = QueryKey::from_raw(shard as u64, 0);
        let result = CachedResult::new(format!("shard_{shard}"), SmallVec::new(), None, None);
        cache.insert(shard, key, result);
    }

    assert_eq!(cache.total_entries(), 64);

    // Retrieve from each shard
    for shard in 0..64 {
        let key = QueryKey::from_raw(shard as u64, 0);
        let val: Option<String> = cache.get_if_valid(shard, &key, |_| true);
        assert_eq!(val, Some(format!("shard_{shard}")));
    }
}

#[test]
fn cache_file_dep_invalidation_flow() {
    let cache = ShardedCache::new(4);
    let mut store = FileInputStore::new();
    store.insert(FileId::new(1), FileInput::new(Default::default()));
    store.insert(FileId::new(2), FileInput::new(Default::default()));

    // Insert with deps pointing to file 1 and 2 at revision 1
    let mut deps: SmallVec<[FileDep; 8]> = SmallVec::new();
    deps.push((FileId::new(1), 1));
    deps.push((FileId::new(2), 1));

    let key = QueryKey::from_raw(100, 0);
    let result = CachedResult::new(42u32, deps, None, None);
    cache.insert(0, key.clone(), result);

    // Should be valid
    let val: Option<u32> = cache.get_if_valid(0, &key, |c| c.validate_file_deps(&store));
    assert_eq!(val, Some(42));

    // Bump file 1 revision
    store
        .get_mut(FileId::new(1))
        .unwrap()
        .update(Default::default());

    // Should now be invalid
    let val: Option<u32> = cache.get_if_valid(0, &key, |c| c.validate_file_deps(&store));
    assert!(
        val.is_none(),
        "cache should be invalidated after file revision bump"
    );
}

#[test]
fn cache_edge_revision_invalidation() {
    let cache = ShardedCache::new(4);

    let key = QueryKey::from_raw(200, 0);
    let result = CachedResult::new(vec![1u32, 2, 3], SmallVec::new(), Some(5), None);
    cache.insert(0, key.clone(), result);

    // Valid when edge revision matches
    let current_edge_rev = 5u64;
    let val: Option<Vec<u32>> =
        cache.get_if_valid(0, &key, |c| c.edge_revision() == Some(current_edge_rev));
    assert_eq!(val, Some(vec![1, 2, 3]));

    // Invalid when edge revision advances
    let current_edge_rev = 6u64;
    let val: Option<Vec<u32>> =
        cache.get_if_valid(0, &key, |c| c.edge_revision() == Some(current_edge_rev));
    assert!(val.is_none());
}

#[test]
fn cache_metadata_revision_invalidation() {
    let cache = ShardedCache::new(4);

    let key = QueryKey::from_raw(300, 0);
    let result = CachedResult::new("unused_nodes".to_string(), SmallVec::new(), None, Some(3));
    cache.insert(0, key.clone(), result);

    // Valid when metadata revision matches
    let val: Option<String> = cache.get_if_valid(0, &key, |c| c.metadata_revision() == Some(3));
    assert_eq!(val, Some("unused_nodes".to_string()));

    // Invalid when metadata revision advances
    let val: Option<String> = cache.get_if_valid(0, &key, |c| c.metadata_revision() == Some(4));
    assert!(val.is_none());
}

#[test]
fn cache_concurrent_read_write() {
    use std::sync::Arc;
    use std::thread;

    let cache = Arc::new(ShardedCache::new(64));

    // Spawn writers
    let mut handles = vec![];
    for i in 0u64..8 {
        let cache = Arc::clone(&cache);
        handles.push(thread::spawn(move || {
            for j in 0u64..100 {
                let shard = ((i * 100 + j) % 64) as usize;
                let key = QueryKey::from_raw(i * 1000 + j, 0);
                let result = CachedResult::new((i, j), SmallVec::new(), None, None);
                cache.insert(shard, key, result);
            }
        }));
    }

    // Spawn readers
    for _ in 0..4 {
        let cache = Arc::clone(&cache);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let _ = cache.total_entries();
                let _ = cache.shard_entry_counts();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // All writes should have succeeded
    assert!(cache.total_entries() > 0);
}
