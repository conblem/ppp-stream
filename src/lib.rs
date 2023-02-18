use futures_util::{ready, FutureExt};
use ppp::v2::{Addresses, Header, ParseError};
use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf, AsyncWrite};
use tokio_util::io::poll_read_buf;

pub trait Ext {
    fn remote_addr_owned(self) -> PPPFuture<Self> where Self: Sized;
    fn remote_addr(self: Pin<&mut Self>) -> PPPRefFuture<'_, Self>;
    fn remote_addr_unpin(&mut self) -> PPPRefFuture<'_, Self>
    where
        Self: Unpin;
}

impl<T> Ext for T
where
    T: AsyncRead,
{
    fn remote_addr_owned(self) -> PPPFuture<Self> where Self: Sized {
        PPPFuture {
            inner: Some(self),
            buf: vec![]
        }
    }

    fn remote_addr(self: Pin<&mut Self>) -> PPPRefFuture<'_, Self> {
        PPPRefFuture {
            inner: Some(self),
            buf: vec![],
        }
    }

    fn remote_addr_unpin(&mut self) -> PPPRefFuture<'_, Self>
    where
        Self: Unpin,
    {
        Pin::new(self).remote_addr()
    }
}

pub struct PPPFuture<T> {
    inner: Option<T>,
    buf: Vec<u8>,
}

impl <T: Unpin> Unpin for PPPFuture<T> {}

impl<T> Future for PPPFuture<T>
where
    T: AsyncRead + Unpin,
{
    type Output = Result<PPPStream<T>, IoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let inner = match &mut this.inner {
            None => panic!("Future polled after completion"),
            Some(inner) => inner,
        };
        let buf = std::mem::take(&mut this.buf);

        let mut fut = PPPRefFuture {
            inner: Some(Pin::new(inner)),
            buf,
        };
        let res = fut.poll_unpin(cx);

        this.buf = fut.buf;

        let PPPRefStream {
            start_of_data,
            addr,
            data,
            ..
        } = ready!(res)?;

        return Poll::Ready(Ok(PPPStream {
            inner: this.inner.take().unwrap(),
            start_of_data,
            data,
            addr,
        }));
    }
}

impl <'a, T> AsyncWrite for PPPRefStream<'a, T> where T: AsyncWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, IoError>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), IoError>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), IoError>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_shutdown(cx)
    }
}

pub struct PPPStream<T> {
    inner: T,
    data: Vec<u8>,
    start_of_data: usize,
    pub addr: Addresses,
}

impl <T> Unpin for PPPStream<T> {}

impl <T> AsyncRead for PPPStream<T> where T: AsyncRead + Unpin {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let data = std::mem::take(&mut this.data);

        let mut stream = PPPRefStream {
            inner: Pin::new(&mut this.inner),
            addr: Addresses::Unspecified,
            data,
            start_of_data: this.start_of_data
        };

        let res = Pin::new(&mut stream).poll_read(cx, buf);
        this.data = stream.data;

        return res;
    }
}

#[derive(Debug)]
pub struct PPPRefFuture<'a, T: ?Sized> {
    inner: Option<Pin<&'a mut T>>,
    buf: Vec<u8>,
}

impl<'a, T> Unpin for PPPRefFuture<'a, T> {}

impl<'a, T> Future for PPPRefFuture<'a, T>
where
    T: AsyncRead,
{
    type Output = Result<PPPRefStream<'a, T>, IoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let buf = &mut this.buf;
        let inner = match &mut this.inner {
            Some(inner) => inner.as_mut(),
            None => panic!("future polled after completion"),
        };

        let added = ready!(poll_read_buf(inner, cx, buf))?;
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

        let stream = PPPRefStream {
            addr,
            inner,
            data,
            start_of_data,
        };

        return Poll::Ready(Ok(stream));
    }
}

#[derive(Debug)]
pub struct PPPRefStream<'a, T> {
    inner: Pin<&'a mut T>,
    data: Vec<u8>,
    start_of_data: usize,
    pub addr: Addresses,
}

impl<'a, T> PPPRefStream<'a, T> {
    pub fn inner(&mut self) -> Pin<&mut T> {
        return self.inner.as_mut();
    }
}

impl<'a, T> Unpin for PPPRefStream<'a, T> {}

impl<'a, T> AsyncRead for PPPRefStream<'a, T>
where
    T: AsyncRead,
{
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

            return Poll::Ready(std::io::Result::Ok(()));
        } else if this.data.len() > 0 {
            this.data = Vec::new()
        }

        this.inner.as_mut().poll_read(cx, buf)
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
            0x21, 0x12, 0, 16, 127, 0, 0, 1, 192, 168, 1, 1, 0, 80, 1, 187,  4, 0, 1, 42,
            10, 20, 30, 40, 50, 60
        ]);

        let mut stream = buf.as_slice();
        let mut addr = (&mut stream).remote_addr_unpin().await.unwrap();

        let res = addr.read_u8().await.unwrap();
        assert_eq!(10, res);

        let mut res = vec![0;4];
        addr.read_exact(&mut res).await.unwrap();

        let expected = vec![20, 30, 40, 50];
        assert_eq!(expected, res);

        let res = addr.read_u8().await.unwrap();
        assert_eq!(60, res);
    }

    #[tokio::test]
    async fn test() {
        let mut buf = Vec::from(PROTOCOL_PREFIX);

        let err = (&mut &*buf).remote_addr_unpin().await.unwrap_err();
        let err = err.into_inner().unwrap().downcast::<ParseError>().unwrap();
        assert!(matches!(*err, ParseError::Incomplete(12)));

        buf.extend([
            0x21, 0x12, 0, 16, 127, 0, 0, 1, 192, 168, 1, 1, 0, 80, 1, 187, 4, 0, 1, 42,
        ]);
        let mut stream = &*buf;
        let mut addr = (&mut stream).remote_addr_unpin().await.unwrap();
        let err = addr.read_u8().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

        buf.push(10);
        let mut stream = &*buf;
        let mut addr = (&mut stream).remote_addr_unpin().await.unwrap();
        let res = addr.read_u8().await.unwrap();
        assert_eq!(10, res);

        // test access to inner
        let err = addr.inner().read_u8().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);

        assert!(!addr.addr.is_empty());
    }
}
