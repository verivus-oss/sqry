namespace CrossLanguage.Generics;

public interface IA {}
public interface IB {}

public class CsBox<T> where T : IA {
    public T Value;
}

public class Generics {
    public static T Identity<T>(T value) where T : class, new() {
        return value;
    }

    public static T Combine<T>(T value) where T : IA, IB, notnull {
        return value;
    }
}
