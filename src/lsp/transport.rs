use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    ContentLength,
    JsonLines,
}

pub struct Connection {
    pub reader: Box<dyn BufRead + Send>,
    pub writer: Box<dyn Write + Send>,
    pub framing: Framing,
}

pub fn read_message(reader: &mut dyn BufRead, framing: Framing) -> io::Result<Option<Vec<u8>>> {
    match framing {
        Framing::JsonLines => loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(None);
            }
            while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                line.pop();
            }
            if !line.is_empty() {
                return Ok(Some(line));
            }
        },
        Framing::ContentLength => {
            let mut length = None;
            let mut header = String::new();
            loop {
                header.clear();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(None);
                }
                let line = header.trim_end();
                if line.is_empty() {
                    if length.is_some() {
                        break;
                    }
                    continue;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.trim().eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().ok();
                }
            }
            let mut body = vec![0; length.unwrap_or(0)];
            reader.read_exact(&mut body)?;
            Ok(Some(body))
        }
    }
}

pub fn write_message(writer: &mut dyn Write, framing: Framing, body: &[u8]) -> io::Result<()> {
    match framing {
        Framing::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
            writer.write_all(body)?;
        }
        Framing::JsonLines => {
            writer.write_all(body)?;
            writer.write_all(b"\n")?;
        }
    }
    writer.flush()
}

pub fn stdio() -> Connection {
    Connection { reader: Box::new(BufReader::with_capacity(1 << 16, io::stdin())), writer: Box::new(io::stdout()), framing: Framing::ContentLength }
}

fn stream_connection<S: Read + Write + Send + 'static>(stream: S, clone: impl FnOnce(&S) -> io::Result<S>, framing: Framing) -> io::Result<Connection> {
    let writer = clone(&stream)?;
    Ok(Connection { reader: Box::new(BufReader::with_capacity(1 << 16, stream)), writer: Box::new(writer), framing })
}

#[cfg(unix)]
pub fn node_ipc() -> io::Result<Connection> {
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;
    let fd: i32 = std::env::var("NODE_CHANNEL_FD")
        .map_err(|_| io::Error::other("--node-ipc needs NODE_CHANNEL_FD (spawn with stdio 'ipc')"))?
        .parse()
        .map_err(|_| io::Error::other("NODE_CHANNEL_FD is not a file descriptor"))?;
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream_connection(stream, UnixStream::try_clone, Framing::JsonLines)
}

#[cfg(not(unix))]
pub fn node_ipc() -> io::Result<Connection> {
    Err(io::Error::other("--node-ipc is only supported on Unix; use --stdio or --pipe"))
}

#[cfg(unix)]
pub fn pipe(name: &str) -> io::Result<Connection> {
    use std::os::unix::net::UnixStream;
    stream_connection(UnixStream::connect(name)?, UnixStream::try_clone, Framing::ContentLength)
}

#[cfg(windows)]
pub fn pipe(name: &str) -> io::Result<Connection> {
    let path = if name.starts_with(r"\\") { name.to_string() } else { format!(r"\\.\pipe\{name}") };
    let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    stream_connection(file, std::fs::File::try_clone, Framing::ContentLength)
}

pub fn socket(port: u16) -> io::Result<Connection> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_nodelay(true)?;
    stream_connection(stream, std::net::TcpStream::try_clone, Framing::ContentLength)
}

pub enum Listener {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener, std::path::PathBuf),
    Tcp(std::net::TcpListener),
}

impl Listener {
    pub fn bind(spec: &str) -> io::Result<Listener> {
        if let Some(port) = spec.strip_prefix("tcp:") {
            let port: u16 = port.parse().map_err(|_| io::Error::other("tcp:<port> expects a port number"))?;
            return Ok(Listener::Tcp(std::net::TcpListener::bind(("127.0.0.1", port))?));
        }
        #[cfg(unix)]
        {
            let path = std::path::PathBuf::from(spec);
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("a daemon is already listening on {spec}")));
            }
            let _ = std::fs::remove_file(&path);
            Ok(Listener::Unix(std::os::unix::net::UnixListener::bind(&path)?, path))
        }
        #[cfg(not(unix))]
        Err(io::Error::other("on Windows the daemon listens on tcp:<port>"))
    }

    pub fn accept(&self) -> io::Result<Connection> {
        match self {
            #[cfg(unix)]
            Listener::Unix(l, _) => stream_connection(l.accept()?.0, std::os::unix::net::UnixStream::try_clone, Framing::ContentLength),
            Listener::Tcp(l) => {
                let stream = l.accept()?.0;
                stream.set_nodelay(true)?;
                stream_connection(stream, std::net::TcpStream::try_clone, Framing::ContentLength)
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Listener::Unix(_, path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_round_trip() {
        let mut out = Vec::new();
        write_message(&mut out, Framing::ContentLength, br#"{"a":1}"#).unwrap();
        write_message(&mut out, Framing::ContentLength, "{\"b\":\"é\"}".as_bytes()).unwrap();
        let mut reader = io::Cursor::new(out);
        assert_eq!(read_message(&mut reader, Framing::ContentLength).unwrap().unwrap(), br#"{"a":1}"#);
        assert_eq!(read_message(&mut reader, Framing::ContentLength).unwrap().unwrap(), "{\"b\":\"é\"}".as_bytes().to_vec());
        assert!(read_message(&mut reader, Framing::ContentLength).unwrap().is_none());
    }

    #[test]
    fn json_lines_skip_blank_lines() {
        let mut reader = io::Cursor::new(b"{\"a\":1}\n\r\n{\"b\":2}\n".to_vec());
        assert_eq!(read_message(&mut reader, Framing::JsonLines).unwrap().unwrap(), br#"{"a":1}"#);
        assert_eq!(read_message(&mut reader, Framing::JsonLines).unwrap().unwrap(), br#"{"b":2}"#);
    }
}
