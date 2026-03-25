package com.example.scopes;

class NestedScopes {
    void overlap() {
        int x = 1;
        {
            int x = 2; // invalid - overlap with outer x
            System.out.println(x);
        }
    }

    void disjoint() {
        {
            int x = 1;
            System.out.println(x);
        }
        {
            int x = 2;
            System.out.println(x);
        }
    }
}
