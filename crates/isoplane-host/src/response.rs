//! Response building. A guest may return `done` with a streaming response
//! handle and keep appending chunks while that response remains unfinished.
//! The dispatch ends after the response finishes, not after the slice that
//! initially returned its handle.

use std::collections::HashMap;
use std::mem::size_of;

use isoplane_abi::dispatch::ResponseHead;
use parking_lot::Mutex;
use serde::de::{DeserializeSeed, Error, SeqAccess, Visitor};

/// A response retained until its dispatch completes or its requester takes it.
#[derive(Debug, Clone)]
pub struct Response {
    /// The dispatch that created it.
    pub dispatch: u64,
    /// Response metadata and the complete buffered body, including appended writes.
    pub head: ResponseHead,
    /// Whether the guest closed this response against further writes.
    pub finished: bool,
}

impl Response {
    /// Copies the buffered body, including appended writes, for its requester.
    pub fn body(&self) -> Vec<u8> {
        self.head.body.clone()
    }

    fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.head.body.capacity()
            + self.head.headers.capacity() * size_of::<(String, String)>()
            + self
                .head
                .headers
                .iter()
                .map(|(name, value)| name.capacity() + value.capacity())
                .sum::<usize>()
    }
}

/// Bounds host response buffers independently of the guest's linear memory.
/// The byte ceiling is the application's effective instance-memory limit.
pub struct ResponseTable {
    next: u32,
    open: HashMap<u32, Response>,
    max_count: usize,
    max_bytes: usize,
    retained_bytes: usize,
}

impl ResponseTable {
    /// Creates an empty table with an aggregate count and retained-byte ceiling.
    pub fn new(max_count: usize, max_bytes: u64) -> Self {
        Self {
            next: 0,
            open: HashMap::new(),
            max_count,
            max_bytes: usize::try_from(max_bytes).unwrap_or(usize::MAX),
            retained_bytes: 0,
        }
    }

