use futures_lite::io::{AsyncBufRead, AsyncRead, AsyncWrite};
use futures_lite::ready;
use pin_project_lite::pin_project;
use std::io::{ErrorKind, Result};
use std::pin::Pin;
use std::task::{Context, Poll};

mod bufreader;
pub use bufreader::BufReader; // until https://github.com/smol-rs/futures-lite/pull/41 is published

pub async fn full_duplex_copy<A, B>(a: A, b: B) -> Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Send + Sync,
    B: AsyncRead + AsyncWrite + Send + Sync,
{
    FullDuplexCopy {
        a: FullDuplexInner::new(a),
        b: FullDuplexInner::new(b),
    }
    .await
}

pin_project! {
    struct FullDuplexCopy<A, B> {
        #[pin]
        a: FullDuplexInner<A>,
        #[pin]
        b: FullDuplexInner<B>,
    }
}

impl<R> FullDuplexInner<R>
where
    R: AsyncRead + AsyncWrite + Send + Sync,
{
    fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            done: false,
            bytes_read: 0,
        }
    }

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
            return Poll::Ready(Ok(true));
        }
        let buffer = ready!(this.inner.as_mut().poll_fill_buf(cx))?;

        if buffer.is_empty() {
            ready!(target.as_mut().poll_flush(cx))?;
            ready!(target.as_mut().poll_close(cx))?;
            *this.done = true;
            Poll::Ready(Ok(true))
        } else {
            let i = ready!(target.as_mut().poll_write(cx, buffer))?;
            if i == 0 {
                return Poll::Ready(Err(ErrorKind::WriteZero.into()));
            }
            this.inner.as_mut().consume(i);
            *this.bytes_read += i as u64;
            Poll::Ready(Ok(false))
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

pin_project! {
    struct FullDuplexInner<R> {
        #[pin]
        inner: BufReader<R>,
        done: bool,
        bytes_read: u64,
    }
}

use Poll::*;
impl<A, B> std::future::Future for FullDuplexCopy<A, B>
where
    A: AsyncRead + AsyncWrite + Send + Sync,
    B: AsyncRead + AsyncWrite + Send + Sync,
{
    type Output = Result<(u64, u64)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            let (a, b) = if fastrand::bool() {
                let a = this.a.as_mut().poll_copy(this.b.as_mut(), cx)?;
                let b = this.b.as_mut().poll_copy(this.a.as_mut(), cx)?;
                (a, b)
            } else {
                let b = this.b.as_mut().poll_copy(this.a.as_mut(), cx)?;
                let a = this.a.as_mut().poll_copy(this.b.as_mut(), cx)?;
                (a, b)
            };

            match (a, b) {
                (Ready(true), Ready(true)) => {
                    return Ready(Ok((this.a.bytes_read, this.b.bytes_read)));
                }

                (Pending, Pending) => return Pending,

                _ => {}
            }
        }
    }
}
