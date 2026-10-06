using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Retains deployment settings in the initialized guest template.</summary>
public static class ConfigProbe
{
    private static IReadOnlyList<KeyValuePair<string, string>> variables = [];

    [Initializer]
    public static Task InitializeAsync()
    {
        variables = DeploymentConfiguration.ReadVariables();
        return Task.CompletedTask;
    }

    /// <summary>Returns the captured names and values in their declared order.</summary>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request, Ctx context)
    {
        var body = new StringBuilder();
        foreach (var variable in variables)
            body.Append(variable.Key).Append('=').Append(variable.Value).Append('\n');
        return Task.FromResult(Response.Text(body.ToString()));
    }
}
