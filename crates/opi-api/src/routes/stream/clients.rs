//! The open streams of each client IP. A client holds at most
//! STREAM_MAX_PER_CLIENT streams: a newer stream takes the place of the
//! client's oldest, which ends, so a client that reconnects while its old
//! connection still looks open gets its stream at once. Every stream also
//! holds a place of STREAM_MAX_SUBSCRIBERS; a stream that takes the place of
//! the client's oldest takes over that place too.

use {
    std::{
        collections::{HashMap, VecDeque},
        sync::{Arc, Mutex},
    },
    tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot},
};

/// The open streams of every client.
pub struct Clients {
    max_per_client: usize,
    permits: Arc<Semaphore>,
    open: Mutex<Open>,
}

#[derive(Default)]
struct Open {
    /// Each client's streams, oldest first.
    by_client: HashMap<String, VecDeque<Stream>>,
    next_id: u64,
}

struct Stream {
    id: u64,
    permit: OwnedSemaphorePermit,
    /// Dropped to end the stream when a newer one of its client takes its
    /// place.
    _replaced: oneshot::Sender<()>,
}

/// A stream's place: the stream's entry leaves its client's list when the
/// place drops.
pub struct Place {
    clients: Arc<Clients>,
    client: String,
    id: u64,
    /// Completes once a newer stream of the client has taken this place.
    pub replaced: oneshot::Receiver<()>,
}

impl Clients {
    pub fn new(max_subscribers: usize, max_per_client: usize) -> Arc<Clients> {
        Arc::new(Clients {
            max_per_client,
            permits: Arc::new(Semaphore::new(max_subscribers)),
            open: Mutex::default(),
        })
    }

    /// A place for a new stream of `client`: the place of the client's
    /// oldest stream when the client holds STREAM_MAX_PER_CLIENT, otherwise
    /// a free place of STREAM_MAX_SUBSCRIBERS; `None` when none is free.
    pub fn admit(self: &Arc<Self>, client: &str) -> Option<Place> {
        let mut open = self.open.lock().expect("the clients lock");
        let streams = open.by_client.get_mut(client);
        let permit = match streams {
            Some(streams) if streams.len() >= self.max_per_client => {
                let oldest = streams.pop_front().expect("a client's stream");
                tracing::debug!("stream: a newer stream of {client} replaces its oldest");
                oldest.permit
            }
            _ => self.permits.clone().try_acquire_owned().ok()?,
        };
        let id = open.next_id;
        open.next_id += 1;
        let (replace, replaced) = oneshot::channel();
        open.by_client
            .entry(client.to_string())
            .or_default()
            .push_back(Stream {
                id,
                permit,
                _replaced: replace,
            });
        Some(Place {
            clients: self.clone(),
            client: client.to_string(),
            id,
            replaced,
        })
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut open = self.clients.open.lock().expect("the clients lock");
        if let Some(streams) = open.by_client.get_mut(&self.client) {
            streams.retain(|stream| stream.id != self.id);
            if streams.is_empty() {
                open.by_client.remove(&self.client);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_s_newer_stream_replaces_its_oldest() {
        let clients = Clients::new(10, 2);
        let mut first = clients.admit("a").unwrap();
        let mut second = clients.admit("a").unwrap();
        let _other = clients.admit("b").unwrap();
        let _third = clients.admit("a").unwrap();
        assert!(matches!(
            first.replaced.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            second.replaced.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        // the replacement took over the oldest's place
        assert_eq!(clients.permits.available_permits(), 7);
        drop(first);
        assert_eq!(clients.permits.available_permits(), 7);
    }

    #[test]
    fn the_global_cap_refuses_a_client_below_its_own_cap() {
        let clients = Clients::new(1, 5);
        let place = clients.admit("a").unwrap();
        assert!(clients.admit("b").is_none());
        drop(place);
        let _place = clients.admit("b").unwrap();
        assert_eq!(clients.open.lock().unwrap().by_client.len(), 1);
    }
}