    /// Checks the encoded head without allocating guest-controlled collections,
    /// then decodes only a head whose metadata and body fit the remaining budget.
    pub(crate) fn decode_head(&self, payload: &[u8]) -> Result<ResponseHead, ResponseError> {
        self.check_count()?;
        let remaining_bytes = self.max_bytes.saturating_sub(self.retained_bytes);
        let mut exceeded = false;
        let mut deserializer = postcard::Deserializer::from_bytes(payload);
        let result = HeadAllocationSeed {
            max_bytes: remaining_bytes,
            exceeded: &mut exceeded,
            owned_headers: None,
        }
        .deserialize(&mut deserializer);
        let head = match result {
            Ok(head) => head,
            Err(_) if exceeded => return Err(ResponseError::BufferLimit),
            Err(error) => return Err(ResponseError::Malformed(error)),
        };
        if !deserializer
            .finalize()
            .map_err(ResponseError::Malformed)?
            .is_empty()
        {
            return Err(ResponseError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        let bytes = size_of::<Response>()
            .checked_add(head.headers_bytes)
            .and_then(|bytes| bytes.checked_add(head.body.len()))
            .ok_or(ResponseError::BufferLimit)?;
        self.check_bytes(bytes)?;
        let mut headers = Vec::new();
        headers
            .try_reserve_exact(head.header_count)
            .map_err(|_| ResponseError::BufferLimit)?;
        let reserved_bytes = headers
            .capacity()
            .checked_sub(head.header_count)
            .and_then(|count| count.checked_mul(size_of::<(String, String)>()))
            .and_then(|extra| bytes.checked_add(extra))
            .ok_or(ResponseError::BufferLimit)?;
        self.check_bytes(reserved_bytes)?;
        let mut deserializer = postcard::Deserializer::from_bytes(payload);
        HeadAllocationSeed {
            max_bytes: remaining_bytes,
            exceeded: &mut exceeded,
            owned_headers: Some(&mut headers),
        }
        .deserialize(&mut deserializer)
        .map_err(ResponseError::Malformed)?;
        Ok(ResponseHead {
            status: head.status,
            headers,
            body: head.body.to_vec(),
            streaming: head.streaming,
        })
    }

    fn check_count(&self) -> Result<(), ResponseError> {
        if self.open.len() >= self.max_count || self.next == u32::MAX {
            return Err(ResponseError::HandleLimit);
        }
        Ok(())
    }

    fn check_bytes(&self, additional_bytes: usize) -> Result<(), ResponseError> {
        if additional_bytes > self.max_bytes.saturating_sub(self.retained_bytes) {
            return Err(ResponseError::BufferLimit);
        }
        Ok(())
    }

    /// Retains a head if both table budgets permit it; zero is never a handle.
    pub fn create(&mut self, dispatch: u64, mut head: ResponseHead) -> Result<u32, ResponseError> {
        self.check_count()?;
        // Deserializers may leave spare collection capacity; retain only the data.
        head.headers = head.headers.into_boxed_slice().into_vec();
        head.body = head.body.into_boxed_slice().into_vec();
        let response = Response {
            dispatch,
            head,
            finished: false,
        };
        let bytes = response.retained_bytes();
        self.check_bytes(bytes)?;
        self.next += 1;
        let handle = self.next;
        self.retained_bytes += bytes;
        self.open.insert(handle, response);
        Ok(handle)
    }

    /// Appends only after checking the aggregate byte ceiling, without retaining
    /// a separate allocation or metadata entry for each write.
    pub fn write(&mut self, handle: u32, chunk: &[u8]) -> Result<usize, ResponseError> {
        let response = self.open.get(&handle).ok_or(ResponseError::NoSuchHandle)?;
        if response.finished {
            return Err(ResponseError::AlreadyFinished);
        }
        self.check_bytes(chunk.len())?;
        let response = self
            .open
            .get_mut(&handle)
            .ok_or(ResponseError::NoSuchHandle)?;
        let previous_bytes = response.head.body.capacity();
        response
            .head
            .body
            .try_reserve_exact(chunk.len())
            .map_err(|_| ResponseError::BufferLimit)?;
        let available_bytes = self.max_bytes - self.retained_bytes;
        if response.head.body.capacity() - previous_bytes > available_bytes {
            response.head.body = std::mem::take(&mut response.head.body)
                .into_boxed_slice()
                .into_vec();
            self.retained_bytes =
                self.retained_bytes - previous_bytes + response.head.body.capacity();
            return Err(ResponseError::BufferLimit);
        }
        response.head.body.extend_from_slice(chunk);
        self.retained_bytes += response.head.body.capacity() - previous_bytes;
        Ok(chunk.len())
    }

    /// Closes a live response against further writes.
    pub fn finish(&mut self, handle: u32) -> Result<(), ResponseError> {
        let response = self
            .open
            .get_mut(&handle)
            .ok_or(ResponseError::NoSuchHandle)?;
        response.finished = true;
        Ok(())
    }

    /// Looks up a retained response.
    pub fn get(&self, handle: u32) -> Option<&Response> {
        self.open.get(&handle)
    }

    /// Transfers a response and releases its retained-byte and count allowances.
    pub fn take(&mut self, handle: u32) -> Option<Response> {
        let response = self.open.remove(&handle)?;
        self.retained_bytes -= response.retained_bytes();
        Some(response)
    }

    /// Transfers an initial body into the bounded streaming transport.
    pub(crate) fn take_body(&mut self, handle: u32) -> Option<Vec<u8>> {
        let response = self.open.get_mut(&handle)?;
        let body = std::mem::take(&mut response.head.body);
        self.retained_bytes -= body.capacity();
        Some(body)
    }

    /// A finished streaming response of `dispatch`.
    pub(crate) fn finished_stream(&self, dispatch: u64) -> Option<u32> {
        self.open.iter().find_map(|(handle, response)| {
            (response.dispatch == dispatch && response.head.streaming && response.finished)
                .then_some(*handle)
        })
    }

    /// Drops only the responses a dispatch left behind and releases their budget.
    pub(crate) fn end_dispatch(&mut self, dispatch: u64) {
        self.open.retain(|_, response| {
            if response.dispatch == dispatch {
                self.retained_bytes -= response.retained_bytes();
                false
            } else {
                true
            }
        });
    }

    /// Releases all response buffers when an instance is discarded.
    pub(crate) fn clear(&mut self) {
        self.open.clear();
        self.retained_bytes = 0;
    }

    /// Whether a streaming response can still receive writes.
    pub fn is_streaming(&self, handle: u32) -> bool {
        self.open
            .get(&handle)
            .is_some_and(|response| response.head.streaming && !response.finished)
    }
}

impl Default for ResponseTable {
    fn default() -> Self {
        Self::new(
            crate::bindings::GuestResourceLimits::default().response_handles,
            isoplane_abi::config::Limits::default().instance_memory_max,
        )
    }
}

/// A response allocation, ownership or protocol refusal.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResponseError {
    /// The handle is absent from this instance.
    #[error("no such response handle")]
    NoSuchHandle,
    /// The guest already closed the response.
    #[error("response already finished")]
    AlreadyFinished,
    /// The instance exhausted its response count or its non-reusable handle IDs.
    #[error("instance response handle limit exceeded")]
    HandleLimit,
    /// The instance cannot retain the additional metadata or body allocation.
    #[error("instance response-buffer byte limit exceeded")]
    BufferLimit,
    /// The head does not satisfy the complete canonical wire encoding.
    #[error("malformed response head: {0}")]
    Malformed(postcard::Error),
}

// ResponseHead owns its buffers, so the admission pass uses the same Serde
// struct/sequence representation while borrowing every string and byte slice.
// The nonempty-head test checks this mirror against the public wire encoding.
#[derive(Debug, PartialEq, Eq)]
struct HeadAllocation<'a> {
    status: u16,
    headers_bytes: usize,
    header_count: usize,
    body: &'a [u8],
    streaming: bool,
}

