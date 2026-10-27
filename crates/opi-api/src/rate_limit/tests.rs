//! express-rate-limit 7.5.1 with its MemoryStore, `standardHeaders: true`
//! (draft-6) and `legacyHeaders: false`. Header values were checked against
//! the running Express app.

use super::RateLimiter;

fn headers(hit: &super::Hit) -> Vec<(&'static str, String)> {
    hit.headers.clone()
}

#[test]
fn counts_hits_per_key_in_a_fixed_window() {
    let limiter = RateLimiter::new(60_000.0, 2.0);
    let t0 = 1_000_000.0;
    let first = limiter.hit("a", t0);
    assert!(!first.blocked);
    assert_eq!(
        headers(&first),
        [
            ("RateLimit-Policy", "2;w=60".to_string()),
            ("RateLimit-Limit", "2".to_string()),
            ("RateLimit-Remaining", "1".to_string()),
            ("RateLimit-Reset", "60".to_string()),
        ]
    );
    assert!(!limiter.hit("a", t0 + 10.0).blocked);
    // another key has its own count
    assert!(!limiter.hit("b", t0 + 20.0).blocked);
    let third = limiter.hit("a", t0 + 30_500.0);
    assert!(third.blocked);
    assert_eq!(
        headers(&third),
        [
            ("RateLimit-Policy", "2;w=60".to_string()),
            ("RateLimit-Limit", "2".to_string()),
            ("RateLimit-Remaining", "0".to_string()),
            // ceil((reset - now) / 1000)
            ("RateLimit-Reset", "30".to_string()),
            ("Retry-After", "30".to_string()),
        ]
    );
    // the window started at the first hit; it ends at t0 + 60 s
    assert!(limiter.hit("a", t0 + 59_999.0).blocked);
    let after = limiter.hit("a", t0 + 60_000.0);
    assert!(!after.blocked);
    assert_eq!(after.headers[2], ("RateLimit-Remaining", "1".to_string()));
}

#[test]
fn a_zero_reset_omits_the_reset_header_but_not_retry_after() {
    // getResetSeconds rounds up, so only a window under a millisecond away
    // from its end gives 0; a 0 window does it every time.
    let limiter = RateLimiter::new(0.0, 0.0);
    let hit = limiter.hit("a", 5.0);
    assert!(hit.blocked);
    assert_eq!(
        headers(&hit),
        [
            ("RateLimit-Policy", "0;w=0".to_string()),
            ("RateLimit-Limit", "0".to_string()),
            ("RateLimit-Remaining", "0".to_string()),
            ("Retry-After", "0".to_string()),
        ]
    );
}

#[test]
fn a_zero_limit_blocks_every_request() {
    let limiter = RateLimiter::new(60_000.0, 0.0);
    let hit = limiter.hit("a", 0.0);
    assert!(hit.blocked);
    assert_eq!(hit.headers[4], ("Retry-After", "60".to_string()));
}

#[test]
fn a_nan_limit_never_blocks_and_says_nan() {
    // RATE_LIMIT_MAX=x: parseInt gives NaN, `hits > NaN` is false
    let limiter = RateLimiter::new(1500.0, f64::NAN);
    for i in 0..5 {
        let hit = limiter.hit("a", f64::from(i));
        assert!(!hit.blocked);
        assert_eq!(
            headers(&hit),
            [
                ("RateLimit-Policy", "NaN;w=2".to_string()),
                ("RateLimit-Limit", "NaN".to_string()),
                ("RateLimit-Remaining", "NaN".to_string()),
                ("RateLimit-Reset", "2".to_string()),
            ]
        );
    }
}

#[test]
fn a_negative_limit_blocks_and_reports_it() {
    let limiter = RateLimiter::new(1000.0, -1.0);
    let hit = limiter.hit("a", 0.0);
    assert!(hit.blocked);
    assert_eq!(hit.headers[0], ("RateLimit-Policy", "-1;w=1".to_string()));
    assert_eq!(hit.headers[2], ("RateLimit-Remaining", "0".to_string()));
}

#[test]
fn a_full_store_serves_new_keys_untracked_and_keeps_existing_limits() {
    let limiter = RateLimiter::with_max_keys(60_000.0, 1.0, 2);
    let t0 = 1_000_000.0;
    assert!(!limiter.hit("a", t0).blocked);
    assert!(!limiter.hit("b", t0).blocked);
    // "c" is new and the store is full: never limited, nothing recorded
    for i in 0..3 {
        let hit = limiter.hit("c", t0 + f64::from(i));
        assert!(!hit.blocked);
        assert_eq!(
            headers(&hit),
            [
                ("RateLimit-Policy", "1;w=60".to_string()),
                ("RateLimit-Limit", "1".to_string()),
                ("RateLimit-Remaining", "1".to_string()),
                ("RateLimit-Reset", "60".to_string()),
            ]
        );
    }
    assert_eq!(limiter.clients(), 2);
    // tracked keys keep their limits
    assert!(limiter.hit("a", t0 + 10.0).blocked);
    // once the windows end the sweep frees room and "c" is tracked
    assert!(!limiter.hit("c", t0 + 60_001.0).blocked);
    assert!(limiter.hit("c", t0 + 60_002.0).blocked);
    assert_eq!(limiter.clients(), 1);
}

#[test]
fn expired_clients_are_dropped() {
    let limiter = RateLimiter::new(1000.0, 5.0);
    for i in 0..100 {
        limiter.hit(&format!("k{i}"), 0.0);
    }
    limiter.hit("late", 2500.0);
    assert_eq!(limiter.clients(), 1);
}
