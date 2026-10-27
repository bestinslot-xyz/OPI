//! express-rate-limit 7's MemoryStore with `standardHeaders: true`
//! (draft-6) and `legacyHeaders: false`.
//!
//! Each key (the client IP) has a fixed window that starts at its first hit
//! and lasts `windowMs`; the hit that ends past the window starts a new one.
//! A request is limited when the window's hits exceed the limit. Numbers
//! follow JavaScript arithmetic, so a NaN limit never limits and reports
//! `NaN`.

use {crate::js::number_to_string, std::collections::HashMap, std::sync::Mutex};

#[cfg(test)]
mod tests;

/// The 429 body express-rate-limit sends with `response.send(message)`.
pub const MESSAGE: &str = "Too many requests, please try again later.";

/// One request's outcome: whether it is limited, and the headers it gets.
#[derive(Debug)]
pub struct Hit {
    pub blocked: bool,
    pub headers: Vec<(&'static str, String)>,
}

struct Client {
    hits: f64,
    reset_at: f64,
}

struct Store {
    clients: HashMap<String, Client>,
    next_sweep: f64,
}

pub struct RateLimiter {
    window_ms: f64,
    limit: f64,
    store: Mutex<Store>,
}

/// `Math.max(a, b)`: NaN when either is.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

impl RateLimiter {
    pub fn new(window_ms: f64, limit: f64) -> Self {
        RateLimiter {
            window_ms,
            limit,
            store: Mutex::new(Store {
                clients: HashMap::new(),
                next_sweep: f64::NEG_INFINITY,
            }),
        }
    }

    /// Counts a request from `key` at `now_ms` (milliseconds since the epoch).
    pub fn hit(&self, key: &str, now_ms: f64) -> Hit {
        let (hits, reset_at) = {
            let mut store = self.store.lock().expect("the rate limit store lock");
            // The MemoryStore drops clients a window after their last hit;
            // a client whose window ended counts from zero either way.
            if now_ms >= store.next_sweep {
                store.clients.retain(|_, c| c.reset_at > now_ms);
                store.next_sweep = now_ms + self.window_ms.max(1.0);
            }
            let client = store.clients.entry(key.to_string()).or_insert(Client {
                hits: 0.0,
                reset_at: now_ms + self.window_ms,
            });
            if client.reset_at <= now_ms {
                client.hits = 0.0;
                client.reset_at = now_ms + self.window_ms;
            }
            client.hits += 1.0;
            (client.hits, client.reset_at)
        };
        let limit = number_to_string(self.limit);
        let window_seconds = number_to_string((self.window_ms / 1000.0).ceil());
        let remaining = js_max(self.limit - hits, 0.0);
        let reset_seconds = js_max(0.0, ((reset_at - now_ms) / 1000.0).ceil());
        let mut headers = vec![
            ("RateLimit-Policy", format!("{limit};w={window_seconds}")),
            ("RateLimit-Limit", limit),
            ("RateLimit-Remaining", number_to_string(remaining)),
        ];
        // `if (resetSeconds)`: 0 and NaN leave the header out
        if reset_seconds != 0.0 && !reset_seconds.is_nan() {
            headers.push(("RateLimit-Reset", number_to_string(reset_seconds)));
        }
        let blocked = hits > self.limit;
        if blocked {
            headers.push(("Retry-After", number_to_string(reset_seconds)));
        }
        Hit { blocked, headers }
    }

    /// The clients the store holds.
    pub fn clients(&self) -> usize {
        self.store
            .lock()
            .expect("the rate limit store lock")
            .clients
            .len()
    }
}
