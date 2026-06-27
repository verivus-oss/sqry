use criterion::{Criterion, criterion_group, criterion_main};
use sqry_lsp::LspOptions;
use sqry_lsp::handlers::{index, relations, search};
use sqry_lsp::protocol::{RelationKind, SqryRelationParams, SqrySearchParams};
use sqry_lsp::session::SessionManager;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_source() -> PathBuf {
    workspace_root().join("tests/fixtures/mcp/synthetic_100k_symbols")
}

fn baseline_workspace() -> &'static PathBuf {
    static DEST: OnceLock<PathBuf> = OnceLock::new();
    DEST.get_or_init(|| {
        let dest = workspace_root().join("target/lsp_baseline_workspace");
        if dest.exists() {
            fs::remove_dir_all(&dest).expect("clear previous baseline workspace");
        }
        copy_dir(&fixture_source(), &dest).expect("copy baseline fixture");
        let session = session_for(&dest);
        let reporter = sqry_core::progress::no_op_reporter();
        // Use force=true in benchmarks to ensure clean slate
        index::rebuild_index(&session, &dest, &reporter, true).expect("rebuild index");
        dest
    })
}

fn session_for(root: &Path) -> SessionManager {
    let options = LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(root.to_path_buf()),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    };
    SessionManager::new(options)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

fn bench_search(c: &mut Criterion) {
    let root = baseline_workspace().clone();
    let session = session_for(&root);
    let params = SqrySearchParams {
        query: "kind:function".into(),
        path: None,
        limit: Some(20),
        ..SqrySearchParams::default()
    };

    c.bench_function("sqry_search_baseline", |b| {
        b.iter(|| {
            let _ = search::execute(&session, &params).expect("search executes");
        });
    });
}

fn bench_relations(c: &mut Criterion) {
    let root = baseline_workspace().clone();
    let session = session_for(&root);
    let params = SqryRelationParams {
        relation: RelationKind::Callers,
        target: "process_data".into(),
        path: None,
        limit: Some(50),
    };

    if relations::execute(&session, params.clone()).is_ok() {
        c.bench_function("sqry_relations_baseline", |b| {
            b.iter(|| {
                let _ = relations::execute(&session, params.clone()).expect("relations executes");
            });
        });
    } else {
        eprintln!("skipping sqry_relations_baseline: relation data unavailable in baseline index");
    }
}

fn bench_index_status(c: &mut Criterion) {
    let root = baseline_workspace().clone();
    let session = session_for(&root);

    c.bench_function("sqry_index_status_baseline", |b| {
        b.iter(|| {
            let _ = index::index_status(&session, None).expect("index status executes");
        });
    });
}

fn handlers_baseline(c: &mut Criterion) {
    bench_search(c);
    bench_relations(c);
    bench_index_status(c);
}

criterion_group!(benches, handlers_baseline);
criterion_main!(benches);
