using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class TimerNoticesTests
{
    [Fact]
    public void a_timer_shorter_than_the_minimum_interval_is_woken_once_per_interval()
    {
        Assert.Equal((36_000, 36_000), TimerNotices.WakesPerHour(20, [20], minimumIntervalMs: 100));
    }

    [Fact]
    public void the_instance_estimate_adds_its_timers_up_to_one_wake_per_minimum_interval()
    {
        Assert.Equal((3_600, 6_000), TimerNotices.WakesPerHour(1_000, [1_000, 1_500], minimumIntervalMs: 100));
        Assert.Equal((36_000, 36_000), TimerNotices.WakesPerHour(100, [100, 200, 300], minimumIntervalMs: 100));
    }

    [Fact]
    public void the_notice_states_the_ceiling_per_hour_and_per_month_while_the_app_receives_requests()
    {
        string notice = TimerNotices.Notice("App.Telemetry.Flush", 1_000, [1_000, 1_500], minimumIntervalMs: 100);
        Assert.Contains("Timer App.Telemetry.Flush fires every 1 s.", notice);
        Assert.Contains("up to 3,600 times an hour on its own", notice);
        Assert.Contains(
            "with this instance's 2 periodic timers, it is woken up to 6,000 per hour (4,380,000 per month) while "
            + "the app is receiving at least one request per 5 minutes.",
            notice);
    }

    [Theory]
    [InlineData(20u, "20 ms")]
    [InlineData(1_500u, "1.5 s")]
    [InlineData(300_000u, "5 min")]
    public void periods_are_described_in_the_unit_that_reads_best(uint periodMs, string described)
    {
        Assert.Equal(described, TimerNotices.Duration(periodMs));
    }
}
