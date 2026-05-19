/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic driver-dispatch harness for the sqry C indirect-call
 * precision measurement (Phase A, DESIGN §13.1's `ext4_fops_table.c`
 * companion file).
 *
 * THIS FILE IS NOT VENDORED FROM LINUX. It is a hand-written
 * stand-in that:
 *
 *   - Declares minimal stand-ins for the kernel structs whose
 *     definitions live in <linux/fs.h> etc. — `struct file`,
 *     `struct inode`, `struct file_operations`, `struct
 *     inode_operations`, `struct vm_operations_struct`, `struct
 *     tcp_congestion_ops` — so the C plugin's struct walker stages
 *     the `struct_field_fnptr` entries WITHOUT the kernel header
 *     graph that would push the vendored subset past the 5 KLOC
 *     budget (DESIGN §13.2).
 *
 *   - Provides a handful of synthetic top-level functions that
 *     invoke those struct's function pointers through receivers of
 *     known declared type (`struct file *f`, `struct inode *ip`,
 *     `struct vm_area_struct *vma`, ...). Each callsite exercises
 *     a real `field_expression` and `pointer_expression` shape so
 *     sqry's `IndirectCallsite` capture (graph_builder
 *     `classify_indirect_callsite_shape`) records them and pass5b
 *     can recover a candidate set.
 *
 * The receivers below match the struct types bound in the vendored
 * `file.c` / `dir.c` / `symlink.c`. After Phase 4 unification merges
 * this file's struct definitions with the bindings recorded in those
 * three vendored files, the type-match resolver (DESIGN §4.2 step 3)
 * returns the bound functions as candidates and the
 * `measure_indirect_fanout` example emits non-zero counts per
 * callsite.
 *
 * Carrying this harness inside `linux-driver-subset/` (rather than a
 * sibling directory) keeps the measurement self-contained: a single
 * `cargo run --example measure_indirect_fanout -- linux-driver-subset/`
 * invocation builds one workspace that contains BOTH the GPL-2.0
 * vendored bindings AND this MIT-licensed dispatch harness. The two
 * file-licence pools do not interact at the source-code level —
 * neither file `#include`s the other.
 */

#include <stddef.h>

struct file;
struct inode;
struct dentry;
struct kstat;
struct iattr;
struct iov_iter;
struct kiocb;
struct page;
struct address_space;
struct vm_area_struct;
struct vm_fault;
struct dir_context;
struct path;
struct delayed_call;
struct mnt_idmap;
struct sock;
struct pipe_inode_info;
struct posix_acl;

typedef int vm_fault_t;
typedef long ssize_t;
typedef unsigned int u32;
typedef unsigned long pgoff_t;
typedef unsigned long loff_t;

/* Minimal stand-ins for the kernel structs that ext4's vendored
 * file.c / dir.c / symlink.c bind into. Field shape is taken from
 * the upstream <linux/fs.h> and <linux/mm.h>; only the function-
 * pointer slots referenced by the vendored binding tables are
 * declared. The remaining slots are omitted so sqry's classifier
 * walker has nothing extra to chew through. */

struct file_operations {
    int (*llseek)(struct file *, loff_t, int);
    ssize_t (*read)(struct file *, char *, size_t, loff_t *);
    ssize_t (*read_iter)(struct kiocb *, struct iov_iter *);
    ssize_t (*write_iter)(struct kiocb *, struct iov_iter *);
    int (*iopoll)(struct kiocb *, void *);
    long (*unlocked_ioctl)(struct file *, unsigned int, unsigned long);
    long (*compat_ioctl)(struct file *, unsigned int, unsigned long);
    int (*mmap)(struct file *, struct vm_area_struct *);
    int (*open)(struct inode *, struct file *);
    int (*release)(struct inode *, struct file *);
    int (*fsync)(struct file *, loff_t, loff_t, int);
    unsigned long (*get_unmapped_area)(struct file *, unsigned long,
                                       unsigned long, unsigned long, unsigned long);
    ssize_t (*splice_read)(struct file *, loff_t *, struct pipe_inode_info *,
                           size_t, unsigned int);
    ssize_t (*splice_write)(struct pipe_inode_info *, struct file *, loff_t *,
                            size_t, unsigned int);
    long (*fallocate)(struct file *, int, loff_t, loff_t);
    int (*iterate_shared)(struct file *, struct dir_context *);
};