struct HeadAllocationSeed<'a> {
    max_bytes: usize,
    exceeded: &'a mut bool,
    owned_headers: Option<&'a mut Vec<(String, String)>>,
}

impl<'de> DeserializeSeed<'de> for HeadAllocationSeed<'_> {
    type Value = HeadAllocation<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_struct(
            "ResponseHead",
            &["status", "headers", "body", "streaming"],
            self,
        )
    }
}

struct HeaderAllocationSeed<'a> {
    max_bytes: usize,
    exceeded: &'a mut bool,
    owned_headers: Option<&'a mut Vec<(String, String)>>,
}

struct HeaderAllocation {
    bytes: usize,
    count: usize,
}

impl<'de> DeserializeSeed<'de> for HeaderAllocationSeed<'_> {
    type Value = HeaderAllocation;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for HeadAllocationSeed<'_> {
    type Value = HeadAllocation<'de>;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a response head")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let status = sequence
            .next_element()?
            .ok_or_else(|| Error::invalid_length(0, &self))?;
        let headers = sequence
            .next_element_seed(HeaderAllocationSeed {
                max_bytes: self.max_bytes.saturating_sub(size_of::<Response>()),
                exceeded: self.exceeded,
                owned_headers: self.owned_headers,
            })?
            .ok_or_else(|| Error::custom("missing response headers"))?;
        let body = sequence
            .next_element()?
            .ok_or_else(|| Error::custom("missing response body"))?;
        let streaming = sequence
            .next_element()?
            .ok_or_else(|| Error::custom("missing response streaming flag"))?;
        Ok(HeadAllocation {
            status,
            headers_bytes: headers.bytes,
            header_count: headers.count,
            body,
            streaming,
        })
    }
}

