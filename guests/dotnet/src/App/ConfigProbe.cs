using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Isoplane.Durable;

namespace Isoplane.App;

/// <summary>Retains deployment settings in the initialized guest template.</summary>
public sealed class ConfigProbe : IIsoplaneHandler
{
    private readonly IReadOnlyList<KeyValuePair<string, string>> variables = DeploymentConfiguration.ReadVariables();

    /// <summary>Returns the captured names and values in their declared order.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        var body = new StringBuilder();
        foreach (var variable in variables)
            body.Append(variable.Key).Append('=').Append(variable.Value).Append('\n');
        return Task.FromResult(Response.Text(body.ToString()));
    }
}
