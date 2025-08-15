use std::time::Instant;

use crate::database::get_brc20_database;

pub struct EventTimer {
    label: String,
    start_time: Instant,
}

pub fn start_timer(span: impl Into<String>, event: impl Into<String>) -> EventTimer {
    EventTimer {
        label: format!("{}#{}", span.into(), event.into()),
        start_time: Instant::now(),
    }
}

pub async fn stop_timer(logger: &EventTimer) {
    let duration = logger.start_time.elapsed();
    let _ = get_brc20_database()
        .lock()
        .await
        .log_timer(logger.label.clone(), duration.as_nanos())
        .await;
}
