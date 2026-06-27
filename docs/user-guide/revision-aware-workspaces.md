# Revision-Aware Workspaces

Revision-aware workspaces let a running daemon keep more than one view of the
same repository addressable at once: the live checkout, immutable Git
revisions, point-in-time dirty snapshots, and daemon-managed worktrees.

The compatibility rule is strict: existing `sqry daemon load <path>` and
queries that omit a revision selector use the live workspace only. Loading a
revision never turns later unqualified queries into multi-revision or union
queries.

## Terms

| Term | Meaning |
|------|---------|
| Live workspace | The checked-out source root loaded by `sqry daemon load`. It keeps the existing watcher and incremental rebuild behavior. |
| Revision selector | User input that names `live`, `dirty`, a ref, a commit, a tree, or a managed worktree. |
| Resolved revision | The pinned result of resolving a selector at load time. A branch or tag is not followed lazily during query. |
| Immutable revision | A graph built from stable Git object content, normally through raw tree/blob traversal. |
| Dirty snapshot | A point-in-time capture of selected working-tree changes. It is not a mutable live workspace. |
| Managed worktree | A daemon-owned Git worktree used for checkout-byte fallback or an explicit agent worktree. |
| Artifact id | A deterministic cache key for persisted revision graph artifacts. It is not the same thing as an in-memory resident handle. |

## Live Default

Use the existing flow when you want the current checkout:

```bash
sqry daemon start
sqry daemon load .
sqry query "kind:function AND name:authenticate"
```

Even after loading revisions, a query without `--revision-id` or an explicit
selector continues to target only the live workspace for the selected source
root.

## Immutable Revisions

Immutable revision loading resolves refs immediately and records provenance:

```bash
sqry daemon load-revision . --ref main
sqry daemon load-revision . --commit 0123456789abcdef
sqry daemon load-revision . --tree 89abcdef01234567 --source-byte-mode raw-git-objects
sqry daemon list-revisions --root .
```

The preferred immutable source mode is `raw-git-objects`. sqry traverses Git
tree and blob objects directly and parses the stored blob bytes. It does not
run checkout filters, smudge filters, or implicit network fetches.

If an object required by the selected revision is missing, sqry returns an
actionable missing-object error. It does not fetch from remotes automatically.
Fetch explicitly with Git if that is appropriate for your workflow, then retry
the load.

## Checkout Bytes

Checkout-byte normalization is not exposed as a public CLI mode. Checked-out
bytes can differ from Git blob bytes through attributes, EOL conversion,
`ident`, encodings, clean or smudge filters, LFS, sparse checkout, submodules,
and worktree-local config.

If a checkout-filter build is requested through lower-level protocol surfaces
before support is available, sqry returns a source-unavailable error instead of
silently reusing a raw-object artifact. Immutable public CLI loads should use
`raw-git-objects`; dirty worktree captures should use `dirty-snapshot`.

## Dirty Snapshots

Dirty snapshots are exact-byte, point-in-time query targets:

```bash
sqry daemon load-revision . --dirty --include-untracked
```

The fingerprint includes the base `HEAD`, index state, staged paths, unstaged
tracked bytes, deletions, mode and symlink changes, and selected untracked
bytes. sqry hashes the same byte stream it parses, re-scans before publishing,
and retries once if the working tree changes during capture. A second mutation
returns `DirtySnapshotChanged`.

Dirty snapshots are resident-only unless explicitly sealed into a full content
artifact. They must not be treated as reusable immutable graphs.

## Querying Revisions

Use explicit revision selectors for non-live queries:

```bash
sqry query "kind:function AND name:authenticate" --revision-id <id> --json
sqry query "kind:function AND name:authenticate" --revision-ref feature/login --json
```

Machine-readable results for explicit revision queries include revision
provenance: revision id, resolved revision, artifact id when present, and
source-byte mode. Human-readable output identifies the selected revision in the
command summary.

If a selector is ambiguous, sqry returns `RevisionSelectorAmbiguous` rather than
choosing a revision implicitly.

## Managed Worktrees And Agents

Managed worktrees are fallback and agent infrastructure, not the primary
immutable build path.

Default agent rule: one task, one branch, one worktree, one agent. Shared
worktree collaboration must be requested explicitly and should be visible in
status output.

Agent branch rules:

- Allowed automation branches must match configured safe prefixes.
- `main`, `master`, protected release branches, and release-control surfaces
  are rejected unless a reviewed release-control workflow explicitly grants
  access.
- Reusing a branch already checked out in another worktree is refused unless
  detached fallback is explicitly requested.
- sqry never uses `git stash` as an isolation or rollback primitive.

Worktrees isolate files only. They do not isolate ports, databases, Docker
volumes, generated caches, credentials, local services, or other external
resources. Namespace those resources per worktree or document the shared
resource before running parallel agents.

## Cleanup And Budgets

Revision artifacts are stored outside the live workspace graph directory under
the daemon cache root:

```text
$XDG_CACHE_HOME/sqry/revision-graphs/<repo_identity_hash>/<artifact_id>/
```

Live workspace artifacts remain under `.sqry/graph/` for compatibility.

Use dry-run pruning before deleting revision artifacts or managed worktrees:

```bash
sqry daemon prune-revisions --root . --json
sqry daemon prune-revisions --root . --apply
```

Pruning reports bytes, artifact ids, managed worktrees, active-handle refusals,
and budget decisions. Active resident handles pin artifacts from deletion.

## Errors

Common revision-aware errors are intentionally specific:

| Error | Meaning |
|-------|---------|
| `RevisionSelectorAmbiguous` | The selector matched more than one revision or worktree identity. |
| `RevisionObjectMissing` | A required Git object is not available locally. |
| `RevisionSourceUnavailable` | sqry cannot obtain the selected source bytes safely. |
| `CheckoutFilterUnsupported` | Checkout-byte mode needs a filter or normalization input sqry cannot validate. |
| `SubmoduleUnavailable` | The revision contains a gitlink that was not explicitly available for indexing. |
| `DirtySnapshotChanged` | Dirty content changed during capture after the allowed retry. |
| `ArtifactKeyMismatch` | A persisted manifest does not match the artifact id inputs. |
| `ManagedWorktreeInUse` | A worktree or branch is active elsewhere. |
| `RevisionDiskBudgetExceeded` | Unpinned cleanup could not free enough space. |
