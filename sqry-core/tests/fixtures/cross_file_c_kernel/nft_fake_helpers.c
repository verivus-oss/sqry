#include <stdlib.h>

/* Cross-file function: registers an NFT chain.
 * Called from nft_add_set_elem to validate chain state. */
int nft_register_chain(const char *chain_name, int flags) {
    if (!chain_name) {
        return -1;
    }
    return flags & 0xFF;
}

/* Another cross-file helper for deregistration. */
void nft_unregister_chain(const char *chain_name) {
    (void)chain_name;
}
