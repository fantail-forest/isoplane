//! Response building and streaming.
//!
//! The guest may return `done` with a body-stream handle and continue
//! producing chunks via write futures under `WaitUntil` semantics. So a
//! response handle outlives the call that returned `done`, and the instance
//! stays alive until the stream finishes — which is why the `fresh` reset is
//! owed *after the response **and any `WaitUntil` work***, not after `done`.

use std::collections::HashMap;

use parking_lot::Mutex;
use wisp_abi::dispatch::ResponseHead;

/// A response under construction. `resp_new` creates one, `resp_write` appends
/// (an Unbounded future — a stream cannot be buffered across hibernation),
/// `resp_finish` closes it.
#[derive(Debug, Clone)]
pub struct Response {
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
    pub fn byte_len(&self) -> usize {
        self.head.body.len() + self.chunks.iter().map(|c| c.len()).sum::<usize>()
    }
}

#[derive(Default)]
pub struct ResponseTable {
    next: u32,
    open: HashMap<u32, Response>,
}

impl ResponseTable {
    pub fn new(head: &ResponseHead) -> Self {
        let _ = head;
        Self::default()
    }

    pub fn create(&mut self, head: ResponseHead) -> u32 {
        // Handle 0 is "no response" in `PollStatus::Done`, so start at 1.
        self.next += 1;
        let h = self.next;
        self.open.insert(h, Response { head, chunks: Vec::new(), finished: false });
        h
    }

    pub fn write(&mut self, handle: u32, chunk: Vec<u8>) -> Result<usize, ResponseError> {
        let r = self.open.get_mut(&handle).ok_or(ResponseError::NoSuchHandle)?;
        if r.finished {
            return Err(ResponseError::AlreadyFinished);
        }
        let n = chunk.len();
        r.chunks.push(chunk);
        Ok(n)
    }

    pub fn finish(&mut self, handle: u32) -> Result<(), ResponseError> {
        let r = self.open.get_mut(&handle).ok_or(ResponseError::NoSuchHandle)?;
        r.finished = true;
        Ok(())
    }

    pub fn get(&self, handle: u32) -> Option<&Response> {
        self.open.get(&handle)
    }

    pub fn take(&mut self, handle: u32) -> Option<Response> {
        self.open.remove(&handle)
    }

    pub fn is_streaming(&self, handle: u32) -> bool {
        self.open.get(&handle).is_some_and(|r| r.head.streaming && !r.finished)
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
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
        ResponseHead { status: 200, headers: vec![], body: b"head".to_vec(), streaming }
    }

    #[test]
    fn handles_are_never_zero() {
        let mut t = ResponseTable::default();
        assert_ne!(t.create(head(false)), 0, "0 means `no response` in PollStatus::Done");
    }

    #[test]
    fn streaming_appends_after_done() {
        let mut t = ResponseTable::default();
        let h = t.create(head(true));
        assert!(t.is_streaming(h));
        t.write(h, b"-one".to_vec()).unwrap();
        t.write(h, b"-two".to_vec()).unwrap();
        t.finish(h).unwrap();
        assert!(!t.is_streaming(h));
        assert_eq!(t.take(h).unwrap().body(), b"head-one-two");
    }

    #[test]
    fn writes_after_finish_are_refused() {
        let mut t = ResponseTable::default();
        let h = t.create(head(true));
        t.finish(h).unwrap();
        assert_eq!(t.write(h, vec![1]), Err(ResponseError::AlreadyFinished));
        assert_eq!(t.write(999, vec![1]), Err(ResponseError::NoSuchHandle));
    }
}
