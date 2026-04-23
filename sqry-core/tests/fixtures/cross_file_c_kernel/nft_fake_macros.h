#ifndef NFT_FAKE_MACROS_H
#define NFT_FAKE_MACROS_H

/* Kernel-style iterator macro */
#define list_for_each_entry(pos, head, member) \
    for (pos = (head)->next; pos != (head); pos = pos->next)

/* Memory management macro */
#define kfree(ptr) do { free(ptr); (ptr) = NULL; } while (0)

/* NFT extension check macro */
#define nft_set_ext_exists(ext, id) ((ext)->flags & (1 << (id)))

/* Type definition for list head */
struct list_head {
    struct list_head *next;
    struct list_head *prev;
};

#endif /* NFT_FAKE_MACROS_H */
