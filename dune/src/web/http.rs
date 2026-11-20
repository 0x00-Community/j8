//! The wire under `fetch`: one HTTP/1.1 exchange over a fresh TCP connection. The prelude writes the request head
//! (with `connection: close`) and parses the raw response, so the Rust side only moves bytes.
use std::io::{Read, Write};
use std::net::TcpStream;

use j8::Bytes;

use crate::host::{Io, Offload};

pub struct Http;

#[j8::object("http")]
impl Http {
    /// Sends `head` and `body` to `host:port` and reads the response until the server closes
    async fn exchange(host: String, port: u32, head: String, body: Bytes) -> Io<Bytes> {
        Offload::new(move || {
            let addr = format!("{host}:{port}");
            let run = || -> std::io::Result<Vec<u8>> {
                let port = u16::try_from(port).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
                let mut stream = TcpStream::connect((host.as_str(), port))?;
                stream.write_all(head.as_bytes())?;
                stream.write_all(&body.0)?;
                let mut out = Vec::new();
                stream.read_to_end(&mut out)?;
                Ok(out)
            };
            Io::new(run().map(Bytes), "connect", &addr)
        })
        .await
    }
}
