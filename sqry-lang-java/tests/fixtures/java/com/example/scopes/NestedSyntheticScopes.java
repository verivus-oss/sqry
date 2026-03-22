package com.example.scopes;

class NestedSyntheticScopes {
    int test(Object obj) {
        return obj instanceof String s && s.length() > 0
            ? s.hashCode()
            : 0;
    }
}
