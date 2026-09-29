using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Reflection;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>Reflection, runtime-created generics and serialization probes.</summary>
internal sealed partial class Reflection
{
    public Task<string> RunAsync(string[] path) => path[0] switch
    {
        "events-after-park" => EventsAfterPark(),
        "native-declaration-after-park" => NativeDeclarationAfterPark(),
        "native-symbol-after-park" => NativeSymbolAfterPark(path[1]),
        "native-library-after-park" => NativeLibraryAfterPark(path[1]),
        "assembly-after-park" => LoadedAssemblyAfterPark(),
        "native-payload-after-park" => NativePayloadAfterPark(path[1]),
        "trusted-identity-after-park" => TrustedIdentityAfterPark(path[1]),
        "dynamic-after-park" => DynamicBindingAfterPark(path[1]),
        "expression-after-park" => ExpressionAfterPark(preferInterpretation: true),
        "expression-default-after-park" => ExpressionAfterPark(preferInterpretation: false),
        "safe-handle-dispose" => Task.FromResult(DisposeSafeHandle()),
        "safe-handle-finalizer-after-park" => FinalizeSafeHandleAfterPark(),
        "emitted-delegate-after-park" => EmittedDelegateAfterPark(),
        _ => Task.FromResult(Run(path)),
    };

    private static string Run(string[] path)
    {
        switch (path[0])
        {
            case "type-by-name":
                return TypeByName(path[1]);
            case "make-generic":
                return MakeGeneric(path[1]);
            case "activator":
                return ActivatorInstances();
            case "attributes":
                return Attributes();
            case "invoke":
                return Invoke();
            case "generic-virtual":
                return GenericVirtual();
            case "dispatch":
                return Dispatch();
            case "enum":
                return Enums();
            case "json":
                return Json(path[1]);
            default:
                throw new ArgumentException("unknown reflection case " + path[0]);
        }
    }

    /// <summary>
    /// A type by name: one the trimmer sees named by a literal, one whose
    /// name is assembled at run time (so unreachable to the trimmer), and a
    /// framework generic instantiation.
    /// </summary>
    private static string TypeByName(string which)
    {
        string name = which switch
        {
            "literal" => "Acceptance.Sample",
            "assembled" => string.Concat("Acceptance.", "Orph", "an"),
            "framework" => typeof(List<int>).AssemblyQualifiedName ?? "",
            _ => throw new ArgumentException("unknown type-by-name case " + which),
        };
        Type? type = Type.GetType(name, throwOnError: false);
        return type is null ? "null" : type.FullName + " instance=" + (Activator.CreateInstance(type) is null ? "null" : "created");
    }

