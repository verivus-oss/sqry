package com.example.perf;

import java.util.List;
import java.util.ArrayList;
import java.util.Map;
import java.util.HashMap;

/**
 * Small performance fixture (~100 lines).
 * Tests local variable tracking across diverse scope patterns.
 */
public class PerfSmall100 {

    private final String name;
    private int counter;

    public PerfSmall100(String name) {
        this.name = name;
        this.counter = 0;
    }

    public String processItems(List<String> items) {
        StringBuilder result = new StringBuilder();
        int count = 0;

        for (String item : items) {
            String trimmed = item.trim();
            if (!trimmed.isEmpty()) {
                int length = trimmed.length();
                result.append(trimmed).append(":").append(length).append(",");
                count++;
            }
        }

        String summary = "Processed " + count + " items";
        return result.append(summary).toString();
    }

    public Map<String, Integer> buildHistogram(String text) {
        Map<String, Integer> histogram = new HashMap<>();
        String[] words = text.split("\\s+");

        for (int i = 0; i < words.length; i++) {
            String word = words[i].toLowerCase();
            Integer existing = histogram.get(word);
            int newCount = (existing != null) ? existing + 1 : 1;
            histogram.put(word, newCount);
        }

        return histogram;
    }

    public double computeAverage(int[] values) {
        if (values == null || values.length == 0) {
            return 0.0;
        }

        long sum = 0;
        int min = Integer.MAX_VALUE;
        int max = Integer.MIN_VALUE;

        for (int val : values) {
            sum += val;
            if (val < min) {
                min = val;
            }
            if (val > max) {
                max = val;
            }
        }

        double average = (double) sum / values.length;
        double range = max - min;
        System.out.println("Range: " + range + ", Avg: " + average);
        return average;
    }

    public List<String> filterAndTransform(List<String> input, String prefix) {
        List<String> output = new ArrayList<>();

        try {
            for (String entry : input) {
                String cleaned = entry.strip();
                boolean matches = cleaned.startsWith(prefix);
                if (matches) {
                    String transformed = cleaned.substring(prefix.length()).toUpperCase();
                    output.add(transformed);
                }
            }
        } catch (NullPointerException npe) {
            String errorMsg = "Null encountered: " + npe.getMessage();
            System.err.println(errorMsg);
        } catch (Exception ex) {
            String generalError = "Error: " + ex.getMessage();
            System.err.println(generalError);
            throw new RuntimeException(generalError, ex);
        }

        int resultSize = output.size();
        System.out.println("Filtered " + resultSize + " entries with prefix: " + prefix);
        return output;
    }

    public int nestedScopes(int depth) {
        int accumulator = 0;
        {
            int blockVar = depth * 2;
            accumulator += blockVar;
            {
                int innerBlock = blockVar + 1;
                accumulator += innerBlock;
            }
        }
        for (int i = 0; i < depth; i++) {
            int step = i * 3;
            for (int j = 0; j < i; j++) {
                int product = step * j;
                accumulator += product;
            }
        }
        return accumulator;
    }
}
