//! A stream's response body: the chunks its session queues, in a bounded
//! channel, until the session ends the stream early, which drops what is
//! still queued and sends one last frame, if any. The last frame goes out
//! only between frames: after part of a frame the body just ends, and the
//! client drops the unfinished frame at the end of the stream.

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
    chunks: mpsc::Receiver<Bytes>,
    /// The early end and its last frame. A session that ends without it
    /// drops the sender, and the queued chunks are sent.
    end: Option<oneshot::Receiver<Option<Bytes>>>,
    done: bool,
    /// Whether the chunks sent so far end with a whole frame.
    between_frames: bool,
}

impl SseBody {
    pub fn new(chunks: mpsc::Receiver<Bytes>, end: oneshot::Receiver<Option<Bytes>>) -> SseBody {
        SseBody {
            chunks,
            end: Some(end),
            done: false,
            between_frames: true,
        }
    }
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
                let last = last.filter(|_| self.between_frames);
                if last.is_some() {
                    opi_core::metrics::global().stream_message("error");
                }
                return Poll::Ready(last.map(Ok));
            }
        }
        match self.chunks.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                self.between_frames = chunk.ends_with(b"\n\n");
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, futures::StreamExt};

    async fn body_after(chunks: &[&'static str]) -> Vec<String> {
        let (sender, receiver) = mpsc::channel(8);
        let (end, ended) = oneshot::channel();
        let mut body = SseBody::new(receiver, ended);
        let mut out = Vec::new();
        for chunk in chunks {
            sender
                .send(Bytes::from_static(chunk.as_bytes()))
                .await
                .unwrap();
            let sent = body.next().await.unwrap().unwrap();
            out.push(String::from_utf8(sent.to_vec()).unwrap());
        }
        end.send(Some(Bytes::from_static(b"event: error\ndata: {}\n\n")))
            .unwrap();
        while let Some(chunk) = body.next().await {
            out.push(String::from_utf8(chunk.unwrap().to_vec()).unwrap());
        }
        out
    }

    #[tokio::test]
    async fn the_last_frame_follows_a_whole_frame() {
        assert_eq!(
            body_after(&["event: block\ndata: {", "}\nid: a\n\n"]).await,
            [
                "event: block\ndata: {",
                "}\nid: a\n\n",
                "event: error\ndata: {}\n\n"
            ]
        );
    }

    #[tokio::test]
    async fn a_cut_frame_ends_the_body_without_the_last_frame() {
        assert_eq!(
            body_after(&["event: block\ndata: {"]).await,
            ["event: block\ndata: {"]
        );
    }
}
