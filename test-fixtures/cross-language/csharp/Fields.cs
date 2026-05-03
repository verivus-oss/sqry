namespace CrossLanguage;

public class Ledger {
    public int MutableField;
    public readonly int ImmutableField = 1;
    public static int StaticField;
    private string PrivateField = "";
    public int SharedName;
}

public class Archive {
    public int SharedName;
}
