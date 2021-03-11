use std::io::{IoSliceMut, Result, SeekFrom};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_lite::io::{AsyncBufRead, AsyncRead, AsyncSeek, AsyncWrite};
use futures_lite::ready;
use pin_project_lite::pin_project;
use std::{cmp, fmt};
const DEFAULT_BUF_SIZE: usize = 8 * 1024;

pin_project! {
    /// Adds buffering to a reader.
    ///
    /// It can be excessively inefficient to work directly with an [`AsyncRead`] instance. A
    /// [`BufReader`] performs large, infrequent reads on the underlying [`AsyncRead`] and
    /// maintains an in-memory buffer of the incoming byte stream.
    ///
    /// [`BufReader`] can improve the speed of programs that make *small* and *repeated* reads to
    /// the same file or networking socket. It does not help when reading very large amounts at
    /// once, or reading just once or a few times. It also provides no advantage when reading from
    /// a source that is already in memory, like a `Vec<u8>`.
    ///
    /// When a [`BufReader`] is dropped, the contents of its buffer are discarded. Creating
    /// multiple instances of [`BufReader`] on the same reader can cause data loss.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::{AsyncBufReadExt, BufReader};
    ///
    /// # spin_on::spin_on(async {
    /// let input: &[u8] = b"hello";
    /// let mut reader = BufReader::new(input);
    ///
    /// let mut line = String::new();
    /// reader.read_line(&mut line).await?;
    /// # std::io::Result::Ok(()) });
    /// ```
    pub struct BufReader<R> {
        #[pin]
        inner: R,
        buf: Box<[u8]>,
        pos: usize,
        cap: usize,
    }
}

impl<R: AsyncRead> BufReader<R> {
    /// Creates a buffered reader with the default buffer capacity.
    ///
    /// The default capacity is currently 8 KB, but that may change in the future.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let reader = BufReader::new(input);
    /// ```
    pub fn new(inner: R) -> BufReader<R> {
        BufReader::with_capacity(DEFAULT_BUF_SIZE, inner)
    }

    /// Creates a buffered reader with the specified capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let reader = BufReader::with_capacity(1024, input);
    /// ```
    pub fn with_capacity(capacity: usize, inner: R) -> BufReader<R> {
        BufReader {
            inner,
            buf: vec![0; capacity].into_boxed_slice(),
            pos: 0,
            cap: 0,
        }
    }
}

impl<R> BufReader<R> {
    /// Gets a reference to the underlying reader.
    ///
    /// It is not advisable to directly read from the underlying reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let reader = BufReader::new(input);
    ///
    /// let r = reader.get_ref();
    /// ```
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Gets a mutable reference to the underlying reader.
    ///
    /// It is not advisable to directly read from the underlying reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let mut reader = BufReader::new(input);
    ///
    /// let r = reader.get_mut();
    /// ```
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Gets a pinned mutable reference to the underlying reader.
    ///
    /// It is not advisable to directly read from the underlying reader.
    fn get_pin_mut(self: Pin<&mut Self>) -> Pin<&mut R> {
        self.project().inner
    }

    /// Returns a reference to the internal buffer.
    ///
    /// This method will not attempt to fill the buffer if it is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let reader = BufReader::new(input);
    ///
    /// // The internal buffer is empty until the first read request.
    /// assert_eq!(reader.buffer(), &[]);
    /// ```
    pub fn buffer(&self) -> &[u8] {
        &self.buf[self.pos..self.cap]
    }

    /// Unwraps the buffered reader, returning the underlying reader.
    ///
    /// Note that any leftover data in the internal buffer will be lost.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures_lite::io::BufReader;
    ///
    /// let input: &[u8] = b"hello";
    /// let reader = BufReader::new(input);
    ///
    /// assert_eq!(reader.into_inner(), input);
    /// ```
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Invalidates all data in the internal buffer.
    #[inline]
    fn discard_buffer(self: Pin<&mut Self>) {
        let this = self.project();
        *this.pos = 0;
        *this.cap = 0;
    }
}

