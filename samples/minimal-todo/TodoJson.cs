using System.Text.Json.Serialization;

namespace MinimalTodo;

internal sealed record Todo(int Id, string Title, bool Done);
internal sealed record TodoInput(string Title, bool Done);

[JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
[JsonSerializable(typeof(Todo[]))]
[JsonSerializable(typeof(Todo))]
[JsonSerializable(typeof(TodoInput))]
internal partial class TodoJson : JsonSerializerContext;
