using System.Text.Json.Serialization;

namespace TodoApp;

[JsonSerializable(typeof(Todo[]))]
[JsonSerializable(typeof(Todo))]
[JsonSerializable(typeof(TodoInput))]
internal partial class TodoJson : JsonSerializerContext;
