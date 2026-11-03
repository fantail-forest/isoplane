//! DNS answers and cancellation retained independently of a guest Store.

use super::*;
use std::collections::VecDeque;
use std::net::IpAddr;

enum State {
    Pending,
    Ready(Result<VecDeque<IpAddr>, Error>),
    Dropped,
}

pub(super) struct Addresses {
    state: Arc<Mutex<State>>,
    /// The lookup not yet started: the dispatch had no future to spare when
    /// the guest asked, so the poll that finds one starts it.
    deferred: Option<Lookup>,
    task: Option<ReactorWork>,
}

struct Lookup {
    name: String,
    maximum: ResourceCount,
}

impl Addresses {
    pub(super) fn new(name: String, maximum: ResourceCount) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Pending)),
            deferred: Some(Lookup { name, maximum }),
            task: None,
        }
    }

    /// Starts the lookup on the reactor if it has not started and the
    /// dispatch has a future for it.
    pub(super) fn arm(&mut self, bindings: &dyn Context) -> Result<(), Error> {
        let Some(lookup) = self.deferred.take() else {
            return Ok(());
        };
        let runtime = bindings
            .runtime()
            .ok_or(Error::Unsupported("DNS without reactor"))?
            .clone();
        let Some(future) = bindings.alloc(isoplane_abi::native_io::canonical::SOCKET_IMPORT) else {
            self.deferred = Some(lookup);
            return Ok(());
        };
        let Lookup { name, maximum } = lookup;
        let completion = runtime.clone();
        let answer = self.state.clone();
        let allowlist = bindings.reach().allowlist.map(<[String]>::to_vec);
        let exceptions = bindings.reach().exceptions.to_vec();
        let tenant = bindings.tenant().to_owned();
        let instance = bindings.instance();
        let task = runtime.spawn(async move {
            let result = completion
                .egress
                .resolve_name(
                    &name,
                    crate::egress::Reach {
                        allowlist: allowlist.as_deref(),
                        exceptions: &exceptions,
                    },
                )
                .await
                .map_err(|error| match error {
                    crate::egress::EgressError::Denied(reason) => {
                        completion
                            .metrics
                            .egress_denials
                            .fetch_add(1, Ordering::Relaxed);
                        Error::Denied(reason.to_string())
                    }
                    crate::egress::EgressError::Dns(error) => Error::Dns {
                        kind: error.kind(),
                        message: error.to_string(),
                    },
                })
                .and_then(|addresses| bounded_answer(addresses, maximum));
            if let Err(error) = &result {
                tracing::warn!(%tenant, %instance, %error, "outbound DNS failed");
            }
            {
                let mut state = answer.lock();
                if !matches!(*state, State::Pending) {
                    return;
                }
                *state = State::Ready(result);
            }
            completion.settle_future(future, ResultRecord::Ok(Vec::new()));
        });
        runtime.futures.attach_task(future, task.abort_handle());
        self.task = Some(ReactorWork {
            future: FutureId::from(future),
            owner: bindings.instance(),
            table: runtime.futures.clone(),
            task: task.abort_handle(),
        });
        Ok(())
    }

    pub(super) fn ready(&self) -> bool {
        !matches!(*self.state.lock(), State::Pending)
    }

    pub(super) fn next(&mut self) -> Result<Option<IpAddr>, Error> {
        match &mut *self.state.lock() {
            State::Pending => Err(Error::WouldBlock),
            State::Ready(Ok(addresses)) => Ok(addresses.pop_front()),
            State::Ready(Err(error)) => Err(error.clone()),
            State::Dropped => Err(Error::InvalidState("DNS stream was dropped")),
        }
    }
}

impl Drop for Addresses {
    fn drop(&mut self) {
        *self.state.lock() = State::Dropped;
    }
}

fn bounded_answer(
    addresses: Vec<IpAddr>,
    maximum: ResourceCount,
) -> Result<VecDeque<IpAddr>, Error> {
    if addresses.len() > maximum.0 {
        Err(Error::ResourceLimit)
    } else {
        Ok(VecDeque::from(addresses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oversized_dns_answer_is_refused_without_retaining_a_partial_answer() {
        let addresses = vec![
            IpAddr::from([93, 184, 216, 34]),
            IpAddr::from([93, 184, 216, 35]),
        ];
        assert!(
            matches!(
                bounded_answer(addresses, ResourceCount(1)),
                Err(Error::ResourceLimit)
            ),
            "DNS must reject an answer larger than the instance resource budget"
        );
    }
}
