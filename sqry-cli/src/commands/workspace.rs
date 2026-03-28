//! Workspace command implementations (`sqry workspace …`)

use crate::args::{Cli, WorkspaceCommand, WorkspaceDiscoveryMode};
use crate::output::{JsonFormatter, NameDisplayMode, OutputStreams, TextFormatter};
use anyhow::{Context, Result, anyhow, bail};
use sqry_core::workspace::{
    DiscoveryMode, WorkspaceIndex, WorkspaceRegistry, WorkspaceRepoId, WorkspaceRepository,
    discover_repositories,
};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const REGISTRY_FILE: &str = ".sqry-workspace";

/// Entry point for all workspace subcommands.
///
/// # Errors
/// Returns an error if any subcommand fails to execute.
pub fn run_workspace(cli: &Cli, action: &WorkspaceCommand) -> Result<()> {
    match action {
        WorkspaceCommand::Init {
            workspace,
            mode,
            name,
        } => init_workspace(cli, workspace, *mode, name.as_ref()),
        WorkspaceCommand::Scan {
            workspace,
            mode,
            prune_stale,
        } => scan_workspace(cli, workspace, *mode, *prune_stale),
        WorkspaceCommand::Add {
            workspace,
            repo,
            name,
        } => add_repository(cli, workspace, repo, name.as_ref()),
        WorkspaceCommand::Remove { workspace, repo_id } => {
            remove_repository(cli, workspace, repo_id)
        }
        WorkspaceCommand::Query {
            workspace,
            query,
            threads,
        } => query_workspace(cli, workspace, query, *threads),
        WorkspaceCommand::Stats { workspace } => stats_workspace(cli, workspace),
    }
}

fn init_workspace(
    cli: &Cli,
    workspace: &str,
    mode: WorkspaceDiscoveryMode,
    name: Option<&String>,
) -> Result<()> {
    let workspace_root = PathBuf::from(workspace);
    fs::create_dir_all(&workspace_root).with_context(|| {
        format!(
            "Failed to create workspace directory {}",
            workspace_root.display()
        )
    })?;

    let registry_path = registry_path(&workspace_root);
    if registry_path.exists() {
        bail!(
            "Workspace registry already exists at {}. Use `sqry workspace scan` or other commands instead.",
            registry_path.display()
        );
    }

    let mut registry = WorkspaceRegistry::new(
        name.cloned()
            .or_else(|| derive_workspace_name(&workspace_root)),
    );
    registry.metadata.default_discovery_mode = Some(mode_label(mode).to_string());
    registry
        .save(&registry_path)
        .with_context(|| format!("Failed to write registry at {}", registry_path.display()))?;

    let mut streams = OutputStreams::with_pager(cli.pager_config());
    streams.write_result(&format!(
        "Workspace initialised at {}",
        registry_path.display()
    ))?;
    if let Some(name) = &registry.metadata.workspace_name {
        streams.write_result(&format!("Name: {name}"))?;
    }
    streams.write_result(&format!("Default discovery mode: {}", mode_label(mode)))?;
    streams.finish_checked()
}

fn scan_workspace(
    cli: &Cli,
    workspace: &str,
    mode: WorkspaceDiscoveryMode,
    prune_stale: bool,
) -> Result<()> {
    let workspace_root = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = registry_path(&workspace_root);

    let mut registry = WorkspaceRegistry::load(&registry_path)
        .with_context(|| format!("Failed to load registry at {}", registry_path.display()))?;

    let discovery_mode = convert_mode(mode);
    let discovered = discover_repositories(&workspace_root, discovery_mode).with_context(|| {
        format!(
            "Failed to discover repositories under {}",
            workspace_root.display()
        )
    })?;

    let mut known_ids: HashSet<_> = registry
        .repositories
        .iter()
        .map(|repo| repo.id.clone())
        .collect();

    let mut added = 0usize;
    let mut updated = 0usize;

    for mut repo in discovered {
        // Refresh metadata (discovery already sets last_indexed_at but be defensive)
        if let Ok(meta) = fs::metadata(&repo.index_path)
            && let Ok(modified) = meta.modified()
        {
            repo.last_indexed_at = Some(modified);
        }

        if known_ids.contains(&repo.id) {
            updated += 1;
        } else {
            added += 1;
            known_ids.insert(repo.id.clone());
        }
        registry.upsert_repo(repo)?;
    }

    let mut pruned = 0usize;
    if prune_stale {
        let before = registry.repositories.len();
        registry
            .repositories
            .retain(|repo| repo.index_path.exists());
        pruned = before.saturating_sub(registry.repositories.len());
    }

    registry.metadata.default_discovery_mode = Some(mode_label(mode).to_string());
    registry
        .save(&registry_path)
        .with_context(|| format!("Failed to update registry at {}", registry_path.display()))?;

    let mut streams = OutputStreams::with_pager(cli.pager_config());
    streams.write_result(&format!(
        "Scan complete: {} added, {} updated{}",
        added,
        updated,
        if prune_stale {
            format!(", {pruned} pruned")
        } else {
            String::new()
        }
    ))?;
    streams.write_result(&format!("Registry saved to {}", registry_path.display()))?;
    streams.finish_checked()
}

