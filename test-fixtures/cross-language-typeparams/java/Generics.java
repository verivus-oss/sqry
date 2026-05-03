package crosslanguage.generics;

import java.util.List;

interface Bar {}
interface A {}
interface B {}

public class Generics<T extends Number & Comparable<T>> {
    public <U extends Comparable<U>> U max(U left, U right) {
        return left.compareTo(right) >= 0 ? left : right;
    }

    public <V> Generics(V value) {}

    public static <K extends A & B> K choose(K value) {
        return value;
    }

    public interface Box<W extends Bar> {}
}
