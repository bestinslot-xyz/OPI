use std::time::Instant;

use crate::database::get_brc20_database;

pub struct EventTimer {
    label: String,
    start_time: Instant,
    block_height: i32,
}

pub fn start_timer(span: impl Into<String>, event: impl Into<String>, block_height: i32) -> EventTimer {
    EventTimer {
        label: format!("{}#{}", span.into(), event.into()),
        start_time: Instant::now(),
        block_height,
    }
}

pub async fn stop_timer(logger: &EventTimer) {
    let duration = logger.start_time.elapsed();
    let _ = get_brc20_database()
        .lock()
        .await
        .log_timer(logger.label.clone(), duration.as_nanos(), logger.block_height)
        .await;
}
