//! Client Requests
use std::marker::PhantomData;
use std::io::{self, Write, BufWriter};

use url::Url;

use method::{self, Method};
use header::Headers;
use header::{self, Host};
use net::{NetworkStream, NetworkConnector, HttpConnector, Fresh, Streaming};
use http::{HttpWriter, LINE_ENDING};
use http::HttpWriter::{ThroughWriter, ChunkedWriter, SizedWriter, EmptyWriter};
use version;
use client::{Response, get_host_and_port};


/// A client request to a remote server.
/// The W type tracks the state of the request, Fresh vs Streaming.
pub struct Request<W> {
    /// The target URI for this request.
    pub url: Url,

    /// The HTTP version of this request.
    pub version: version::HttpVersion,

    body: HttpWriter<BufWriter<Box<NetworkStream + Send>>>,
    headers: Headers,
    method: method::Method,

    _marker: PhantomData<W>,
}

impl<W> Request<W> {
    /// Read the Request headers.
    #[inline]
    pub fn headers(&self) -> &Headers { &self.headers }

    /// Read the Request method.
    #[inline]
    pub fn method(&self) -> method::Method { self.method.clone() }
}

impl Request<Fresh> {
    /// Create a new client request.
    pub fn new(method: method::Method, url: Url) -> ::Result<Request<Fresh>> {
        let mut conn = HttpConnector(None);
        Request::with_connector(method, url, &mut conn)
    }

    /// Create a new client request with a specific underlying NetworkStream.
    pub fn with_connector<C, S>(method: method::Method, url: Url, connector: &C)
        -> ::Result<Request<Fresh>> where
        C: NetworkConnector<Stream=S>,
        S: Into<Box<NetworkStream + Send>> {
        let (host, port) = try!(get_host_and_port(&url));

        let stream = try!(connector.connect(&*host, port, &*url.scheme)).into();
        let stream = ThroughWriter(BufWriter::new(stream));

        let mut headers = Headers::new();
        headers.set(Host {
            hostname: host,
            port: Some(port),
        });

        Ok(Request {
            method: method,
            headers: headers,
            url: url,
            version: version::HttpVersion::Http11,
            body: stream,
            _marker: PhantomData,
        })
    }

    /// Consume a Fresh Request, writing the headers and method,
    /// returning a Streaming Request.
    pub fn start(mut self) -> ::Result<Request<Streaming>> {
        let mut uri = self.url.serialize_path().unwrap();
        if let Some(ref q) = self.url.query {
            uri.push('?');
            uri.push_str(&q[..]);
        }

        debug!("request line: {:?} {:?} {:?}", self.method, uri, self.version);
        try!(write!(&mut self.body, "{} {} {}{}",
                    self.method, uri, self.version, LINE_ENDING));


        let stream = match self.method {
            Method::Get | Method::Head => {
                debug!("headers={:?}", self.headers);
                try!(write!(&mut self.body, "{}{}", self.headers, LINE_ENDING));
                EmptyWriter(self.body.into_inner())
            },
            _ => {
                let mut chunked = true;
                let mut len = 0;

                match self.headers.get::<header::ContentLength>() {
                    Some(cl) => {
                        chunked = false;
                        len = **cl;
                    },
                    None => ()
                };

                // can't do in match above, thanks borrowck
                if chunked {
                    let encodings = match self.headers.get_mut::<header::TransferEncoding>() {
                        Some(&mut header::TransferEncoding(ref mut encodings)) => {
                            //TODO: check if chunked is already in encodings. use HashSet?
                            encodings.push(header::Encoding::Chunked);
                            false
                        },
                        None => true
                    };

                    if encodings {
                        self.headers.set::<header::TransferEncoding>(
                            header::TransferEncoding(vec![header::Encoding::Chunked]))
                    }
                }

                debug!("headers={:?}", self.headers);
                try!(write!(&mut self.body, "{}{}", self.headers, LINE_ENDING));

                if chunked {
                    ChunkedWriter(self.body.into_inner())
                } else {
                    SizedWriter(self.body.into_inner(), len)
                }
            }
        };

        Ok(Request {
            method: self.method,
            headers: self.headers,
            url: self.url,
            version: self.version,
            body: stream,
            _marker: PhantomData,
        })
    }

    /// Get a mutable reference to the Request headers.
    #[inline]
    pub fn headers_mut(&mut self) -> &mut Headers { &mut self.headers }
}

impl Request<Streaming> {
    /// Completes writing the request, and returns a response to read from.
    ///
    /// Consumes the Request.
    pub fn send(self) -> ::Result<Response> {
        let raw = try!(self.body.end()).into_inner().unwrap(); // end() already flushes
        Response::new(raw)
    }
}

impl Write for Request<Streaming> {
    #[inline]
    fn write(&mut self, msg: &[u8]) -> io::Result<usize> {
        self.body.write(msg)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.body.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::str::from_utf8;
    use url::Url;
    use method::Method::{Get, Head, Post};
    use mock::{MockStream, MockConnector};
    use net::Fresh;
    use header::{ContentLength,TransferEncoding,Encoding};
    use url::form_urlencoded;
    use super::Request;

    fn run_request(req: Request<Fresh>) -> Vec<u8> {
        let req = req.start().unwrap();
        let stream = *req.body.end().unwrap()
            .into_inner().unwrap().downcast::<MockStream>().ok().unwrap();
        stream.write
    }

    fn assert_no_body(s: &str) {
        assert!(!s.contains("Content-Length:"));
        assert!(!s.contains("Transfer-Encoding:"));
    }

    #[test]
    fn test_get_empty_body() {
        let req = Request::with_connector(
            Get, Url::parse("http://example.dom").unwrap(), &mut MockConnector
        ).unwrap();
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert_no_body(s);
    }

    #[test]
    fn test_head_empty_body() {
        let req = Request::with_connector(
            Head, Url::parse("http://example.dom").unwrap(), &mut MockConnector
        ).unwrap();
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert_no_body(s);
    }

    #[test]
    fn test_url_query() {
        let url = Url::parse("http://example.dom?q=value").unwrap();
        let req = Request::with_connector(
            Get, url, &mut MockConnector
        ).unwrap();
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert!(s.contains("?q=value"));
    }

    #[test]
    fn test_post_content_length() {
        let url = Url::parse("http://example.dom").unwrap();
        let mut req = Request::with_connector(
            Post, url, &mut MockConnector
        ).unwrap();
        let body = form_urlencoded::serialize(vec!(("q","value")).into_iter());
        req.headers_mut().set(ContentLength(body.len() as u64));
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert!(s.contains("Content-Length:"));
    }

    #[test]
    fn test_post_chunked() {
        let url = Url::parse("http://example.dom").unwrap();
        let req = Request::with_connector(
            Post, url, &mut MockConnector
        ).unwrap();
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert!(!s.contains("Content-Length:"));
    }

    #[test]
    fn test_post_chunked_with_encoding() {
        let url = Url::parse("http://example.dom").unwrap();
        let mut req = Request::with_connector(
            Post, url, &mut MockConnector
        ).unwrap();
        req.headers_mut().set(TransferEncoding(vec![Encoding::Chunked]));
        let bytes = run_request(req);
        let s = from_utf8(&bytes[..]).unwrap();
        assert!(!s.contains("Content-Length:"));
        assert!(s.contains("Transfer-Encoding:"));
    }
}
