//! [`HostFuture`] and the combinators that make `Task.WhenAll`-style
//! concurrency work with zero threads.
//!
//! A `HostFuture` is a *handle to work the host is doing*, not work the guest
//! is doing. Its `poll` never waits: it either finds a delivered result in the
//! future table or records itself there and returns `Poll::Pending`. That is
//! the entire suspension mechanism -- see [`crate::rt`].

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use serde::de::DeserializeOwned;
use serde::Serialize;
use wisp_abi::result::ResultRecord;

use crate::error::{Result, WispError};
use crate::rt::{self, Registered};

/// A pending host operation, identified by the `future-id: u64` the import
/// returned. Every potentially-slow import returns one immediately; no import
/// waits.
pub struct HostFuture<T> {
    id: u64,
    registered: bool,
    finished: bool,
    decode: fn(&[u8]) -> Result<T>,
    _t: PhantomData<fn() -> T>,
}

impl<T> HostFuture<T> {
    pub(crate) fn new(id: u64, decode: fn(&[u8]) -> Result<T>) -> Self {
        HostFuture {
            id,
            registered: false,
            finished: false,
            decode,
            _t: PhantomData,
        }
    }

    /// The identifier of the host-owned future.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl<T> Future for HostFuture<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `HostFuture` holds no self-references, so it is `Unpin`.
        let me = self.get_mut();
        debug_assert!(!me.finished, "HostFuture polled after completion");
        match rt::take_result(me.id) {
            Some(ResultRecord::Ok(bytes)) => {
                me.finished = true;
                me.registered = false;
                rt::forget(me.id);
                Poll::Ready((me.decode)(&bytes))
            }
            Some(ResultRecord::Err(e)) => {
                me.finished = true;
                me.registered = false;
                rt::forget(me.id);
                Poll::Ready(Err(e.into()))
            }
            None => {
                if !me.registered {
                    match rt::register(me.id) {
                        Registered::Ok => me.registered = true,
                        Registered::TooMany => {
                            me.finished = true;
                            return Poll::Ready(Err(WispError::Protocol(
                                "too many simultaneous pending futures".into(),
                            )));
                        }
                    }
                }
                // THE PARK. Nothing below this line waits; every caller's
                // `.await` turns this into its own `return Poll::Pending`,
                // and `rt::run` returns `PollStatus::Pending` to the host with
                // an empty wasm stack.
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for HostFuture<T> {
    fn drop(&mut self) {
        if self.registered {
            rt::forget(self.id);
        }
    }
}

// ---------------------------------------------------------------------------
// Issuing calls
// ---------------------------------------------------------------------------

pub(crate) fn decode_unit(_: &[u8]) -> Result<()> {
    Ok(())
}


pub(crate) fn decode_postcard<T: DeserializeOwned>(b: &[u8]) -> Result<T> {
    postcard::from_bytes(b).map_err(|e| WispError::Protocol(format!("decode: {e}")))
}

/// Encode `arg`, hand `(ptr, len)` to a future-returning import, and wrap the
/// returned id. The buffer is freed as soon as the import returns: a
/// guest->host pointer is valid only for the duration of the call, and the
/// host copies out before returning.
pub(crate) fn call<A: Serialize, T>(
    imp: unsafe extern "C" fn(i32, i32) -> i64,
    arg: &A,
    decode: fn(&[u8]) -> Result<T>,
) -> HostFuture<T> {
    let buf = postcard::to_allocvec(arg).expect("postcard encode");
    let id = unsafe { imp(buf.as_ptr() as i32, buf.len() as i32) } as u64;
    drop(buf);
    HostFuture::new(id, decode)
}

// ---------------------------------------------------------------------------
// Combinators
// ---------------------------------------------------------------------------

enum MaybeDone<F: Future> {
    Pending(Pin<Box<F>>),
    Done(F::Output),
    Taken,
}

// Safe: the inner future is `Box`-pinned and pins itself; the completed
// output is only ever moved out by value, never pinned in place.
impl<F: Future> Unpin for MaybeDone<F> {}

impl<F: Future> MaybeDone<F> {
    fn poll(&mut self, cx: &mut Context<'_>) -> bool {
        match self {
            MaybeDone::Pending(f) => match f.as_mut().poll(cx) {
                Poll::Ready(v) => {
                    *self = MaybeDone::Done(v);
                    true
                }
                Poll::Pending => false,
            },
            _ => true,
        }
    }
    fn take(&mut self) -> F::Output {
        match core::mem::replace(self, MaybeDone::Taken) {
            MaybeDone::Done(v) => v,
            _ => unreachable!("join output taken twice"),
        }
    }
}

pub struct Join2<A: Future, B: Future> {
    a: MaybeDone<A>,
    b: MaybeDone<B>,
}

/// Await two host operations concurrently. Both imports are issued on the
/// first poll, so both are in flight while the instance is Parked -- this is
/// `Task.WhenAll` with **zero threads**: the concurrency lives entirely in the
/// host's reactor, and the guest holds nothing but a table entry per
/// outstanding future.
pub fn join2<A: Future, B: Future>(a: A, b: B) -> Join2<A, B> {
    Join2 {
        a: MaybeDone::Pending(Box::pin(a)),
        b: MaybeDone::Pending(Box::pin(b)),
    }
}

impl<A: Future, B: Future> Future for Join2<A, B> {
    type Output = (A::Output, B::Output);
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        let a = me.a.poll(cx);
        let b = me.b.poll(cx);
        if a && b {
            Poll::Ready((me.a.take(), me.b.take()))
        } else {
            Poll::Pending
        }
    }
}

pub struct Join3<A: Future, B: Future, C: Future> {
    a: MaybeDone<A>,
    b: MaybeDone<B>,
    c: MaybeDone<C>,
}

pub fn join3<A: Future, B: Future, C: Future>(a: A, b: B, c: C) -> Join3<A, B, C> {
    Join3 {
        a: MaybeDone::Pending(Box::pin(a)),
        b: MaybeDone::Pending(Box::pin(b)),
        c: MaybeDone::Pending(Box::pin(c)),
    }
}

impl<A: Future, B: Future, C: Future> Future for Join3<A, B, C> {
    type Output = (A::Output, B::Output, C::Output);
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        let (a, b, c) = (me.a.poll(cx), me.b.poll(cx), me.c.poll(cx));
        if a && b && c {
            Poll::Ready((me.a.take(), me.b.take(), me.c.take()))
        } else {
            Poll::Pending
        }
    }
}

pub struct JoinAll<F: Future> {
    items: Vec<MaybeDone<F>>,
}

/// Fan-out over N host operations. Bounded by the plan's
/// `pending_futures_per_dispatch` -- one of the three factors in the
/// structural anti-abuse residency bound, which is why park time is free.
pub fn join_all<F: Future>(futs: impl IntoIterator<Item = F>) -> JoinAll<F> {
    JoinAll {
        items: futs
            .into_iter()
            .map(|f| MaybeDone::Pending(Box::pin(f)))
            .collect(),
    }
}

impl<F: Future> Future for JoinAll<F> {
    type Output = Vec<F::Output>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        let mut all = true;
        for it in me.items.iter_mut() {
            all &= it.poll(cx);
        }
        if all {
            Poll::Ready(me.items.iter_mut().map(|i| i.take()).collect())
        } else {
            Poll::Pending
        }
    }
}

pub enum Either<A, B> {
    Left(A),
    Right(B),
}

pub struct Select2<A: Future, B: Future> {
    a: Pin<Box<A>>,
    b: Pin<Box<B>>,
}

/// First of two. Used with [`crate::Ctx::cancelled`] to observe the dispatch's
/// implicit cancellation future at an await point -- the host never re-enters
/// a guest to announce cancellation, so this is the only way to see it.
pub fn select2<A: Future, B: Future>(a: A, b: B) -> Select2<A, B> {
    Select2 {
        a: Box::pin(a),
        b: Box::pin(b),
    }
}

impl<A: Future, B: Future> Future for Select2<A, B> {
    type Output = Either<A::Output, B::Output>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if let Poll::Ready(v) = me.a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(v));
        }
        if let Poll::Ready(v) = me.b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(v));
        }
        Poll::Pending
    }
}

#[cfg(test)]
pub(crate) fn new_unit(id: u64) -> HostFuture<()> {
    HostFuture::new(id, decode_unit)
}