fn add_repository(cli: &Cli, workspace: &str, repo: &str, name: Option<&String>) -> Result<()> {
    let workspace_root = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = registry_path(&workspace_root);
    let mut registry = WorkspaceRegistry::load(&registry_path)
        .with_context(|| format!("Failed to load registry at {}", registry_path.display()))?;

    let repo_root =
        canonicalize_existing(repo).with_context(|| format!("Repository path {repo} not found"))?;
    // Check for the unified graph index path (.sqry/graph)
    let index_path = repo_root.join(".sqry").join("graph");
    if !index_path.exists() {
        bail!(
            "Repository {} does not contain an index. Run `sqry index {}` first.",
            repo_root.display(),
            repo_root.display()
        );
    }

    let relative = repo_root.strip_prefix(&workspace_root).map_err(|_| {
        anyhow!(
            "Repository {} is not inside the workspace {}",
            repo_root.display(),
            workspace_root.display()
        )
    })?;
    let repo_id = WorkspaceRepoId::new(relative);
    let repo_name = name
        .cloned()
        .or_else(|| derive_workspace_name(&repo_root))
        .ok_or_else(|| {
            anyhow!(
                "Unable to determine repository name for {}",
                repo_root.display()
            )
        })?;

    let last_indexed_at = fs::metadata(&index_path)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let repo_entry = WorkspaceRepository::new(
        repo_id.clone(),
        repo_name.clone(),
        repo_root.clone(),
        index_path,
        last_indexed_at,
    );

    let existed = registry
        .repositories
        .iter()
        .any(|existing| existing.id == repo_id);
    registry.upsert_repo(repo_entry)?;
    registry
        .save(&registry_path)
        .with_context(|| format!("Failed to update registry at {}", registry_path.display()))?;

    let mut streams = OutputStreams::with_pager(cli.pager_config());
    if existed {
        streams.write_result(&format!(
            "Updated repository {} ({}) in {}",
            repo_name,
            repo_id.as_str(),
            registry_path.display()
        ))?;
    } else {
        streams.write_result(&format!(
            "Added repository {} ({}) to {}",
            repo_name,
            repo_id.as_str(),
            registry_path.display()
        ))?;
    }
    streams.finish_checked()
}

fn remove_repository(cli: &Cli, workspace: &str, repo_id: &str) -> Result<()> {
    let workspace_root = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = registry_path(&workspace_root);
    let mut registry = WorkspaceRegistry::load(&registry_path)
        .with_context(|| format!("Failed to load registry at {}", registry_path.display()))?;

    let removed = registry.remove_repo(&WorkspaceRepoId::new(repo_id));
    if !removed {
        bail!("Repository '{repo_id}' not found in workspace");
    }

    registry
        .save(&registry_path)
        .with_context(|| format!("Failed to update registry at {}", registry_path.display()))?;
    let mut streams = OutputStreams::with_pager(cli.pager_config());
    streams.write_result(&format!(
        "Removed repository '{}' from {}",
        repo_id,
        registry_path.display()
    ))?;
    streams.finish_checked()
}

fn query_workspace(
    cli: &Cli,
    workspace: &str,
    query_str: &str,
    threads: Option<usize>,
) -> Result<()> {
    if threads.is_some() {
        log::info!("Thread override not applicable for workspace queries (build-time only)");
    }

    let workspace_root = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = registry_path(&workspace_root);

    let mut index = WorkspaceIndex::open(&workspace_root, &registry_path)
        .with_context(|| format!("Failed to open workspace at {}", registry_path.display()))?;

    let mut results = index
        .query(query_str)
        .with_context(|| "Workspace query execution failed".to_string())?;

    let total_results = results.len();
    if let Some(limit) = cli.limit
        && results.len() > limit
    {
        results.truncate(limit);
    }

    if cli.count {
        println!("{}", results.len());
        return Ok(());
    }
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    if cli.json {
        JsonFormatter::format_workspace(&results, &mut streams)?;
    } else {
        let mode = if cli.qualified_names {
            NameDisplayMode::Qualified
        } else {
            NameDisplayMode::Simple
        };
        let theme = crate::output::resolve_theme(cli);
        let use_color = !cli.no_color
            && theme != crate::output::ThemeName::None
            && std::env::var("NO_COLOR").is_err();
        let formatter = TextFormatter::new(use_color, mode, theme);
        formatter.format_workspace(&results, &mut streams)?;
        if let Some(limit) = cli.limit
            && total_results > limit
        {
            streams.write_diagnostic(&format!(
                "Note: showing {} of {} results (--limit={})",
                results.len(),
                total_results,
                limit
            ))?;
        }
    }

    streams.finish_checked()
}

