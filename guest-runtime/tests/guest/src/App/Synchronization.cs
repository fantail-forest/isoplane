using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Synchronous waits and lock acquisitions on the guest's one thread. A call
/// that can be satisfied at once answers "acquired", a try that cannot answers
/// "refused", and any other outcome is named by its exception type, so a wait
/// that would have to block shows as the exception it refused with.
/// </summary>
internal static class Synchronization
{
    private static readonly TimeSpan Second = TimeSpan.FromSeconds(1);

    internal static string Run(string[] path)
    {
        string scenario = string.Join('/', path);
        if (!Scenarios.TryGetValue(scenario, out Func<bool>? attempt))
        {
            throw new ArgumentException("unknown synchronization scenario " + scenario);
        }
        try
        {
            return attempt() ? "acquired" : "refused";
        }
        catch (BrokenState broken)
        {
            return "broken: " + broken.Message;
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static readonly Dictionary<string, Func<bool>> Scenarios = new()
    {
        ["semaphore-slim/free/wait"] = () => Taken(new SemaphoreSlim(1), semaphore => semaphore.Wait()),
        ["semaphore-slim/free/wait-zero"] = () => new SemaphoreSlim(1).Wait(0),
        ["semaphore-slim/free/wait-timeout"] = () => new SemaphoreSlim(1).Wait(Second),
        ["semaphore-slim/free/wait-token"] = () => Taken(new SemaphoreSlim(1), semaphore => semaphore.Wait(new CancellationTokenSource().Token)),
        ["semaphore-slim/held/wait-zero"] = () => HeldSemaphore(semaphore => semaphore.Wait(0)),
        ["semaphore-slim/held/wait"] = () => HeldSemaphore(semaphore => { semaphore.Wait(); return true; }),
        ["semaphore-slim/held/wait-timeout"] = () => HeldSemaphore(semaphore => semaphore.Wait(Second)),
        ["semaphore-slim/held/wait-token"] = () => HeldSemaphore(semaphore => { semaphore.Wait(new CancellationTokenSource().Token); return true; }),
        ["semaphore-slim/held/wait-cancelled"] = () => HeldSemaphore(semaphore => { semaphore.Wait(new CancellationToken(canceled: true)); return true; }),
        ["semaphore-slim/async-waiter/wait"] = SemaphoreWithAsyncWaiter,

        ["manual-reset-event-slim/set/wait"] = () => { new ManualResetEventSlim(true).Wait(); return true; },
        ["manual-reset-event-slim/set/wait-zero"] = () => new ManualResetEventSlim(true).Wait(0),
        ["manual-reset-event-slim/unset/wait-zero"] = () => new ManualResetEventSlim(false).Wait(0),
        ["manual-reset-event-slim/unset/wait"] = () => { new ManualResetEventSlim(false).Wait(); return true; },
        ["manual-reset-event-slim/unset/wait-timeout"] = () => new ManualResetEventSlim(false).Wait(Second),

        ["manual-reset-event/set/wait-one"] = () => new ManualResetEvent(true).WaitOne(),
        ["manual-reset-event/unset/wait-one-zero"] = () => new ManualResetEvent(false).WaitOne(0),
        ["manual-reset-event/unset/wait-one"] = () => new ManualResetEvent(false).WaitOne(),
        ["manual-reset-event/unset/wait-one-timeout"] = () => new ManualResetEvent(false).WaitOne(Second),
        ["auto-reset-event/set/wait-one"] = AutoResetEventConsumesItsSignal,
        ["auto-reset-event/unset/wait-one"] = () => new AutoResetEvent(false).WaitOne(),

        ["wait-handle/wait-any/one-set"] = () => WaitHandle.WaitAny([new ManualResetEvent(false), new ManualResetEvent(true)]) == 1,
        ["wait-handle/wait-any/none-set-zero"] = () => WaitHandle.WaitAny([new ManualResetEvent(false)], 0) != WaitHandle.WaitTimeout,
        ["wait-handle/wait-any/none-set"] = () => WaitHandle.WaitAny([new ManualResetEvent(false)]) == 0,
        ["wait-handle/wait-all/all-set"] = () => WaitHandle.WaitAll([new ManualResetEvent(true), new ManualResetEvent(true)]),
        ["wait-handle/wait-all/one-unset-zero"] = () => WaitHandle.WaitAll([new ManualResetEvent(true), new ManualResetEvent(false)], 0),
        ["wait-handle/wait-all/one-unset"] = () => WaitHandle.WaitAll([new ManualResetEvent(true), new ManualResetEvent(false)]),

        ["semaphore/free/wait-one"] = () => new Semaphore(1, 1).WaitOne(),
        ["semaphore/held/wait-one-zero"] = () => new Semaphore(0, 1).WaitOne(0),
        ["semaphore/held/wait-one"] = () => new Semaphore(0, 1).WaitOne(),
        ["mutex/free/wait-one"] = () => Released(new Mutex(), mutex => mutex.WaitOne()),
        ["mutex/owned/wait-one"] = MutexIsRecursive,

        ["monitor/enter"] = () => Monitored((gate, taken) => Monitor.Enter(gate, ref taken)),
        ["monitor/try-enter"] = () => Monitored((gate, taken) => Monitor.TryEnter(gate, ref taken)),
        ["monitor/try-enter-timeout"] = () => Monitored((gate, taken) => Monitor.TryEnter(gate, Second, ref taken)),
        ["monitor/lock"] = LockStatementIsRecursive,
        ["monitor/wait-zero"] = () => WhileHolding(gate => Monitor.Wait(gate, 0)),
        ["monitor/wait"] = () => WhileHolding(gate => Monitor.Wait(gate)),
        ["monitor/wait-timeout"] = () => WhileHolding(gate => Monitor.Wait(gate, Second)),

        ["lock/enter"] = LockIsRecursive,
        ["lock/try-enter"] = () => Exited(new Lock(), gate => gate.TryEnter()),
        ["lock/try-enter-timeout"] = () => Exited(new Lock(), gate => gate.TryEnter(Second)),
        ["lock/enter-scope"] = () => { var gate = new Lock(); using (gate.EnterScope()) { return gate.IsHeldByCurrentThread; } },

        ["spin-lock/enter"] = () => SpinLocked(new SpinLock(), (ref SpinLock gate, ref bool taken) => gate.Enter(ref taken)),
        ["spin-lock/try-enter"] = () => SpinLocked(new SpinLock(), (ref SpinLock gate, ref bool taken) => gate.TryEnter(ref taken)),
        ["spin-lock/try-enter-timeout"] = () => SpinLocked(new SpinLock(), (ref SpinLock gate, ref bool taken) => gate.TryEnter(Second, ref taken)),
        ["spin-lock/held/try-enter-zero"] = () => HeldSpinLock((ref SpinLock gate, ref bool taken) => gate.TryEnter(0, ref taken)),
        ["spin-lock/held/enter"] = () => HeldSpinLock((ref SpinLock gate, ref bool taken) => gate.Enter(ref taken)),
        ["spin-lock/owned/enter"] = OwnedSpinLockRefusesRecursion,

        ["reader-writer-lock-slim/read"] = () => Slim(rw => { rw.EnterReadLock(); return rw.IsReadLockHeld; }),
        ["reader-writer-lock-slim/read-zero"] = () => Slim(rw => rw.TryEnterReadLock(0)),
        ["reader-writer-lock-slim/read-timeout"] = () => Slim(rw => rw.TryEnterReadLock(Second)),
        ["reader-writer-lock-slim/write"] = () => Slim(rw => { rw.EnterWriteLock(); return rw.IsWriteLockHeld; }),
        ["reader-writer-lock-slim/write-zero"] = () => Slim(rw => rw.TryEnterWriteLock(0)),
        ["reader-writer-lock-slim/write-timeout"] = () => Slim(rw => rw.TryEnterWriteLock(Second)),
        ["reader-writer-lock-slim/upgradeable"] = () => Slim(rw => { rw.EnterUpgradeableReadLock(); return rw.IsUpgradeableReadLockHeld; }),
        ["reader-writer-lock-slim/upgradeable-zero"] = () => Slim(rw => rw.TryEnterUpgradeableReadLock(0)),
        ["reader-writer-lock-slim/upgradeable-timeout"] = () => Slim(rw => rw.TryEnterUpgradeableReadLock(Second)),
        ["reader-writer-lock-slim/upgrade"] = () => Slim(rw => { rw.EnterUpgradeableReadLock(); rw.EnterWriteLock(); return rw.IsWriteLockHeld; }),
        ["reader-writer-lock-slim/read-held/write"] = () => Slim(rw => { rw.EnterReadLock(); rw.EnterWriteLock(); return true; }),

        ["reader-writer-lock/read"] = () => Legacy(rw => { rw.AcquireReaderLock(Timeout.Infinite); return rw.IsReaderLockHeld; }),
        ["reader-writer-lock/read-timeout"] = () => Legacy(rw => { rw.AcquireReaderLock(Second); return rw.IsReaderLockHeld; }),
        ["reader-writer-lock/write"] = () => Legacy(rw => { rw.AcquireWriterLock(Timeout.Infinite); return rw.IsWriterLockHeld; }),
        ["reader-writer-lock/write-timeout"] = () => Legacy(rw => { rw.AcquireWriterLock(Second); return rw.IsWriterLockHeld; }),
        ["reader-writer-lock/upgrade"] = () => Legacy(rw => { rw.AcquireReaderLock(Timeout.Infinite); rw.UpgradeToWriterLock(Timeout.Infinite); return rw.IsWriterLockHeld; }),

        ["countdown-event/reached/wait"] = () => { var countdown = new CountdownEvent(1); countdown.Signal(); countdown.Wait(); return true; },
        ["countdown-event/pending/wait-zero"] = () => new CountdownEvent(1).Wait(0),
        ["countdown-event/pending/wait"] = () => { new CountdownEvent(1).Wait(); return true; },
        ["barrier/one-participant/signal-and-wait"] = () => { var barrier = new Barrier(1); barrier.SignalAndWait(); return barrier.CurrentPhaseNumber == 1; },
        ["barrier/two-participants/signal-and-wait-zero"] = () => new Barrier(2).SignalAndWait(0),
        ["barrier/two-participants/signal-and-wait"] = BarrierRefusalLeavesTheParticipantOut,

        ["task/completed/wait"] = () => { Task.FromResult(1).Wait(); return true; },
        ["task/completed/result"] = () => Task.FromResult(1).Result == 1,
        ["task/completed/wait-all"] = () => { Task.WaitAll(Task.CompletedTask, Task.FromResult(1)); return true; },
        ["task/completed/wait-any"] = () => Task.WaitAny(Task.CompletedTask) == 0,
        ["task/faulted/wait"] = () => { Task.FromException(new InvalidOperationException()).Wait(); return true; },
        ["task/pending/wait-zero"] = () => new TaskCompletionSource().Task.Wait(0),
        ["task/pending/wait"] = () => { new TaskCompletionSource().Task.Wait(); return true; },
        ["task/pending/result"] = () => new TaskCompletionSource<bool>().Task.Result,
        ["task/pending/wait-all-zero"] = () => Task.WaitAll([Task.CompletedTask, new TaskCompletionSource().Task], 0),
        ["task/pending/wait-all"] = () => { Task.WaitAll(Task.CompletedTask, new TaskCompletionSource().Task); return true; },
        ["task/pending/wait-any-zero"] = () => Task.WaitAny([new TaskCompletionSource().Task], 0) == 0,
        ["task/pending/wait-any"] = () => Task.WaitAny(new TaskCompletionSource().Task) == 0,
        ["task/queued/wait"] = () => { Task.Run(() => { }).Wait(); return true; },

        ["spin-wait/until-true"] = () => { SpinWait.SpinUntil(() => true); return true; },
        ["spin-wait/until-false-zero"] = () => SpinWait.SpinUntil(() => false, 0),
        ["spin-wait/until-false-timeout"] = () => SpinWait.SpinUntil(() => false, Second),
        ["spin-wait/until-false"] = () => { SpinWait.SpinUntil(() => false); return true; },
        ["thread/sleep-zero"] = () => { Thread.Sleep(0); return true; },
        ["thread/yield"] = () => { Thread.Yield(); return true; },
        ["thread/sleep"] = () => { Thread.Sleep(10); return true; },
        ["thread/join-self-zero"] = () => Thread.CurrentThread.Join(0),
        ["thread/join-self-timeout"] = () => Thread.CurrentThread.Join(Second),
        ["thread/join-self"] = () => { Thread.CurrentThread.Join(); return true; },
        ["thread/join-unstarted"] = () => { new Thread(() => { }).Join(); return true; },

        ["blocking-collection/item/take"] = () => Filled().Take() == 1,
        ["blocking-collection/item/try-take"] = () => Filled().TryTake(out _),
        ["blocking-collection/item/take-from-any"] = () => BlockingCollection<int>.TakeFromAny([new BlockingCollection<int>(), Filled()], out _) == 1,
        ["blocking-collection/empty/try-take"] = () => new BlockingCollection<int>().TryTake(out _),
        ["blocking-collection/empty/take"] = EmptyCollectionRefusesTake,
        ["blocking-collection/room/add"] = () => { var bounded = new BlockingCollection<int>(1); bounded.Add(1); return bounded.Count == 1; },
        ["blocking-collection/full/try-add"] = () => Full().TryAdd(2),
        ["blocking-collection/full/add"] = () => { Full().Add(2); return true; },

        ["lazy/execution-and-publication"] = () => new Lazy<int>(() => 42).Value == 42,
        ["concurrent/dictionary"] = ConcurrentDictionaryUpdates,
        ["concurrent/queue"] = () => { var queue = new ConcurrentQueue<int>(); queue.Enqueue(1); return queue.TryDequeue(out int value) && value == 1; },
        ["concurrent/stack"] = () => { var stack = new ConcurrentStack<int>(); stack.Push(1); return stack.TryPop(out int value) && value == 1; },
        ["concurrent/bag"] = () => { var bag = new ConcurrentBag<int> { 1 }; return bag.TryTake(out int value) && value == 1; },
    };

    private delegate void SpinLockAttempt(ref SpinLock gate, ref bool taken);

    private sealed class BrokenState(string message) : Exception(message);

    private static void Require(bool condition, string what)
    {
        if (!condition) throw new BrokenState(what);
    }

    private static bool Taken(SemaphoreSlim semaphore, Action<SemaphoreSlim> wait)
    {
        wait(semaphore);
        Require(semaphore.CurrentCount == 0, "the wait returned without taking the slot");
        return true;
    }

    /// <summary>A refused wait must leave the semaphore as it found it: a release is still there to take.</summary>
    private static bool HeldSemaphore(Func<SemaphoreSlim, bool> wait)
    {
        var semaphore = new SemaphoreSlim(0);
        try
        {
            return wait(semaphore);
        }
        finally
        {
            semaphore.Release();
            Require(semaphore.Wait(0), "a refused wait consumed a later release");
        }
    }

    /// <summary>A refused synchronous wait must not strand a waiter that a release would then feed.</summary>
    private static bool SemaphoreWithAsyncWaiter()
    {
        var semaphore = new SemaphoreSlim(0);
        Task<bool> pending = semaphore.WaitAsync(Timeout.Infinite);
        try
        {
            semaphore.Wait();
            return true;
        }
        finally
        {
            semaphore.Release();
            Require(pending.IsCompletedSuccessfully, "the release did not reach the waiter that was queued first");
            Require(semaphore.CurrentCount == 0, "the release was taken twice");
        }
    }

    private static bool AutoResetEventConsumesItsSignal()
    {
        var signal = new AutoResetEvent(true);
        bool acquired = signal.WaitOne();
        Require(!signal.WaitOne(0), "the wait did not reset the event");
        return acquired;
    }

    private static bool Released(Mutex mutex, Func<Mutex, bool> wait)
    {
        bool acquired = wait(mutex);
        mutex.ReleaseMutex();
        return acquired;
    }

    private static bool MutexIsRecursive()
    {
        using var mutex = new Mutex(initiallyOwned: true);
        bool acquired = mutex.WaitOne();
        mutex.ReleaseMutex();
        mutex.ReleaseMutex();
        return acquired;
    }

    private static bool Monitored(Action<object, bool> enter)
    {
        var gate = new object();
        bool taken = false;
        enter(gate, taken);
        bool held = Monitor.IsEntered(gate);
        if (held) Monitor.Exit(gate);
        return held;
    }

    private static bool LockStatementIsRecursive()
    {
        var gate = new object();
        lock (gate)
        {
            lock (gate)
            {
                return Monitor.IsEntered(gate);
            }
        }
    }

    private static bool WhileHolding(Func<object, bool> wait)
    {
        var gate = new object();
        lock (gate)
        {
            return wait(gate);
        }
    }

    private static bool LockIsRecursive()
    {
        var gate = new Lock();
        gate.Enter();
        gate.Enter();
        bool held = gate.IsHeldByCurrentThread;
        gate.Exit();
        gate.Exit();
        return held;
    }

    private static bool Exited(Lock gate, Func<Lock, bool> enter)
    {
        bool acquired = enter(gate);
        if (acquired) gate.Exit();
        return acquired;
    }

    private static bool SpinLocked(SpinLock gate, SpinLockAttempt enter)
    {
        bool taken = false;
        enter(ref gate, ref taken);
        if (taken) gate.Exit();
        return taken;
    }

    /// <summary>A spin lock without owner tracking that this thread already holds.</summary>
    private static bool HeldSpinLock(SpinLockAttempt enter)
    {
        var gate = new SpinLock(enableThreadOwnerTracking: false);
        bool first = false;
        gate.Enter(ref first);
        bool taken = false;
        enter(ref gate, ref taken);
        return taken;
    }

    private static bool OwnedSpinLockRefusesRecursion()
    {
        var gate = new SpinLock(enableThreadOwnerTracking: true);
        bool first = false;
        gate.Enter(ref first);
        bool second = false;
        gate.Enter(ref second);
        return second;
    }

    private static bool Slim(Func<ReaderWriterLockSlim, bool> acquire) => acquire(new ReaderWriterLockSlim());

    private static bool Legacy(Func<ReaderWriterLock, bool> acquire) => acquire(new ReaderWriterLock());

    private static BlockingCollection<int> Filled()
    {
        var collection = new BlockingCollection<int>();
        collection.Add(1);
        return collection;
    }

    private static BlockingCollection<int> Full()
    {
        var collection = new BlockingCollection<int>(boundedCapacity: 1);
        collection.Add(1);
        return collection;
    }

    /// <summary>A refused take must leave the collection usable: an item added afterwards is the one taken.</summary>
    private static bool EmptyCollectionRefusesTake()
    {
        var collection = new BlockingCollection<int>();
        try
        {
            return collection.Take() == 1;
        }
        finally
        {
            collection.Add(2);
            Require(collection.TryTake(out int item) && item == 2, "a refused take lost the next item");
        }
    }

    /// <summary>A refused wait must withdraw its signal, as a timed-out one does.</summary>
    private static bool BarrierRefusalLeavesTheParticipantOut()
    {
        var barrier = new Barrier(2);
        try
        {
            barrier.SignalAndWait();
            return true;
        }
        finally
        {
            Require(barrier.ParticipantsRemaining == 2, "a refused wait left its signal behind");
        }
    }

    private static bool ConcurrentDictionaryUpdates()
    {
        var dictionary = new ConcurrentDictionary<string, int>();
        dictionary.GetOrAdd("a", 1);
        dictionary.AddOrUpdate("a", 1, (_, value) => value + 1);
        return dictionary.TryRemove("a", out int value) && value == 2;
    }
}
