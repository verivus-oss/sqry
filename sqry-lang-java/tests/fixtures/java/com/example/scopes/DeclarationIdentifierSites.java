package com.example.scopes;

class DeclarationIdentifierSites {
    void params(int a, String b) {
        int c = 1;
        for (int i = 0; i < 10; i++) {
            System.out.println(i);
        }
        for (String s : new String[]{"a"}) {
            System.out.println(s);
        }
        try {
        } catch (Exception e) {
            System.out.println(e);
        }
    }
}
