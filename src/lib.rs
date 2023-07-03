use futures_util::{ready, FutureExt};
use ppp::v2::{Addresses, Header, ParseError};
use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::io::poll_read_buf;

pub trait Ext: Sized + AsyncRead {
    fn remote_addr(self) -> PPPFuture<Self> {
        PPPFuture {
            inner: Some(self),
            buf: vec![],
        }
    }
}

impl<T: AsyncRead> Ext for T {}

#[derive(Debug)]
pub struct PPPFuture<T> {
    inner: Option<T>,
    buf: Vec<u8>,
}

impl<T: AsyncRead + Unpin> Future for PPPFuture<T> {
    type Output = Result<PPPStream<T>, IoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let buf = &mut this.buf;
        let inner = this.inner.as_mut().expect("future polled after completion");

        let added = ready!(poll_read_buf(Pin::new(inner), cx, buf))?;
        // stream is eof
        if added == 0 {
            return Poll::Ready(Err(IoError::new(
                ErrorKind::Other,
                ParseError::Incomplete(buf.len()),
            )));
        }
        let res = match Header::try_from(buf.as_ref()) {
            Err(ParseError::Incomplete(_)) => return this.poll_unpin(cx),
            Err(e) => return Poll::Ready(Err(IoError::new(ErrorKind::Other, e))),
            Ok(res) => res,
        };

        let addr = res.addresses;
        let start_of_data = res.len();

        let data = std::mem::take(buf);
        let inner = this.inner.take().unwrap();

        let stream = PPPStream {
            addr,
            inner,
            data,
            start_of_data,
        };

        return Poll::Ready(Ok(stream));
    }
}

#[derive(Debug)]
pub struct PPPStream<T> {
    inner: T,
    data: Vec<u8>,
    start_of_data: usize,
    pub addr: Addresses,
}

impl<T> PPPStream<T> {
    pub fn inner(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for PPPStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let start_of_data = this.start_of_data;

        if this.data.len() > 0 && start_of_data < this.data.len() {
            if buf.remaining() < this.data.len() - start_of_data {
                let end_len = start_of_data + buf.remaining();
                buf.put_slice(&this.data[start_of_data..end_len]);
                this.start_of_data = end_len;
            } else {
                buf.put_slice(&this.data[start_of_data..]);
                this.data = Vec::new();
            }

            return Poll::Ready(Ok(()));
        } else if this.data.len() > 0 {
            this.data = Vec::new()
        }

        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for PPPStream<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, IoError>> {
        Pin::new(self.inner()).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), IoError>> {
        Pin::new(self.inner()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), IoError>> {
        Pin::new(self.inner()).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use ppp::v2::{ParseError, PROTOCOL_PREFIX};
    use std::io::ErrorKind;
    use tokio::io::AsyncReadExt;

    use super::Ext;

    #[tokio::test]
    async fn test_small_buffer() {
        let mut buf = Vec::from(PROTOCOL_PREFIX);
        buf.extend([
            0x21, 0x12, 0, 16, 127, 0, 0, 1, 192, 168, 1, 1, 0, 80, 1, 187, 4, 0, 1, 42, 10, 20,
            30, 40, 50, 60,
        ]);

        let mut stream = buf.as_slice();
        let mut addr = (&mut stream).remote_addr().await.unwrap();

        let res = addr.read_u8().await.unwrap();
        assert_eq!(10, res);

        let mut res = vec![0; 4];
        addr.read_exact(&mut res).await.unwrap();

        let expected = vec![20, 30, 40, 50];
        assert_eq!(expected, res);

        let res = addr.read_u8().await.unwrap();
        assert_eq!(60, res);
    }

    #[tokio::test]
    async fn test() {
        let mut buf = Vec::from(PROTOCOL_PREFIX);

        let err = (&mut &*buf).remote_addr().await.unwrap_err();
        let err = err.into_inner().unwrap().downcast::<ParseError>().unwrap();
        assert!(matches!(*err, ParseError::Incomplete(12)));

        buf.extend([
            0x21, 0x12, 0, 16, 127, 0, 0, 1, 192, 168, 1, 1, 0, 80, 1, 187, 4, 0, 1, 42,
        ]);
        let mut stream = &*buf;
        let mut addr = (&mut stream).remote_addr().await.unwrap();
        let err = addr.read_u8().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

        buf.push(10);
        let mut stream = &*buf;
        let mut addr = (&mut stream).remote_addr().await.unwrap();
        let res = addr.read_u8().await.unwrap();
        assert_eq!(10, res);

        // test access to inner
        let err = addr.inner().read_u8().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

        assert!(!addr.addr.is_empty());
    }
}
