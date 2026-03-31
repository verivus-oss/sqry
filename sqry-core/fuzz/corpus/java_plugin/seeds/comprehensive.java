package com.example.app;

import java.util.List;
import java.util.ArrayList;
import static java.util.Collections.*;
import com.example.util.*;

/**
 * Comprehensive Java test with qualified names and generics
 */
public class DataProcessor<T extends Comparable<T>> {
    private List<T> data;
    private final String name;

    public DataProcessor(String name) {
        this.name = name;
        this.data = new ArrayList<>();
    }

    public <R> R process(T item, Function<T, R> transform) {
        return transform.apply(item);
    }

    public static class Builder<T extends Comparable<T>> {
        private String name;

        public Builder<T> setName(String name) {
            this.name = name;
            return this;
        }

        public DataProcessor<T> build() {
            return new DataProcessor<>(name);
        }
    }

    @Override
    public String toString() {
        return "DataProcessor{name='" + name + "'}";
    }
}
