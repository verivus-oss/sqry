#include <stdlib.h>
#include "nft_fake_macros.h"

/* Forward declaration for cross-file function */
int nft_register_chain(const char *chain_name, int flags);
void nft_unregister_chain(const char *chain_name);

struct nft_set_elem {
    struct list_head list;
    int value;
    unsigned int flags;
};

struct nft_set {
    struct list_head elements;
    int count;
};

/* Main function that exercises multiple cross-file and macro call patterns.
 *
 * Calls:
 *   - list_for_each_entry (macro from nft_fake_macros.h)
 *   - kfree (macro from nft_fake_macros.h)
 *   - nft_set_ext_exists (macro from nft_fake_macros.h)
 *   - nft_register_chain (function from nft_fake_helpers.c)
 *   - nft_unregister_chain (function from nft_fake_helpers.c)
 */
int nft_add_set_elem(struct nft_set *set, struct nft_set_elem *elem) {
    struct nft_set_elem *cur;
    int status;

    /* Macro call: iterator from macros.h */
    list_for_each_entry(cur, &set->elements, list) {
        if (cur->value == elem->value) {
            return -1;
        }
    }

    /* Macro call: extension check from macros.h */
    if (nft_set_ext_exists(elem, 0)) {
        status = nft_register_chain("filter", elem->flags);
        if (status < 0) {
            return status;
        }
    }

    set->count++;

    /* Macro call: free from macros.h (cleanup path) */
    if (elem->flags & 0x80) {
        struct nft_set_elem *tmp = malloc(sizeof(*tmp));
        kfree(tmp);
    }

    return 0;
}

/* Secondary function that also calls cross-file helpers. */
void nft_flush_set(struct nft_set *set) {
    struct nft_set_elem *cur;
    list_for_each_entry(cur, &set->elements, list) {
        nft_unregister_chain("filter");
    }
    set->count = 0;
}
