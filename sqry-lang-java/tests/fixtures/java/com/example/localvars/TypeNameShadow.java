package com.example.localvars;

import java.util.List;

class TypeNameShadow {
    // method_invocation: List.hashCode() where List is a local variable
    void testMethodInvocation() {
        Object List = new Object();
        int x = List.hashCode();
    }

    // field_access: List.length where List is a local array variable
    void testFieldAccess() {
        int[] List = new int[0];
        int x = List.length;
    }

    // method_reference: List::hashCode where List is a local variable
    void testMethodReference() {
        Object List = new Object();
        Runnable r = List::hashCode;
    }
}
