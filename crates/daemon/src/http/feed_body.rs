//! A live feed's response body.
//!
//! The feed task sends frames into a channel and this body hands them to the
//! connection. When the page goes away the connection drops the body, which
//! closes the channel, so the task ends and frees its place at once rather
//! than at its next keepalive.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use hyper::body::{Body, Frame};
use tokio::sync::mpsc;

/// The frames one feed task sends, ending when the task ends.
pub(super) struct FeedBody(pub(super) mpsc::Receiver<Bytes>);

impl Body for FeedBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        waker: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(waker)
            .map(|frame| frame.map(|bytes| Ok(Frame::data(bytes))))
    }
}
