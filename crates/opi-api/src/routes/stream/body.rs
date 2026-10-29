//! A stream's response body: the chunks its session queues, in a bounded
//! channel, until the session ends the stream early, which drops what is
//! still queued and sends one last frame, if any.

use {
    axum::body::Bytes,
    futures::Stream,
    std::{
        convert::Infallible,
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    },
    tokio::sync::{mpsc, oneshot},
};

/// The body of a stream.
pub struct SseBody {
    pub chunks: mpsc::Receiver<Bytes>,
    /// The early end and its last frame. A session that ends without it
    /// drops the sender, and the queued chunks are sent.
    pub end: Option<oneshot::Receiver<Option<Bytes>>>,
    pub done: bool,
}

impl Stream for SseBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(end) = self.end.as_mut()
            && let Poll::Ready(end) = Pin::new(end).poll(cx)
        {
            self.end = None;
            // an error: the session finished without ending early
            if let Ok(last) = end {
                self.done = true;
                return Poll::Ready(last.map(Ok));
            }
        }
        match self.chunks.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
