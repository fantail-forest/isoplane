using System.Text.Json;
using System.Text.Json.Serialization;

namespace Wisp.Build;

internal enum GuestExecution { Interpreted, NativeAot }
internal sealed record ManagedApplication(string Assembly, string Factory, GuestExecution Execution);

internal static class BuildJson
{
    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true,
        RespectRequiredConstructorParameters = true,
        RespectNullableAnnotations = true,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow,
        Converters = { new JsonStringEnumConverter<GuestExecution>(allowIntegerValues: false) }
    };
}