struct inode_operations {
    void *(*get_link)(struct dentry *, struct inode *, struct delayed_call *);
    int (*setattr)(struct mnt_idmap *, struct dentry *, struct iattr *);
    int (*getattr)(struct mnt_idmap *, const struct path *, struct kstat *, u32, unsigned int);
    ssize_t (*listxattr)(struct dentry *, char *, size_t);
    struct posix_acl *(*get_inode_acl)(struct inode *, int, int);
    int (*set_acl)(struct mnt_idmap *, struct dentry *, struct posix_acl *, int);
    int (*fiemap)(struct inode *, void *, loff_t, loff_t);
    int (*fileattr_get)(struct dentry *, void *);
    int (*fileattr_set)(struct mnt_idmap *, struct dentry *, void *);
};

struct vm_operations_struct {
    vm_fault_t (*fault)(struct vm_fault *);
    vm_fault_t (*huge_fault)(struct vm_fault *, unsigned int);
    vm_fault_t (*page_mkwrite)(struct vm_fault *);
    vm_fault_t (*pfn_mkwrite)(struct vm_fault *);
    void (*map_pages)(struct vm_fault *, pgoff_t, pgoff_t);
};

/* Synthetic dispatchers — each invokes one slot on one struct via a
 * receiver whose declared type is known in scope, so the C plugin's
 * LocalScopeIndex maps the receiver to a struct tag and pass5b can
 * lookup `struct_field_fnptr`. The actual values supplied are
 * irrelevant — only the call shape matters. */

int dispatch_file_read_iter(struct file *f, struct kiocb *iocb, struct iov_iter *iter) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->read_iter(iocb, iter);
}

int dispatch_file_open(struct inode *ip, struct file *f) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->open(ip, f);
}

int dispatch_file_release(struct inode *ip, struct file *f) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->release(ip, f);
}

int dispatch_file_fsync(struct file *f, loff_t a, loff_t b, int datasync) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->fsync(f, a, b, datasync);
}

long dispatch_file_ioctl(struct file *f, unsigned int cmd, unsigned long arg) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->unlocked_ioctl(f, cmd, arg);
}

int dispatch_inode_setattr(struct mnt_idmap *idmap, struct dentry *d, struct iattr *attr,
                           struct inode_operations *iops) {
    return iops->setattr(idmap, d, attr);
}

ssize_t dispatch_inode_listxattr(struct dentry *d, char *buf, size_t sz,
                                  struct inode_operations *iops) {
    return iops->listxattr(d, buf, sz);
}

int dispatch_inode_fiemap(struct inode *ip, void *fi, loff_t s, loff_t l,
                          struct inode_operations *iops) {
    return iops->fiemap(ip, fi, s, l);
}

void *dispatch_inode_get_link(struct dentry *d, struct inode *ip,
                              struct delayed_call *dc,
                              struct inode_operations *iops) {
    return iops->get_link(d, ip, dc);
}

vm_fault_t dispatch_vm_fault(struct vm_fault *vmf, struct vm_operations_struct *vmops) {
    return vmops->fault(vmf);
}

vm_fault_t dispatch_vm_page_mkwrite(struct vm_fault *vmf,
                                    struct vm_operations_struct *vmops) {
    return vmops->page_mkwrite(vmf);
}

int dispatch_dir_iterate(struct file *f, struct dir_context *ctx) {
    struct file_operations *fops = (struct file_operations *)f;
    return fops->iterate_shared(f, ctx);
}
