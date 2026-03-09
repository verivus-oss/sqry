#include <vector>
#include "repository.h"
import math;

namespace demo {

class Repository {
public:
    static int save(int value) {
        return value;
    }
};

int helper();

class Service {
public:
    int process(int v);
};

inline int Service::process(int v) {
    return Repository::save(v) + helper();
}

int helper() {
    return 1;
}

} // namespace demo
