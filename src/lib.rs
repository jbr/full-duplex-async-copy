#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_debug_implementations,
    missing_copy_implementations,
    nonstandard_style,
    unused_qualifications,
    missing_docs,
    rustdoc::missing_crate_level_docs
)]
#![warn(clippy::pedantic)]

//! Bidirectionally copy between two `AsyncRead + AsyncWrite` transports.
//!
//! [`full_duplex_copy`] relays bytes in both directions concurrently until *both* directions
//! reach EOF, then resolves to the number of bytes copied in each direction. It supports
//! **half-closed** connections: when one direction reaches EOF, that read half stops and the
//! peer's write half is shut down ([`poll_close`][AsyncWrite::poll_close]), forwarding the close
//! as a FIN, while the opposite direction keeps relaying until it too reaches EOF.
//!
//! Built for the [`futures_lite`] / `futures-io` [`AsyncRead`] + [`AsyncWrite`] traits.
//!
//! ```
//! # use futures_lite::io::{AsyncRead, AsyncWrite, Cursor};
//! # use std::{io, pin::Pin, task::{Context, Poll}};
//! # // A minimal duplex transport: reads from `rx`, discards anything written to it. Real
//! # // transports (a TCP socket, say) have independent read and write halves just like this.
//! # struct Duplex { rx: Cursor<Vec<u8>> }
//! # impl Duplex { fn new(bytes: &[u8]) -> Self { Self { rx: Cursor::new(bytes.to_vec()) } } }
//! # impl AsyncRead for Duplex {
//! #     fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
//! #         Pin::new(&mut self.get_mut().rx).poll_read(cx, buf)
//! #     }
//! # }
//! # impl AsyncWrite for Duplex {
//! #     fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> { Poll::Ready(Ok(buf.len())) }
//! #     fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
//! #     fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }
//! # }
//! use full_duplex_async_copy::full_duplex_copy;
//!
//! # futures_lite::future::block_on(async {
//! // `a` and `b` are your two transports — e.g. two TCP connections to proxy between.
//! # let a = Duplex::new(b"hello");
//! # let b = Duplex::new(b"world!");
//! let (a_to_b, b_to_a) = full_duplex_copy(a, b).await?;
//! assert_eq!((a_to_b, b_to_a), (5, 6));
//! # std::io::Result::Ok(())
//! # }).unwrap();
//! ```

use futures_lite::{
    io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader},
    ready,
};
use pin_project_lite::pin_project;
use std::{
    future::Future,
    io::{ErrorKind, Result},
    pin::Pin,
    task::{
        Context,
        Poll::{self, Pending, Ready},
    },
};

/// Bidirectionally copy between two `AsyncRead + AsyncWrite` transports until both reach EOF.
///
/// Bytes are relayed in both directions concurrently. When one direction reaches EOF it stops and
/// the peer's write half is shut down ([`poll_close`][AsyncWrite::poll_close]), forwarding the
/// close as a FIN, while the opposite direction keeps relaying. The returned future resolves once
/// *both* directions have reached EOF, yielding `(bytes copied a → b, bytes copied b → a)`.
///
/// # Errors
///
/// Resolves to any I/O error produced by a read or write on either transport, as soon as it
/// occurs. Some in-flight data may be lost when that happens. A write that accepts zero bytes is
/// reported as [`ErrorKind::WriteZero`].
pub async fn full_duplex_copy<A, B>(a: A, b: B) -> Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite,
    B: AsyncRead + AsyncWrite,
{
    FullDuplexCopy {
        a: FullDuplexInner::new(a),
        b: FullDuplexInner::new(b),
        a_first: true,
    }
    .await
}

pin_project! {
    struct FullDuplexCopy<A, B> {
        #[pin]
        a: FullDuplexInner<A>,
        #[pin]
        b: FullDuplexInner<B>,
        // Which direction to poll first, flipped every loop iteration. Both directions are polled
        // every iteration regardless, so this only breaks ties (e.g. which error surfaces when
        // both fail at once) — deterministic alternation is enough; no RNG needed.
        a_first: bool,
    }
}

pin_project! {
    struct FullDuplexInner<R> {
        #[pin]
        inner: BufReader<R>,
        done: bool,
        bytes_read: u64,
    }
}

impl<R> FullDuplexInner<R>
where
    R: AsyncRead,
{
    fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            done: false,
            bytes_read: 0,
        }
    }

    /// Reads from this transport and writes into `target` (the peer).
    ///
    /// Returns `Ready(false)` after making forward progress (bytes copied), `Ready(true)` once
    /// this read half has reached EOF and the peer's write half has been shut down, and `Pending`
    /// when neither could proceed.
    fn poll_copy<Target>(
        self: Pin<&mut Self>,
        mut target: Pin<&mut Target>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<bool>>
    where
        Target: AsyncWrite,
    {
        let mut this = self.project();
        if *this.done {
            return Ready(Ok(true));
        }
        let buffer = ready!(this.inner.as_mut().poll_fill_buf(cx))?;

        if buffer.is_empty() {
            // This read half reached EOF. Flush whatever we've already written to the peer, then
            // shut its write half down, forwarding the close as a FIN. The opposite direction
            // keeps relaying until it too reaches EOF.
            ready!(target.as_mut().poll_flush(cx))?;
            ready!(target.as_mut().poll_close(cx))?;
            *this.done = true;
            Ready(Ok(true))
        } else {
            let i = ready!(target.as_mut().poll_write(cx, buffer))?;
            if i == 0 {
                return Ready(Err(ErrorKind::WriteZero.into()));
            }
            this.inner.as_mut().consume(i);
            *this.bytes_read += i as u64;
            Ready(Ok(false))
        }
    }
}

impl<R: AsyncWrite> AsyncWrite for FullDuplexInner<R> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.project().inner.poll_close(cx)
    }
}

impl<A, B> Future for FullDuplexCopy<A, B>
where
    A: AsyncRead + AsyncWrite,
    B: AsyncRead + AsyncWrite,
{
    type Output = Result<(u64, u64)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            // `?` on `Poll<Result<bool>>` yields `Poll<bool>`, short-circuiting the whole `poll`
            // with `Ready(Err(_))` on error. Both directions are polled every iteration; the order
            // is alternated only to avoid a systematic first-poll bias.
            let (a, b) = if *this.a_first {
                let a = this.a.as_mut().poll_copy(this.b.as_mut(), cx)?;
                let b = this.b.as_mut().poll_copy(this.a.as_mut(), cx)?;
                (a, b)
            } else {
                let b = this.b.as_mut().poll_copy(this.a.as_mut(), cx)?;
                let a = this.a.as_mut().poll_copy(this.b.as_mut(), cx)?;
                (a, b)
            };
            *this.a_first = !*this.a_first;

            match (a, b) {
                // Both read halves have reached EOF: the copy is complete.
                (Ready(true), Ready(true)) => {
                    return Ready(Ok((this.a.bytes_read, this.b.bytes_read)));
                }

                // At least one direction made progress this iteration; loop to keep draining both.
                (Ready(false), _) | (_, Ready(false)) => {}

                // No progress and not yet both done: at least one direction is Pending and has
                // registered a waker (the other may already be done, which returns `Ready(true)`
                // without registering one). Yield rather than loop, or we would busy-spin on the
                // done side.
                _ => return Pending,
            }
        }
    }
}
