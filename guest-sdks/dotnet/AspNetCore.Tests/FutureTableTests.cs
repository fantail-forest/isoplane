using Isoplane.Abi;
using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class FutureTableTests
{
    [Fact]
    public async Task Refused_future_allocation_fails_without_registering_an_unresolvable_wait()
    {
        int before = FutureTable.Count;
        await Assert.ThrowsAsync<IOException>(() => FutureTable.Start(() => 0).Completion);
        Assert.Equal(before, FutureTable.Count);
    }

    [Fact]
    public async Task Operations_past_the_dispatch_cap_start_in_order_as_earlier_ones_complete()
    {
        var dispatch = GuestDispatch.Begin(new DispatchRecord { DispatchId = 7, PendingFuturesLimit = 2 });
        try
        {
            var started = new List<long>();
            var operations = new List<HostOperation>();
            for (long id = 101; id <= 105; id++)
            {
                long future = id;
                operations.Add(FutureTable.Start(() => { started.Add(future); return future; }));
            }
            Assert.Equal(new[] { 101L, 102L }, started);
            Assert.True(FutureTable.Owned(7) == 2 && FutureTable.Waiting(7) == 3, "the cap holds two and queues three");

            Assert.True(FutureTable.Complete(102, Ok(2)), "a completion settles its operation");
            Assert.Equal(new[] { 101L, 102L, 103L }, started);
            Assert.True(FutureTable.Complete(101, Ok(1)) && FutureTable.Complete(103, Ok(3)), "completions release their slots");
            Assert.Equal(new[] { 101L, 102L, 103L, 104L, 105L }, started);
            Assert.True(FutureTable.Complete(104, Ok(4)) && FutureTable.Complete(105, Ok(5)), "the last operations complete");

            for (int index = 0; index < operations.Count; index++)
            {
                var result = await operations[index].Completion;
                Assert.True(result.IsOk && result.Payload[0] == index + 1, $"operation {index + 1} must receive its own result");
            }
            Assert.True(FutureTable.Owned(7) == 0 && FutureTable.Waiting(7) == 0, "nothing is left pending or waiting");
        }
        finally { dispatch.End(); }
    }

    [Fact]
    public async Task A_waiting_operation_cancelled_before_it_starts_never_reaches_the_host()
    {
        var dispatch = GuestDispatch.Begin(new DispatchRecord { DispatchId = 8, PendingFuturesLimit = 1 });
        try
        {
            var first = FutureTable.Start(() => 201);
            bool startedSecond = false;
            var second = FutureTable.Start(() => { startedSecond = true; return 202; });
            second.Cancel();
            var cancelled = await second.Completion;
            Assert.True(!cancelled.IsOk && cancelled.Error == FutureError.Cancelled, "cancellation settles the wait");
            Assert.True(FutureTable.Complete(201, Ok(1)), "the first operation completes");
            Assert.False(startedSecond, "a cancelled operation must not start when its slot frees");
            Assert.True((await first.Completion).IsOk, "the first operation keeps its result");
        }
        finally { dispatch.End(); }
    }

    [Fact]
    public void Work_a_dispatch_leaves_behind_keeps_its_cap_until_the_instance_is_told_its_own()
    {
        var started = new List<long>();
        var dispatch = GuestDispatch.Begin(new DispatchRecord { DispatchId = 9, PendingFuturesLimit = 1 });
        FutureTable.Start(() => { started.Add(301); return 301; });
        FutureTable.Start(() => { started.Add(302); return 302; });
        dispatch.End();
        Assert.Equal(new[] { 301L }, started);
        Assert.True(FutureTable.Owned(FutureTable.InstanceLane) == 1 && FutureTable.Waiting(FutureTable.InstanceLane) == 1,
            "the instance inherits the pending and waiting operations");

        FutureTable.Open(FutureTable.InstanceLane, 2);
        FutureTable.Start(() => { started.Add(303); return 303; });
        Assert.Equal(new[] { 301L, 302L }, started);
        Assert.True(FutureTable.Complete(301, Ok(1)) && FutureTable.Complete(302, Ok(2)) && FutureTable.Complete(303, Ok(3)),
            "the handed-over operations complete under the instance's cap");
        Assert.Equal(new[] { 301L, 302L, 303L }, started);
    }

    private static ResultRecord Ok(byte value) => new() { IsOk = true, Payload = [value] };
}