fn stats_workspace(cli: &Cli, workspace: &str) -> Result<()> {
    let workspace_root = canonicalize_existing(workspace)
        .with_context(|| format!("Workspace path {workspace} not found"))?;
    let registry_path = registry_path(&workspace_root);

    let index = WorkspaceIndex::open(&workspace_root, &registry_path)
        .with_context(|| format!("Failed to open workspace at {}", registry_path.display()))?;
    let detailed_stats = index.detailed_stats();
    let metadata = &index.registry().metadata;

    let mut streams = OutputStreams::with_pager(cli.pager_config());

    if cli.json {
        let json = serde_json::json!({
            "workspace": {
                "path": workspace_root,
                "name": metadata.workspace_name,
                "default_discovery_mode": metadata.default_discovery_mode,
            },
            "repositories": {
                "total": detailed_stats.total_repos,
                "indexed": detailed_stats.indexed_repos,
                "unindexed": detailed_stats.unindexed_repos,
            },
            "symbols": {
                "total": detailed_stats.total_symbols,
                "avg_per_repo": detailed_stats.avg_symbols_per_repo,
            },
            "freshness": {
                "fresh": detailed_stats.freshness.fresh,
                "recent": detailed_stats.freshness.recent,
                "stale": detailed_stats.freshness.stale,
                "very_stale": detailed_stats.freshness.very_stale,
                "never_indexed": detailed_stats.freshness.never_indexed,
            },
            "health": {
                "score": detailed_stats.health_score(),
                "status": detailed_stats.health_status(),
            }
        });
        streams.write_result(&serde_json::to_string_pretty(&json)?)?;
    } else {
        streams.write_result(&format!("Workspace: {}", workspace_root.display()))?;
        if let Some(name) = &metadata.workspace_name {
            streams.write_result(&format!("Name: {name}"))?;
        }
        if let Some(mode) = &metadata.default_discovery_mode {
            streams.write_result(&format!("Default discovery mode: {mode}"))?;
        }
        streams.write_result("")?;
        streams.write_result(&format!(
            "Repositories: {} total ({} indexed, {} unindexed)",
            detailed_stats.total_repos,
            detailed_stats.indexed_repos,
            detailed_stats.unindexed_repos
        ))?;
        streams.write_result(&format!(
            "Total symbols: {} ({:.1} avg per repo)",
            detailed_stats.total_symbols, detailed_stats.avg_symbols_per_repo
        ))?;
        streams.write_result("")?;
        streams.write_result("Freshness:")?;
        streams.write_result(&format!(
            "  Fresh (< 1 hour):     {}",
            detailed_stats.freshness.fresh
        ))?;
        streams.write_result(&format!(
            "  Recent (< 1 day):     {}",
            detailed_stats.freshness.recent
        ))?;
        streams.write_result(&format!(
            "  Stale (< 1 week):     {}",
            detailed_stats.freshness.stale
        ))?;
        streams.write_result(&format!(
            "  Very stale (> 1 week): {}",
            detailed_stats.freshness.very_stale
        ))?;
        streams.write_result(&format!(
            "  Never indexed:        {}",
            detailed_stats.freshness.never_indexed
        ))?;
        streams.write_result("")?;
        streams.write_result(&format!(
            "Health: {} ({:.1}%)",
            detailed_stats.health_status(),
            detailed_stats.health_score() * 100.0
        ))?;
    }

    streams.finish_checked()
}

fn registry_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(REGISTRY_FILE)
}

fn convert_mode(mode: WorkspaceDiscoveryMode) -> DiscoveryMode {
    match mode {
        WorkspaceDiscoveryMode::IndexFiles => DiscoveryMode::IndexFiles,
        WorkspaceDiscoveryMode::GitRoots => DiscoveryMode::GitRoots,
    }
}

fn mode_label(mode: WorkspaceDiscoveryMode) -> &'static str {
    match mode {
        WorkspaceDiscoveryMode::IndexFiles => "index-files",
        WorkspaceDiscoveryMode::GitRoots => "git-roots",
    }
}

fn derive_workspace_name(path: &Path) -> Option<String> {
    path.file_name()
        .or_else(|| {
            path.components()
                .next_back()
                .map(std::path::Component::as_os_str)
        })
        .map(|os| os.to_string_lossy().into_owned())
}

fn canonicalize_existing(path: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        candidate
            .canonicalize()
            .with_context(|| format!("Failed to resolve path {path}"))
    } else {
        Err(anyhow!("Path '{path}' does not exist"))
    }
}
