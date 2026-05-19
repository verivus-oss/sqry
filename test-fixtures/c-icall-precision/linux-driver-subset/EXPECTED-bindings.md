# Expected binding-plane resolutions — linux-driver-subset

For every designated-initializer binding in this subset, sqry pass5b
(Phase A, DESIGN §7) MUST record a `BindingEntry` under
`(struct_qn, field_name) → target_fn`. Cross-file resolution depends on
this table; the integration tests in `sqry-core/tests/` (TEST suite for
U12) assert that every row below resolves correctly.

Each row is `(file:line | struct_qn.field_name → target_fn)`. Line
numbers refer to the vendored files in this directory, NOT the upstream
kernel paths. CONFIG-gated branches (`#ifdef CONFIG_*`) bind the same
slot to different targets in different configurations — pass5b records
all syntactically-visible alternatives as separate `BindingEntry`s.

## `file.c` — `ext4_dax_vm_ops` (`struct vm_operations_struct`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| file.c:767 | `vm_operations_struct.fault` | `ext4_dax_fault` |
| file.c:768 | `vm_operations_struct.huge_fault` | `ext4_dax_huge_fault` |
| file.c:769 | `vm_operations_struct.page_mkwrite` | `ext4_dax_fault` |
| file.c:770 | `vm_operations_struct.pfn_mkwrite` | `ext4_dax_fault` |

## `file.c` — `ext4_file_vm_ops` (`struct vm_operations_struct`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| file.c:777 | `vm_operations_struct.fault` | `filemap_fault` |
| file.c:778 | `vm_operations_struct.map_pages` | `filemap_map_pages` |
| file.c:779 | `vm_operations_struct.page_mkwrite` | `ext4_page_mkwrite` |

## `file.c` — `ext4_file_operations` (`struct file_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| file.c:930 | `file_operations.llseek` | `ext4_llseek` |
| file.c:931 | `file_operations.read_iter` | `ext4_file_read_iter` |
| file.c:932 | `file_operations.write_iter` | `ext4_file_write_iter` |
| file.c:933 | `file_operations.iopoll` | `iocb_bio_iopoll` |
| file.c:934 | `file_operations.unlocked_ioctl` | `ext4_ioctl` |
| file.c:936 | `file_operations.compat_ioctl` | `ext4_compat_ioctl` (CONFIG_COMPAT) |
| file.c:938 | `file_operations.mmap` | `ext4_file_mmap` |
| file.c:939 | `file_operations.open` | `ext4_file_open` |
| file.c:940 | `file_operations.release` | `ext4_release_file` |
| file.c:941 | `file_operations.fsync` | `ext4_sync_file` |
| file.c:942 | `file_operations.get_unmapped_area` | `thp_get_unmapped_area` |
| file.c:943 | `file_operations.splice_read` | `ext4_file_splice_read` |
| file.c:944 | `file_operations.splice_write` | `iter_file_splice_write` |
| file.c:945 | `file_operations.fallocate` | `ext4_fallocate` |

Note: `.fop_flags` (line 946) is a bitmask, not a function-pointer
binding — no `BindingEntry`.

## `file.c` — `ext4_file_inode_operations` (`struct inode_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| file.c:951 | `inode_operations.setattr` | `ext4_setattr` |
| file.c:952 | `inode_operations.getattr` | `ext4_file_getattr` |
| file.c:953 | `inode_operations.listxattr` | `ext4_listxattr` |
| file.c:954 | `inode_operations.get_inode_acl` | `ext4_get_acl` |
| file.c:955 | `inode_operations.set_acl` | `ext4_set_acl` |
| file.c:956 | `inode_operations.fiemap` | `ext4_fiemap` |
| file.c:957 | `inode_operations.fileattr_get` | `ext4_fileattr_get` |
| file.c:958 | `inode_operations.fileattr_set` | `ext4_fileattr_set` |

## `dir.c` — `ext4_dir_operations` (`struct file_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| dir.c:668 | `file_operations.llseek` | `ext4_dir_llseek` |
| dir.c:669 | `file_operations.read` | `generic_read_dir` |
| dir.c:670 | `file_operations.iterate_shared` | `ext4_readdir` |
| dir.c:671 | `file_operations.unlocked_ioctl` | `ext4_ioctl` |
| dir.c:673 | `file_operations.compat_ioctl` | `ext4_compat_ioctl` (CONFIG_COMPAT) |
| dir.c:675 | `file_operations.fsync` | `ext4_sync_file` |
| dir.c:676 | `file_operations.release` | `ext4_release_dir` |

This is the **multi-instance** case from SPEC §3.3.1: `file_operations`
is bound twice in the subset (`ext4_file_operations` in `file.c` AND
`ext4_dir_operations` in `dir.c`), so the type-match resolver alone
would return both `ext4_file_read_iter` and `generic_read_dir` for every
`file_operations.read`-class callsite. The binding plane narrows this
to the correct per-instance target whenever the receiver's qualified
identity is recoverable in scope.

## `symlink.c` — `ext4_encrypted_symlink_inode_operations` (`struct inode_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| symlink.c:118 | `inode_operations.get_link` | `ext4_encrypted_get_link` |
| symlink.c:119 | `inode_operations.setattr` | `ext4_setattr` |
| symlink.c:120 | `inode_operations.getattr` | `ext4_encrypted_symlink_getattr` |
| symlink.c:121 | `inode_operations.listxattr` | `ext4_listxattr` |

