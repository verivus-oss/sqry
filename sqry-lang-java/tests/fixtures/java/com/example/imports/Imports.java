// Test fixture: Import declarations
// Tests: Single imports, wildcard imports

package com.example.imports;

import java.util.List;
import java.util.ArrayList;
import java.util.Map;
import java.util.HashMap;
import java.util.*;
import java.io.File;
import java.io.IOException;

public class Imports {

    public static void main(String[] args) throws IOException {
        List<String> list = new ArrayList<>();
        list.add("item1");
        list.add("item2");

        Map<String, Integer> map = new HashMap<>();
        map.put("key", 42);

        Set<String> set = new HashSet<>();
        set.add("element");

        File file = new File("test.txt");
        file.createNewFile();
    }
}
