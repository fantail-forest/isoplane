//! Response building. A guest may return `done` with a streaming response
//! handle and keep appending chunks while that response remains unfinished.
//! The dispatch ends after the response finishes, not after the slice that
//! initially returned its handle.

use std::collections::HashMap;

use isoplane_abi::dispatch::ResponseHead;
use parking_lot::Mutex;

/// A response under construction: `resp_new` creates one, `resp_write`
/// appends, `resp_finish` closes it.
#[derive(Debug, Clone)]
pub struct Response {
    /// The dispatch that created it.
    pub dispatch: u64,
    pub head: ResponseHead,
    pub chunks: Vec<Vec<u8>>,
    pub finished: bool,
}

impl Response {
    pub fn body(&self) -> Vec<u8> {
        let mut v = self.head.body.clone();
        for c in &self.chunks {
            v.extend_from_slice(c);
        }
        v
    }
}

#[derive(Default)]
pub struct ResponseTable {
    next: u32,
    open: HashMap<u32, Response>,
}

impl ResponseTable {
    /// Handles start at 1; 0 is "no response" in `PollStatus::Done`.
    pub fn create(&mut self, dispatch: u64, head: ResponseHead) -> u32 {
        self.next += 1;
        let h = self.next;
        self.open.insert(
            h,
            Response {
                dispatch,
                head,
                chunks: Vec::new(),
                finished: false,
            },
        );
        h
    }

    pub fn write(&mut self, handle: u32, chunk: Vec<u8>) -> Result<usize, ResponseError> {
        let r = self
            .open
            .get_mut(&handle)
            .ok_or(ResponseError::NoSuchHandle)?;
        if r.finished {
            return Err(ResponseError::AlreadyFinished);
        }
        let n = chunk.len();
        r.chunks.push(chunk);
        Ok(n)
    }

    pub fn finish(&mut self, handle: u32) -> Result<(), ResponseError> {
        let r = self
            .open
            .get_mut(&handle)
            .ok_or(ResponseError::NoSuchHandle)?;
        r.finished = true;
        Ok(())
    }

    pub fn get(&self, handle: u32) -> Option<&Response> {
        self.open.get(&handle)
    }

    pub fn take(&mut self, handle: u32) -> Option<Response> {
        self.open.remove(&handle)
    }

    /// A finished streaming response of `dispatch`.
    pub(crate) fn finished_stream(&self, dispatch: u64) -> Option<u32> {
        self.open.iter().find_map(|(handle, response)| {
            (response.dispatch == dispatch && response.head.streaming && response.finished)
                .then_some(*handle)
        })
    }

    /// Drops the responses a dispatch left behind.
    pub(crate) fn end_dispatch(&mut self, dispatch: u64) {
        self.open
            .retain(|_, response| response.dispatch != dispatch);
    }

    pub fn is_streaming(&self, handle: u32) -> bool {
        self.open
            .get(&handle)
            .is_some_and(|r| r.head.streaming && !r.finished)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseError {
    NoSuchHandle,
    AlreadyFinished,
}

pub type SharedResponses = Mutex<ResponseTable>;

#[cfg(test)]
mod tests {
    use super::*;

    fn head(streaming: bool) -> ResponseHead {
        ResponseHead {
            status: 200,
            headers: vec![],
            body: b"head".to_vec(),
            streaming,
        }
    }

    #[test]
    fn handles_are_never_zero() {
        let mut t = ResponseTable::default();
        assert_ne!(
            t.create(1, head(false)),
            0,
            "0 means `no response` in PollStatus::Done"
        );
    }

    #[test]
    fn streaming_appends_after_done() {
        let mut t = ResponseTable::default();
        let h = t.create(1, head(true));
        assert!(
            t.is_streaming(h),
            "an unfinished streaming response is open"
        );
        t.write(h, b"-one".to_vec()).expect("write");
        t.write(h, b"-two".to_vec()).expect("write");
        t.finish(h).expect("finish");
        assert!(!t.is_streaming(h), "a finished response is no longer open");
        assert_eq!(
            t.take(h).expect("the response").body(),
            b"head-one-two",
            "chunks follow the head in write order"
        );
    }

    #[test]
    fn writes_after_finish_are_refused() {
        let mut t = ResponseTable::default();
        let h = t.create(1, head(true));
        t.finish(h).expect("finish");
        assert_eq!(
            t.write(h, vec![1]),
            Err(ResponseError::AlreadyFinished),
            "a finished response takes no more chunks"
        );
        assert_eq!(
            t.write(999, vec![1]),
            Err(ResponseError::NoSuchHandle),
            "an unknown handle is refused"
        );
    }
}
