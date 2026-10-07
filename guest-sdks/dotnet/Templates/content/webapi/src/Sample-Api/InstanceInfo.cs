namespace Sample_Api;

/// <summary>Identifies the instance serving a request.</summary>
public sealed class InstanceInfo
{
    /// <summary>Assigned when the instance activates; empty in the template.</summary>
    public Guid Id { get; private set; }

    internal void Activate() => Id = Guid.NewGuid();
}

/// <summary>What the root endpoint reports about the running service.</summary>
public sealed record ServiceInfo(string Application, string Environment, Guid Instance);
