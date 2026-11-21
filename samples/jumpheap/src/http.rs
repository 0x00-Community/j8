use std::io::{Read, Write};
use std::net::TcpStream;

use j8::frame::{Agent, Error, ErrorKind, Value};
use j8::IntoJs;
use j8::lockfree::Oneshot;

pub struct Reply(Result<(u32, String), String>);

impl<'gc> IntoJs<'gc> for Reply {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self.0 {
            Ok((status, body)) => {
                let (status, body) = (status.into_js(a)?, body.into_js(a)?);
                vec![status, body].into_js(a)
            }
            Err(e) => Err(a.throw(ErrorKind::Type, &format!("fetch failed: {e}"))),
        }
    }
}

impl Reply {
    fn send(method: &str, url: &str, body: &str) -> Result<(u32, String), String> {
        let rest = url.strip_prefix("http://").ok_or("only http:// URLs")?;
        let (host, path) = rest.split_once('/').map_or((rest, "/".to_owned()), |(h, p)| (h, format!("/{p}")));
        let mut s = TcpStream::connect(host).map_err(|e| format!("{host}: {e}"))?;
        let _ = s.set_nodelay(true);
        write!(s, "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        s.read_to_end(&mut out).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out);
        let (head, body) = text.split_once("\r\n\r\n").ok_or("no HTTP response")?;
        let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or("no status")?;
        Ok((status, body.to_owned()))
    }
}

pub struct Http;

#[j8::object("http")]
impl Http {
    async fn request(method: String, url: String, body: String) -> Reply {
        let (tx, rx) = Oneshot::new();
        std::thread::spawn(move || tx.send(Reply(Reply::send(&method, &url, &body))));
        rx.await
    }
}
