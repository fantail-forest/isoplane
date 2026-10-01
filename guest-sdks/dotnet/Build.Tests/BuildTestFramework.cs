using Microsoft.Build.Locator;
using Xunit;
using Xunit.Abstractions;
using Xunit.Sdk;

[assembly: TestFramework("Isoplane.Build.Tests.BuildTestFramework", "Build.Tests")]

namespace Isoplane.Build.Tests;

/// <summary>Resolves MSBuild from the executing host's SDK before discovering build-task tests.</summary>
public sealed class BuildTestFramework : XunitTestFramework
{
    /// <summary>Registers the SDK selected for the test working directory.</summary>
    public BuildTestFramework(IMessageSink messages) : base(messages)
    {
        MSBuildLocator.RegisterDefaults();
    }
}