impl<'de> Visitor<'de> for HeaderAllocationSeed<'_> {
    type Value = HeaderAllocation;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("response header pairs")
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut bytes = 0usize;
        let mut count = 0usize;
        while let Some((name, value)) = sequence.next_element::<(&str, &str)>()? {
            let next_bytes = bytes
                .checked_add(size_of::<(String, String)>())
                .and_then(|bytes| bytes.checked_add(name.len()))
                .and_then(|bytes| bytes.checked_add(value.len()));
            match next_bytes {
                Some(next) if next <= self.max_bytes => bytes = next,
                Some(_) | None => {
                    *self.exceeded = true;
                    return Err(Error::custom("response header allocation limit exceeded"));
                }
            }
            if let Some(headers) = self.owned_headers.as_mut() {
                if headers.len() == headers.capacity() {
                    return Err(Error::custom(
                        "response head changed after allocation preflight",
                    ));
                }
                headers.push((name.to_owned(), value.to_owned()));
            }
            count += 1;
        }
        Ok(HeaderAllocation { bytes, count })
    }
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
    fn response_preflight_matches_every_field_of_the_owned_head() {
        let head = ResponseHead {
            status: 201,
            headers: vec![
                ("name".into(), "value".into()),
                ("other".into(), "field".into()),
            ],
            body: vec![0, 255, 1],
            streaming: true,
        };
        let encoded = postcard::to_allocvec(&head).expect("encode canonical head");
        let mut deserializer = postcard::Deserializer::from_bytes(&encoded);
        let mut exceeded = false;
        let allocation = HeadAllocationSeed {
            max_bytes: usize::MAX,
            exceeded: &mut exceeded,
            owned_headers: None,
        }
        .deserialize(&mut deserializer)
        .expect("preflight wire shape matches");
        assert_eq!(
            allocation,
            HeadAllocation {
                status: head.status,
                headers_bytes: head.headers.len() * size_of::<(String, String)>()
                    + head
                        .headers
                        .iter()
                        .map(|(name, value)| name.len() + value.len())
                        .sum::<usize>(),
                header_count: head.headers.len(),
                body: &head.body,
                streaming: head.streaming
            }
        );
        assert!(
            deserializer
                .finalize()
                .expect("finish preflight")
                .is_empty(),
            "preflight consumes every wire field"
        );
        let table = ResponseTable::new(
            1,
            (size_of::<Response>() + allocation.headers_bytes + head.body.len()) as u64,
        );
        assert_eq!(
            table.decode_head(&encoded).expect("boundary head decodes"),
            head
        );
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(matches!(
            table.decode_head(&trailing),
            Err(ResponseError::Malformed(_))
        ));
    }

    #[test]
    fn response_budgets_include_headers_and_release_only_the_ended_dispatch() {
        let bytes = (size_of::<Response>() + 4) as u64;
        let mut table = ResponseTable::new(2, bytes * 2);
        let first = table.create(1, head(false)).expect("first fits");
        let second = table
            .create(2, head(false))
            .expect("exact aggregate boundary fits");
        assert_eq!(
            table.create(1, head(false)),
            Err(ResponseError::HandleLimit)
        );
        assert_eq!(table.write(first, b"x"), Err(ResponseError::BufferLimit));
        table.end_dispatch(1);
        assert!(table.get(second).is_some());
        let third = table
            .create(3, head(false))
            .expect("dispatch teardown releases budget");
        table.take(second).expect("take second");
        assert_eq!(table.write(third, b"four"), Ok(4), "take releases bytes");
        for _ in 0..1000 {
            assert_eq!(table.write(third, b""), Ok(0));
        }
        assert_eq!(
            table.retained_bytes,
            size_of::<Response>() + 8,
            "empty writes retain no chunk metadata"
        );
        table.clear();
        assert_eq!(table.retained_bytes, 0);
        assert!(table.open.is_empty());
        let headers = ResponseHead {
            headers: vec![(String::new(), String::new())],
            ..head(false)
        };
        let mut table = ResponseTable::new(2, bytes);
        assert_eq!(
            table.create(1, headers),
            Err(ResponseError::BufferLimit),
            "even empty headers retain pair metadata"
        );
    }

    #[test]
    fn head_preflight_rejects_metadata_before_allocating_or_scanning_the_rest() {
        let table = ResponseTable::new(
            1,
            (size_of::<Response>() + size_of::<(String, String)>()) as u64,
        );
        let head = ResponseHead {
            status: 200,
            headers: vec![(String::new(), String::new()); 128],
            body: vec![],
            streaming: false,
        };
        let mut encoded = postcard::to_allocvec(&head).expect("encode metadata attack");
        encoded.truncate(12);
        assert_eq!(
            table.decode_head(&encoded),
            Err(ResponseError::BufferLimit),
            "header budget must reject before the deliberately missing body"
        );
        assert_eq!(table.retained_bytes, 0);
        assert!(table.open.is_empty());
        let zero = ResponseTable::new(0, u64::MAX);
        assert_eq!(
            zero.decode_head(&[]),
            Err(ResponseError::HandleLimit),
            "count rejection precedes decode"
        );
        let zero = ResponseTable::new(1, 0);
        assert_eq!(
            zero.decode_head(&postcard::to_allocvec(&super::tests::head(false)).expect("encode")),
            Err(ResponseError::BufferLimit)
        );
    }

    #[test]
    fn handles_are_never_zero() {
        let mut t = ResponseTable::default();
        assert_ne!(
            t.create(1, head(false)).expect("create"),
            0,
            "0 means `no response` in PollStatus::Done"
        );
    }

    #[test]
    fn streaming_appends_after_done() {
        let mut t = ResponseTable::default();
        let h = t.create(1, head(true)).expect("create");
        assert!(
            t.is_streaming(h),
            "an unfinished streaming response is open"
        );
        t.write(h, b"-one").expect("write");
        t.write(h, b"-two").expect("write");
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
        let h = t.create(1, head(true)).expect("create");
        t.finish(h).expect("finish");
        assert_eq!(
            t.write(h, &[1]),
            Err(ResponseError::AlreadyFinished),
            "a finished response takes no more chunks"
        );
        assert_eq!(
            t.write(999, &[1]),
            Err(ResponseError::NoSuchHandle),
            "an unknown handle is refused"
        );
    }
}
