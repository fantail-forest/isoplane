using Mono.Cecil.Cil;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class StatementFlowTests
{
    [Fact]
    public void only_empty_stack_instruction_boundaries_can_be_selected()
    {
        var body = Body(
            Node(0, OpCodes.Ldc_I4_1, pop: 0, push: 1),
            Node(1, OpCodes.Ldc_I4_2, pop: 0, push: 1),
            Node(2, OpCodes.Add, pop: 2, push: 1),
            Node(3, OpCodes.Pop, pop: 1, push: 0),
            Node(4, OpCodes.Ret, pop: 0, push: 0));
        Assert.Equal([0, 4], StatementFlow.EmptyStackOffsets(body).Order().ToArray());
    }

    [Fact]
    public void branch_merges_must_agree_on_the_stack_depth()
    {
        var body = Body(
            Node(0, OpCodes.Ldc_I4_0, pop: 0, push: 1),
            Node(1, OpCodes.Brtrue_S, pop: 1, push: 0, 4),
            Node(3, OpCodes.Ldc_I4_1, pop: 0, push: 1),
            Node(4, OpCodes.Ret, pop: 0, push: 0));
        Assert.Empty(StatementFlow.EmptyStackOffsets(body));
    }

    [Fact]
    public void jumps_can_cross_try_boundaries_but_cannot_enter_or_leave_its_finally()
    {
        var body = new StatementFlow.Body(
            [
                Node(0, OpCodes.Nop, pop: 0, push: 0),
                Node(1, OpCodes.Nop, pop: 0, push: 0),
                Node(2, OpCodes.Leave_S, pop: 0, push: 0, 5),
                Node(4, OpCodes.Endfinally, pop: 0, push: 0),
                Node(5, OpCodes.Ret, pop: 0, push: 0)
            ],
            [new StatementFlow.Handler(Start: 4, End: 5, Filter: null, ExceptionOnStack: false)]);
        Assert.Equal([0, 1, 2, 5], StatementFlow.ReachableTargets(body, body, 0).Order().ToArray());
        Assert.Equal([0, 1, 2, 5], StatementFlow.ReachableTargets(body, body, 5).Order().ToArray());
        Assert.Equal([4], StatementFlow.ReachableTargets(body, body, 4).Order().ToArray());
    }

    [Fact]
    public void nested_exception_regions_are_checked_independently()
    {
        const int outer = 0, inner = 1, leaveInner = 2, caught = 4, afterInner = 7, cleanup = 10, end = 11;
        var body = new StatementFlow.Body(
            [
                Node(outer, OpCodes.Nop, pop: 0, push: 0),
                Node(inner, OpCodes.Nop, pop: 0, push: 0),
                Node(leaveInner, OpCodes.Leave_S, pop: 0, push: 0, afterInner),
                Node(caught, OpCodes.Pop, pop: 1, push: 0),
                Node(5, OpCodes.Rethrow, pop: 0, push: 0),
                Node(afterInner, OpCodes.Nop, pop: 0, push: 0),
                Node(8, OpCodes.Leave_S, pop: 0, push: 0, end),
                Node(cleanup, OpCodes.Endfinally, pop: 0, push: 0),
                Node(end, OpCodes.Ret, pop: 0, push: 0)
            ],
            [
                new StatementFlow.Handler(Start: caught, End: afterInner, Filter: null, ExceptionOnStack: true),
                new StatementFlow.Handler(Start: cleanup, End: end, Filter: null, ExceptionOnStack: false)
            ]);
        var outerTargets = StatementFlow.ReachableTargets(body, body, outer);
        Assert.Contains(afterInner, outerTargets);
        Assert.Contains(inner, outerTargets);
        Assert.DoesNotContain(caught, outerTargets);
        Assert.DoesNotContain(cleanup, outerTargets);
        Assert.Contains(end, outerTargets);
        var innerTargets = StatementFlow.ReachableTargets(body, body, inner);
        Assert.Contains(leaveInner, innerTargets);
        Assert.Contains(afterInner, innerTargets);
    }

    private static StatementFlow.Body Body(params StatementFlow.Node[] instructions) => new(instructions, []);

    private static StatementFlow.Node Node(int offset, OpCode opCode, int pop, int push, params int[] branches) =>
        new(offset, opCode, pop, push, branches);
}