## `symlink.c` — `ext4_symlink_inode_operations` (`struct inode_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| symlink.c:125 | `inode_operations.get_link` | `ext4_get_link` |
| symlink.c:126 | `inode_operations.setattr` | `ext4_setattr` |
| symlink.c:127 | `inode_operations.getattr` | `ext4_getattr` |
| symlink.c:128 | `inode_operations.listxattr` | `ext4_listxattr` |

## `symlink.c` — `ext4_fast_symlink_inode_operations` (`struct inode_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| symlink.c:132 | `inode_operations.get_link` | `simple_get_link` |
| symlink.c:133 | `inode_operations.setattr` | `ext4_setattr` |
| symlink.c:134 | `inode_operations.getattr` | `ext4_getattr` |
| symlink.c:135 | `inode_operations.listxattr` | `ext4_listxattr` |

The three `inode_operations` tables in `symlink.c` share field shape but
bind different `.get_link` targets — the binding plane's primary job is
to disambiguate `instance->get_link(...)` callsites against these three
instances rather than returning the type-match union of all three.

## `verity.c` — `ext4_verityops` (`struct fsverity_operations`)

| Callsite | Binding plane | Expected target |
|---|---|---|
| verity.c:392 | `fsverity_operations.begin_enable_verity` | `ext4_begin_enable_verity` |
| verity.c:393 | `fsverity_operations.end_enable_verity` | `ext4_end_enable_verity` |
| verity.c:394 | `fsverity_operations.get_verity_descriptor` | `ext4_get_verity_descriptor` |
| verity.c:395 | `fsverity_operations.read_merkle_tree_page` | `ext4_read_merkle_tree_page` |
| verity.c:396 | `fsverity_operations.write_merkle_tree_block` | `ext4_write_merkle_tree_block` |

The file also contains real indirect callsites (`aops->write_begin(...)` /
`aops->write_end(...)` at lines 83 / 89) that the measurement binary
captures as `IndirectCallsite { shape: FieldExpr { ... } }`. Their
candidate counts depend on what binds into `address_space_operations`
within the bounded corpus; the type-match index is currently empty for
that struct (no ext4 file in the subset defines a
`const struct address_space_operations`), so these two callsites
typically register as `FallbackToStub` in pass5b and are skipped from
the cap-calibration distribution per DESIGN §5.1.

## `fsmap.c` — indirect callsites only (no struct-ops binding tables)

`fsmap.c` defines no struct-ops binding tables. It contains two
indirect callsites — `info->gfi_formatter(...)` at lines 149 / 171 —
where `info` is a local `struct ext4_getfsmap_info *` whose
`gfi_formatter` field is a function pointer. The local-struct
type-token resolves in scope; whether the resolver returns candidates
depends on whether any address-taken function shares that signature
within the bounded corpus.

## `ext4_dispatch_harness.c` — MIT-licensed synthetic dispatcher

Defines stand-ins for `struct file_operations`, `struct
inode_operations`, `struct vm_operations_struct` and provides ~13
dispatcher functions of the shape:

```c
int dispatch_file_read_iter(struct file *f, ...) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->read_iter(iocb, iter);
}
```

Each `fops->slot(...)` / `iops->slot(...)` / `vmops->slot(...)`
invocation is a `field_expression` callsite. The receivers
(`fops`, `iops`, `vmops`) are local declarations whose struct type
is in scope, so the C plugin's LocalScopeIndex maps the receiver to
a struct tag and pass5b's binding-plane / type-match resolver can
compute a candidate set against the vendored binding tables above.

After type-match resolution under the seeded `fn_signature` table,
expected per-slot candidate counts on the current corpus (from one
empirical run; values may drift with kernel-source revisions and
will be re-measured by U17):

| Dispatcher | Slot | Candidate count |
|---|---|---|
| `dispatch_file_read_iter` | `file_operations.read_iter` | 2 |
| `dispatch_file_open` | `file_operations.open` | 3 |
| `dispatch_file_release` | `file_operations.release` | 3 |
| `dispatch_vm_fault` | `vm_operations_struct.fault` | 1 |
| `dispatch_vm_page_mkwrite` | `vm_operations_struct.page_mkwrite` | 1 |
| `dispatch_dir_iterate` | `file_operations.iterate_shared` | 1 |

Other dispatchers resolve to zero candidates because their slot
signatures do not match any address-taken function in the bounded
corpus — they are recorded as `FallbackToStub` in pass5b and are
correctly skipped from the cap-calibration distribution per
DESIGN §5.1.

## Cross-file FFI declarations (NOT recorded as bindings)

The vendored files reference dozens of functions defined elsewhere in
the upstream kernel (`ext4_setattr`, `ext4_ioctl`, `filemap_fault`,
`thp_get_unmapped_area`, etc.). For the fixture's purposes these are
forward-declared via `#include <linux/...>` headers that we do NOT
vendor; sqry's C plugin treats them as unresolved extern declarations.
Their address-taken-ness is recorded per SPEC §3.1.1, but no
`BindingEntry.target_fn` will point at an internally-defined node — it
will point at the externally-declared stub. The U12 integration tests
account for this by asserting on `(struct_qn, field_name)` keys, not on
the target NodeId payload, for cross-file referents.