impl<R: AsyncRead> AsyncRead for BufReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize>> {
        // If we don't have any buffered data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.pos == self.cap && buf.len() >= self.buf.len() {
            let res = ready!(self.as_mut().get_pin_mut().poll_read(cx, buf));
            self.discard_buffer();
            return Poll::Ready(res);
        }
        let mut rem = ready!(self.as_mut().poll_fill_buf(cx))?;
        let nread = std::io::Read::read(&mut rem, buf)?;
        self.consume(nread);
        Poll::Ready(Ok(nread))
    }

    fn poll_read_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Poll<Result<usize>> {
        let total_len = bufs.iter().map(|b| b.len()).sum::<usize>();
        if self.pos == self.cap && total_len >= self.buf.len() {
            let res = ready!(self.as_mut().get_pin_mut().poll_read_vectored(cx, bufs));
            self.discard_buffer();
            return Poll::Ready(res);
        }
        let mut rem = ready!(self.as_mut().poll_fill_buf(cx))?;
        let nread = std::io::Read::read_vectored(&mut rem, bufs)?;
        self.consume(nread);
        Poll::Ready(Ok(nread))
    }
}

impl<R: AsyncRead> AsyncBufRead for BufReader<R> {
    fn poll_fill_buf<'a>(self: Pin<&'a mut Self>, cx: &mut Context<'_>) -> Poll<Result<&'a [u8]>> {
        let mut this = self.project();

        // If we've reached the end of our internal buffer then we need to fetch
        // some more data from the underlying reader.
        // Branch using `>=` instead of the more correct `==`
        // to tell the compiler that the pos..cap slice is always valid.
        if *this.pos >= *this.cap {
            debug_assert!(*this.pos == *this.cap);
            *this.cap = ready!(this.inner.as_mut().poll_read(cx, this.buf))?;
            *this.pos = 0;
        }
        Poll::Ready(Ok(&this.buf[*this.pos..*this.cap]))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let this = self.project();
        *this.pos = cmp::min(*this.pos + amt, *this.cap);
    }
}

impl<R: AsyncRead + fmt::Debug> fmt::Debug for BufReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufReader")
            .field("reader", &self.inner)
            .field(
                "buffer",
                &format_args!("{}/{}", self.cap - self.pos, self.buf.len()),
            )
            .finish()
    }
}

impl<R: AsyncSeek> AsyncSeek for BufReader<R> {
    /// Seeks to an offset, in bytes, in the underlying reader.
    ///
    /// The position used for seeking with [`SeekFrom::Current`] is the position the underlying
    /// reader would be at if the [`BufReader`] had no internal buffer.
    ///
    /// Seeking always discards the internal buffer, even if the seek position would otherwise fall
    /// within it. This guarantees that calling [`into_inner()`][`BufReader::into_inner()`]
    /// immediately after a seek yields the underlying reader at the same position.
    ///
    /// See [`AsyncSeek`] for more details.
    ///
    /// Note: In the edge case where you're seeking with `SeekFrom::Current(n)` where `n` minus the
    /// internal buffer length overflows an `i64`, two seeks will be performed instead of one. If
    /// the second seek returns `Err`, the underlying reader will be left at the same position it
    /// would have if you called [`seek()`][`AsyncSeekExt::seek()`] with `SeekFrom::Current(0)`.
    fn poll_seek(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        pos: SeekFrom,
    ) -> Poll<Result<u64>> {
        let result: u64;
        if let SeekFrom::Current(n) = pos {
            let remainder = (self.cap - self.pos) as i64;
            // it should be safe to assume that remainder fits within an i64 as the alternative
            // means we managed to allocate 8 exbibytes and that's absurd.
            // But it's not out of the realm of possibility for some weird underlying reader to
            // support seeking by i64::min_value() so we need to handle underflow when subtracting
            // remainder.
            if let Some(offset) = n.checked_sub(remainder) {
                result = ready!(self
                    .as_mut()
                    .get_pin_mut()
                    .poll_seek(cx, SeekFrom::Current(offset)))?;
            } else {
                // seek backwards by our remainder, and then by the offset
                ready!(self
                    .as_mut()
                    .get_pin_mut()
                    .poll_seek(cx, SeekFrom::Current(-remainder)))?;
                self.as_mut().discard_buffer();
                result = ready!(self
                    .as_mut()
                    .get_pin_mut()
                    .poll_seek(cx, SeekFrom::Current(n)))?;
            }
        } else {
            // Seeking with Start/End doesn't care about our buffer length.
            result = ready!(self.as_mut().get_pin_mut().poll_seek(cx, pos))?;
        }
        self.discard_buffer();
        Poll::Ready(Ok(result))
    }
}

impl<R: AsyncWrite> AsyncWrite for BufReader<R> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        self.as_mut().get_pin_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.as_mut().get_pin_mut().poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.as_mut().get_pin_mut().poll_close(cx)
    }
}
