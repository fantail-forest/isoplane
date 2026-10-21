using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class FutureTableTests
{
    [Fact]
    public async Task Refused_future_allocation_fails_without_registering_an_unresolvable_wait()
    {
        int before = FutureTable.Count;
        await Assert.ThrowsAsync<IOException>(() => FutureTable.Register(0));
        Assert.Equal(before, FutureTable.Count);
    }
}
