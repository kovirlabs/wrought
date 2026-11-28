//! One subscriber's event stream: the response body of `/_engine/live`.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use tokio::sync::mpsc;

use crate::hub::{HEARTBEAT, Hub, Refused, Sub};

/// The events of one subscription, as server-sent event bytes, with a
/// comment line when nothing happened for a while. Leaves the hub when
/// dropped (the client went away, the server stopped, it fell behind).
pub struct LiveStream {
    rx: mpsc::UnboundedReceiver<Bytes>,
    pub(crate) sub: Arc<Sub>,
    hub: Hub,
    beat: tokio::time::Interval,
}

impl LiveStream {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<Bytes>, sub: Arc<Sub>, hub: Hub) -> Self {
        let mut beat = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
        beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        LiveStream { rx, sub, hub, beat }
    }

    /// The streaming response: `text/event-stream`, never stored or
    /// buffered by a proxy.
    pub fn into_response(self) -> Response {
        (
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-store"),
                // nginx: pass each event on at once.
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            Body::from_stream(self),
        )
            .into_response()
    }
}

impl Stream for LiveStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(b)) => {
                this.sub.queued.fetch_sub(b.len(), Ordering::AcqRel);
                return Poll::Ready(Some(Ok(b)));
            }
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => {}
        }
        if this.beat.poll_tick(cx).is_ready() {
            return Poll::Ready(Some(Ok(Bytes::from_static(b":\n\n"))));
        }
        Poll::Pending
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.hub.unregister(&self.sub);
    }
}

/// The answer to a refused subscription: `503` with `Retry-After` (the
/// page's script waits and tries again).
pub fn refused(r: Refused) -> Response {
    let body = match r {
        Refused::Full => "Too many live connections right now.\n",
        Refused::Closing => "The server is restarting.\n",
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::RETRY_AFTER, "30"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}
