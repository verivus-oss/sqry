// SPDX-License-Identifier: MIT
namespace billing {

template <typename T>
struct CacheEntry {
public:
    T value;
    const int version = 1;
    static int activeCount;
    int sharedName;
private:
    bool expired;
};

template <typename T>
int CacheEntry<T>::activeCount = 0;

struct AuditEntry {
    int sharedName;
};

int summarize(const CacheEntry<int> &entry) {
    return entry.value + CacheEntry<int>::activeCount;
}

}