    /// <summary>
    /// An instantiation chosen at run time so neither compiler sees which
    /// one: over a reference type (shared code) and over a value type the
    /// program never instantiates the generic with.
    /// </summary>
    private static string MakeGeneric(string which)
    {
        Type[] arguments = { typeof(Sample), typeof(UnseenPoint), typeof(UnseenSize) };
        int index = which switch
        {
            "reference" => 0,
            "value" => 1 + (Environment.ProcessorCount % 2),
            _ => throw new ArgumentException("unknown make-generic case " + which),
        };
        try
        {
            Type closed = typeof(Holder<>).MakeGenericType(arguments[index]);
            var holder = (IDescribable?)Activator.CreateInstance(closed);
            MethodInfo method = typeof(Reflection).GetMethod(nameof(NameOf), BindingFlags.NonPublic | BindingFlags.Static)!
                .MakeGenericMethod(arguments[index]);
            return (holder?.Describe() ?? "null") + " method=" + (method.Invoke(null, null) ?? "null");
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static string NameOf<T>() => typeof(T).Name;

    private static string ActivatorInstances()
    {
        var byType = (Sample?)Activator.CreateInstance(typeof(Sample));
        var withArguments = (Sample?)Activator.CreateInstance(typeof(Sample), "named", 3);
        Sample generic = Activator.CreateInstance<Sample>();
        return (byType?.Describe() ?? "null") + "|" + (withArguments?.Describe() ?? "null") + "|" + generic.Describe()
            + "|" + Attempt(() => Activator.CreateInstance(typeof(Sample), "no such constructor")?.ToString() ?? "null");
    }

    private static string Attributes()
    {
        Type type = typeof(Sample);
        var route = type.GetCustomAttribute<RouteAttribute>();
        IEnumerable<string> methods = type
            .GetMethods(BindingFlags.Public | BindingFlags.Instance | BindingFlags.DeclaredOnly)
            .Where(method => method.GetCustomAttribute<RouteAttribute>() is not null)
            .Select(method => method.Name + "@" + method.GetCustomAttribute<RouteAttribute>()!.Path)
            .OrderBy(name => name, StringComparer.Ordinal);
        PropertyInfo[] properties = type.GetProperties();
        var sample = new Sample();
        properties.Single(property => property.Name == nameof(Sample.Count)).SetValue(sample, 9);
        return "class=" + (route?.Path ?? "none")
            + " methods=" + string.Join(",", methods)
            + " properties=" + string.Join(",", properties.Select(property => property.Name + ":" + property.PropertyType.Name).OrderBy(name => name, StringComparer.Ordinal))
            + " set=" + properties.Single(property => property.Name == nameof(Sample.Count)).GetValue(sample);
    }

    private static string Invoke()
    {
        var sample = new Sample("inv", 1);
        MethodInfo greet = typeof(Sample).GetMethod(nameof(Sample.Greet))!;
        string invoked = (string?)greet.Invoke(sample, new object[] { "isoplane" }) ?? "null";
        var bound = greet.CreateDelegate<Func<string, string>>(sample);
        MethodInfo generic = typeof(Sample).GetMethod(nameof(Sample.Echo))!.MakeGenericMethod(typeof(int));
        object? echoed = generic.Invoke(sample, new object[] { 5 });
        ConstructorInfo constructor = typeof(Sample).GetConstructor(new[] { typeof(string), typeof(int) })!;
        var constructed = (Sample)constructor.Invoke(new object[] { "ctor", 2 });
        return invoked + "|" + bound("delegate") + "|" + echoed + "|" + constructed.Describe()
            + "|" + Attempt(() => (greet.Invoke(sample, new object[] { 3 }) ?? "null").ToString() ?? "null");
    }

    private static string GenericVirtual()
    {
        Shape[] shapes = { new Circle(), new Square() };
        var parts = new List<string>();
        foreach (Shape shape in shapes)
        {
            parts.Add(shape.Accept<int>(3) + "/" + shape.Accept<string>("s") + "/" + shape.Accept<UnseenPoint>(new UnseenPoint(1, 2)));
        }
        IVisitor<decimal> visitor = new Circle();
        parts.Add(visitor.Visit(2.5m));
        return string.Join("|", parts);
    }

    private static string Dispatch()
    {
        IAnimal[] animals = { new Dog(), new Cat() };
        int offset = 10;
        Func<int, int>[] functions =
        {
            value => value + offset,
            Double,
            new Dog().Legs,
        };
        Func<int, int> chained = functions.Aggregate((left, right) => value => right(left(value)));
        Delegate open = Double;
        return string.Join(",", animals.Select(animal => animal.Speak()))
            + "|" + string.Join(",", functions.Select(function => function(1)))
            + "|" + chained(1)
            + "|" + (open.DynamicInvoke(21) ?? "null")
            + "|" + Attempt(() => (open.DynamicInvoke("not an int") ?? "null").ToString() ?? "null");
    }

    private static int Double(int value) => value * 2;

    private static string Enums()
    {
        return string.Join("|",
            Enum.Parse<DayOfWeek>("Friday").ToString(),
            Enum.TryParse("friday", ignoreCase: true, out DayOfWeek loose) ? loose.ToString() : "unparsed",
            string.Join(",", Enum.GetNames<Permission>()),
            (Permission.Read | Permission.Execute).ToString(),
            Enum.Parse<Permission>("Read, Write").ToString(),
            Enum.IsDefined(typeof(Permission), 8).ToString(),
            ((Permission)8).ToString(),
            Attempt(() => Enum.Parse<DayOfWeek>("Caturday").ToString()),
            string.Join(",", Enum.GetValues<DayOfWeek>().Select(day => (int)day)));
    }

    private static string Json(string which)
    {
        var sample = new Sample("json", 4) { Tags = { "a", "b" } };
        try
        {
            switch (which)
            {
                case "reflection":
                {
                    string text = JsonSerializer.Serialize(sample);
                    Sample? back = JsonSerializer.Deserialize<Sample>(text);
                    return text + " " + (back?.Describe() ?? "null");
                }
                case "source-generated":
                {
                    string text = JsonSerializer.Serialize(sample, AcceptanceJsonContext.Default.Sample);
                    Sample? back = JsonSerializer.Deserialize(text, AcceptanceJsonContext.Default.Sample);
                    return text + " " + (back?.Describe() ?? "null");
                }
                case "source-generated-options":
                {
                    var options = new JsonSerializerOptions { TypeInfoResolver = AcceptanceJsonContext.Default, WriteIndented = false };
                    string text = JsonSerializer.Serialize(sample, options);
                    var back = JsonSerializer.Deserialize<Sample>(text, options);
                    return text + " " + (back?.Describe() ?? "null");
                }
                case "document":
                {
                    using JsonDocument document = JsonDocument.Parse("""{"name":"doc","count":2,"tags":["x"]}""");
                    JsonElement root = document.RootElement;
                    return root.GetProperty("name").GetString() + " " + root.GetProperty("count").GetInt32().ToString(CultureInfo.InvariantCulture)
                        + " " + root.GetProperty("tags").GetArrayLength().ToString(CultureInfo.InvariantCulture);
                }
                default:
                    throw new ArgumentException("unknown json case " + which);
            }
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static string Attempt(Func<string> operation)
    {
        try
        {
            return operation();
        }
        catch (TargetInvocationException error) when (error.InnerException is not null)
        {
            return "TargetInvocationException/" + error.InnerException.GetType().Name;
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }
}

[AttributeUsage(AttributeTargets.Class | AttributeTargets.Method)]
internal sealed class RouteAttribute(string path) : Attribute
{
    public string Path { get; } = path;
}

/// <summary>A type reflected over by name, attribute, property and method.</summary>
[Route("/sample")]
public sealed class Sample
{
    public Sample()
        : this("default", 0)
    {
    }

    public Sample(string name, int count)
    {
        Name = name;
        Count = count;
    }

    public string Name { get; set; }

    public int Count { get; set; }

    public List<string> Tags { get; set; } = new();

    [Route("/greet")]
    public string Greet(string whom) => Name + " greets " + whom;

    public T Echo<T>(T value) => value;

    public string Describe() => Name + ":" + Count.ToString(CultureInfo.InvariantCulture) + ":" + string.Join("+", Tags);
}

/// <summary>Referenced by no code, so only a name assembled at run time could reach it.</summary>
public sealed class Orphan
{
}

internal interface IDescribable
{
    string Describe();
}

internal sealed class Holder<T> : IDescribable
{
    public string Describe() => "Holder<" + typeof(T).Name + ">";
}

internal readonly record struct UnseenPoint(int X, int Y);

internal readonly record struct UnseenSize(int Width, int Height);

internal interface IVisitor<T>
{
    string Visit(T value);
}

internal abstract class Shape : IVisitor<decimal>
{
    public abstract string Accept<T>(T value);

    public string Visit(decimal value) => GetType().Name + " visits " + value.ToString(CultureInfo.InvariantCulture);
}

internal sealed class Circle : Shape
{
    public override string Accept<T>(T value) => "circle<" + typeof(T).Name + ">" + value;
}

internal sealed class Square : Shape
{
    public override string Accept<T>(T value) => "square<" + typeof(T).Name + ">" + value;
}

internal interface IAnimal
{
    string Speak();
}

internal sealed class Dog : IAnimal
{
    public string Speak() => "woof";

    public int Legs(int value) => value + 4;
}

internal sealed class Cat : IAnimal
{
    public string Speak() => "meow";
}

[Flags]
internal enum Permission
{
    None = 0,
    Read = 1,
    Write = 2,
    Execute = 4,
}

[JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
[JsonSerializable(typeof(Sample))]
internal sealed partial class AcceptanceJsonContext : JsonSerializerContext
{
}
