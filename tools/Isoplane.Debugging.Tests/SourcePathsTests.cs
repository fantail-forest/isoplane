using Isoplane.Debugging;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json.Linq;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class SourcePathsTests
{
    [Fact]
    public void source_mapping_prefers_the_longest_directory_prefix()
    {
        var root = Path.GetFullPath("local-sources");
        var library = Path.GetFullPath("library-sources");
        var paths = new SourcePaths();
        paths.Configure(new JObject { ["/build"] = root, ["/build/library"] = library });
        Assert.Equal(Path.Combine(library, "Source.cs"), paths.ToClient("/build/library/Source.cs"));
        Assert.Equal(Path.Combine(root, "App.cs"), paths.ToClient("/build/App.cs"));
        Assert.Equal("/build-other/App.cs", paths.ToClient("/build-other/App.cs"));
        Assert.Equal("/BUILD/App.cs", paths.ToClient("/BUILD/App.cs"));
    }

    [Fact]
    public void windows_build_paths_map_on_any_editor_platform()
    {
        var root = Path.GetFullPath("local-sources");
        var paths = new SourcePaths();
        paths.Configure(new JObject { [@"C:\build\project"] = root });
        Assert.Equal(Path.Combine(root, "nested", "Source.cs"), paths.ToClient(@"c:\BUILD\project\nested\Source.cs"));
    }

    [Theory]
    [InlineData("/", "/nested/App.cs")]
    [InlineData("C:/", "C:/nested/App.cs")]
    public void root_directory_mappings_preserve_the_relative_source_path(string buildRoot, string source)
    {
        var root = Path.GetFullPath("local-sources");
        var paths = new SourcePaths();
        paths.Configure(new JObject { [buildRoot] = root });
        Assert.Equal(Path.Combine(root, "nested", "App.cs"), paths.ToClient(source));
    }

    [Theory]
    [InlineData("[]")]
    [InlineData("{\"C:\":\"/local\"}")]
    [InlineData("{\"relative\":\"/local\"}")]
    [InlineData("{\"/build\":\"relative\"}")]
    [InlineData("{\"/build\":false}")]
    public void invalid_source_mappings_fail_without_replacing_valid_configuration(string json)
    {
        var root = Path.GetFullPath("local-sources");
        var paths = new SourcePaths();
        paths.Configure(new JObject { ["/build"] = root });
        Assert.Throws<ProtocolException>(() => paths.Configure(JToken.Parse(json)));
        Assert.Equal(Path.Combine(root, "App.cs"), paths.ToClient("/build/App.cs"));
    }
}
