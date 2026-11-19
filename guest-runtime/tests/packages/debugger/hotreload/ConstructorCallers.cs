namespace Debugger.HotReload;

public static partial class Callers
{
    private static int constructions;

    /// <summary>Adopts edits after class and value-type constructors finish.</summary>
    public static string Construct()
    {
        int preserved = 7;
        int result = 100 + new Constructed(20).Value;
        result += 10; // CLASS_CONSTRUCTION_EDIT
        result += new ConstructedValue(3).Value;
        result += 10; // VALUE_CONSTRUCTION_EDIT
        return $"{result}:{preserved}:{constructions}";
    }

    private sealed class Constructed
    {
        public int Value { get; }

        public Constructed(int value)
        {
            constructions++;
            Value = value;
            System.Diagnostics.Debugger.Break();
        }
    }

    private readonly struct ConstructedValue
    {
        public int Value { get; }

        public ConstructedValue(int value)
        {
            constructions++;
            Value = value;
            System.Diagnostics.Debugger.Break();
        }
    }
}
