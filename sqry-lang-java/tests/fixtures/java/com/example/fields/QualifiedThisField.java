package com.example.fields;

class QualifiedThisField {
    int x = 10;

    class Inner {
        void test() {
            int x = 1;
            System.out.println(x);                // resolves to local x
            System.out.println(QualifiedThisField.this.x); // field, skip local
        }
    }
}
