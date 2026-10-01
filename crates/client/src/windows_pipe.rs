//! Windows local transport. Tokio owns overlapped pipe I/O; the protocol's
//! blocking reader/writer threads wait on it without polling. Closing a client
//! cancels pending I/O even when the agent has no output to send.

use std::{
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    runtime::{Builder, Runtime},
    sync::watch,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES},
};

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Builder::new_multi_thread().worker_threads(2).enable_io().build().expect("pipe runtime"))
}

fn name(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let bytes: Vec<u8> = path.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect();
    format!(r"\\.\pipe\sik-{}", proto::build_id(&bytes))
}

fn server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    // Protected DACL: only the object's owner may connect, never other local
    // users. Remote pipe clients are rejected separately by ServerOptions.
    let sddl: Vec<u16> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: NUL-terminated SDDL and valid output storage; the descriptor lives
    // until create_with_security_attributes_raw returns and Windows copies it.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let _enter = runtime().enter();
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&mut attributes as *mut SECURITY_ATTRIBUTES).cast());
        LocalFree(descriptor);
        result
    }
}

pub struct Listener {
    name: String,
    next: Mutex<NamedPipeServer>,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let name = name(path);
        let next = Mutex::new(server(&name, true)?);
        Ok(Self { name, next })
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut next = self.next.lock().unwrap();
        if let Err(error) = runtime().block_on(next.connect()) {
            *next = server(&self.name, false)?;
            return Err(error);
        }
        // Keep a listening instance alive so clients never see a missing pipe.
        let connected = std::mem::replace(&mut *next, server(&self.name, false)?);
        Ok(Stream::new(Pipe::Server(connected)))
    }
}

enum Pipe {
    Client(NamedPipeClient),
    Server(NamedPipeServer),
}

struct Inner {
    pipe: Pipe,
    closed: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Stream(Arc<Inner>);

impl Stream {
    fn new(pipe: Pipe) -> Self {
        let (closed, _) = watch::channel(false);
        Self(Arc::new(Inner { pipe, closed }))
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        let _enter = runtime().enter();
        Ok(Self::new(Pipe::Client(ClientOptions::new().open(name(path))?)))
    }

    pub fn close(&self) {
        self.0.closed.send_replace(true);
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        let mut closed = self.0.closed.subscribe();
        runtime().block_on(async {
            loop {
                if *closed.borrow() { return Ok(0); }
                let ready = async {
                    match &self.0.pipe {
                        Pipe::Client(pipe) => pipe.readable().await,
                        Pipe::Server(pipe) => pipe.readable().await,
                    }
                };
                tokio::select! {
                    biased;
                    _ = closed.changed() => return Ok(0),
                    result = ready => result?,
                }
                let result = match &self.0.pipe {
                    Pipe::Client(pipe) => pipe.try_read(buf),
                    Pipe::Server(pipe) => pipe.try_read(buf),
                };
                match result {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                    result => return result,
                }
            }
        })
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        let mut closed = self.0.closed.subscribe();
        runtime().block_on(async {
            loop {
                if *closed.borrow() { return Err(io::ErrorKind::BrokenPipe.into()); }
                let ready = async {
                    match &self.0.pipe {
                        Pipe::Client(pipe) => pipe.writable().await,
                        Pipe::Server(pipe) => pipe.writable().await,
                    }
                };
                tokio::select! {
                    biased;
                    _ = closed.changed() => return Err(io::ErrorKind::BrokenPipe.into()),
                    result = ready => result?,
                }
                let result = match &self.0.pipe {
                    Pipe::Client(pipe) => pipe.try_write(buf),
                    Pipe::Server(pipe) => pipe.try_write(buf),
                };
                match result {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                    result => return result,
                }
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn duplex_and_close_wakes_idle_reader() {
        let path = std::env::temp_dir().join(format!("sik-pipe-test-{}", std::process::id()));
        let listener = Listener::bind(&path).unwrap();
        assert!(Listener::bind(&path).is_err(), "only one agent may listen");
        let mut client = Stream::connect(&path).unwrap();
        let mut peer = listener.accept().unwrap();
        client.write_all(b"request").unwrap();
        let mut buf = [0; 7];
        peer.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"request");
        peer.write_all(b"reply").unwrap();
        client.read_exact(&mut buf[..5]).unwrap();
        assert_eq!(&buf[..5], b"reply");
        let closer = client.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(client.read(&mut [0; 1])).unwrap());
        closer.close();
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap(), 0);
    }
}
