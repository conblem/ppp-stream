extern crate core;

use futures_util::{ready, FutureExt};
use ppp::v2::{Addresses, Header, ParseError};
use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::io::poll_read_buf;

trait Ext: Sized {
    fn remote_addr(self: Pin<&mut Self>) -> ProxyProtoFuture<'_, Self>;
    fn remote_addr_unpin(&mut self) -> ProxyProtoFuture<'_, Self> where Self: Unpin;
}

impl<T> Ext for T
where
    T: AsyncRead + Sized,
{
    fn remote_addr(self: Pin<&mut Self>) -> ProxyProtoFuture<'_, Self> {
        ProxyProtoFuture {
            inner: Some(self),
            buf: vec![],
        }
    }

    fn remote_addr_unpin(&mut self) -> ProxyProtoFuture<'_, Self> where Self: Unpin {
        Pin::new(self).remote_addr()
    }
}

#[derive(Debug)]
struct ProxyProtoFuture<'a, T> {
    inner: Option<Pin<&'a mut T>>,
    buf: Vec<u8>,
}

impl<'a, T> Unpin for ProxyProtoFuture<'a, T> {}

impl<'a, T> Future for ProxyProtoFuture<'a, T>
where
    T: AsyncRead,
{
    type Output = Result<ProxyProtoStream<'a, T>, IoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let buf = &mut this.buf;
        let inner = match &mut this.inner {
            Some(inner) => inner.as_mut(),
            None => panic!("future polled after completion")
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
        let start_of_data= res.len();

        let buf = std::mem::take(buf);
        let inner = this.inner.take().unwrap();

        let stream = ProxyProtoStream {
            addr,
            inner,
            buf,
            start_of_data,
        };

        return Poll::Ready(Ok(stream))
    }
}

#[derive(Debug)]
struct ProxyProtoStream<'a, T> {
    inner: Pin<&'a mut T>,
    buf: Vec<u8>,
    start_of_data: usize,
    pub addr: Addresses,
}

impl <'a, T> ProxyProtoStream<'a, T> {
    fn inner(&mut self) -> Pin<&mut T> {
        return self.inner.as_mut()
    }
}

impl <'a, T> Unpin for ProxyProtoStream<'a, T> {}

impl <'a, T> AsyncRead for ProxyProtoStream<'a, T> where T: AsyncRead {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let start_of_data = this.start_of_data;

        if this.buf.len() > 0 {
            buf.put_slice(&this.buf[start_of_data..]);
            this.buf = Vec::new();
        }

        return this.inner.as_mut().poll_read(cx, buf)
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use ppp::v2::{ParseError, PROTOCOL_PREFIX};
    use tokio::io::AsyncReadExt;

    use super::Ext;

    #[tokio::test]
    async fn test() {
        let mut buf = Vec::from(PROTOCOL_PREFIX);

        let err = (&mut &*buf).remote_addr_unpin().await.unwrap_err();
        let err = err.into_inner().unwrap().downcast::<ParseError>().unwrap();
        assert!(matches!(*err, ParseError::Incomplete(12)));

        buf.extend([
            0x21, 0x12, 0, 16, 127, 0, 0, 1, 192, 168, 1, 1, 0, 80, 1, 187, 4, 0, 1, 42
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
