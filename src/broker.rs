use anyhow::{bail, Context, Result};
use log::debug;
use std::collections::HashMap;
use std::net::SocketAddr;
#[allow(unused)]
use std::sync::{atomic::Ordering, Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_native_tls::TlsStream;

use crate::control::Control;
use crate::message::{ConnectionHeaderType, DataType, Message, ProxyHeaderType, ProxyType};
#[cfg(feature = "tty")]
use crate::tty::{Tty, UPDATE_WINSIZE};

type TlsReader = Arc<Mutex<Option<ReadHalf<TlsStream<TcpStream>>>>>;
type TlsWriter = Arc<Mutex<Option<WriteHalf<TlsStream<TcpStream>>>>>;
type TcpWriter = Arc<Mutex<Option<WriteHalf<TcpStream>>>>;
type ProxyConnections = Arc<Mutex<HashMap<u16, ProxyConnection>>>;

pub struct Broker {
    pub remote_address: SocketAddr,
    reader: TlsReader,
    writer: TlsWriter,
    proxy_address: Option<SocketAddr>,
    proxy_connections: ProxyConnections,
    #[cfg(feature = "tty")]
    tty: Option<Tty>,
}

pub struct ProxyConnection {
    writer: TcpWriter,
}

impl Broker {
    pub async fn new(control: &mut Control, remote_address: SocketAddr) -> Result<Self> {
        let stream = control.stream.lock().await.take().context("error")?;
        let (r, w) = tokio::io::split(stream);
        Ok(Self {
            remote_address,
            reader: Arc::new(Mutex::new(Some(r))),
            writer: Arc::new(Mutex::new(Some(w))),
            proxy_address: control.proxy_address,
            proxy_connections: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "tty")]
            tty: None,
        })
    }
    #[cfg(feature = "tty")]
    pub fn tty<'a>(&'a mut self) -> &'a mut Self {
        self.tty = Some(Tty::new());
        self
    }

    async fn message_handler(
        mut reader: TlsReader,
        #[allow(unused_mut)] mut _writer: TlsWriter,
        proxy_connections: ProxyConnections,
        #[cfg(feature = "tty")] _tty: Option<Tty>,
    ) -> Result<()> {
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        loop {
            let message = Message::pull(&mut reader).await?;
            match message.data_type {
                DataType::Tty => {
                    stdout.write_all(&message.data).await?;
                    stdout.flush().await?;
                }
                DataType::Error => {
                    stderr.write_all(&message.data).await?;
                    stderr.write_all(b"\r\n").await?;
                    stderr.flush().await?;
                }
                DataType::Connection => {
                    let id = message.header_id;
                    let mut proxy_connections = proxy_connections.lock().await;
                    if let Some(proxy_connection) = proxy_connections.get(&id) {
                        if message.data.is_empty() {
                            proxy_connections.remove(&id);
                        } else {
                            let mut writer = proxy_connection.writer.lock().await;
                            let writer = writer.as_mut().context("error")?;
                            writer.write_all(&message.data).await?;
                        }
                    }
                }
                _ => {
                    debug!("Unknown message: {:?}", message);
                }
            }

            #[cfg(feature = "tty")]
            if UPDATE_WINSIZE.load(Ordering::Relaxed) {
                debug!("Updating winsize");
                let tty_winsize = Tty::get_winsize();
                let mut data = Vec::new();
                data.extend(u16::to_be_bytes(tty_winsize.ws_row));
                data.extend(u16::to_be_bytes(tty_winsize.ws_col));
                Message::new()
                    .data_type(DataType::Winresize)
                    .data(data)
                    .push(&mut _writer)
                    .await?;
                UPDATE_WINSIZE.store(false, Ordering::Relaxed);
            }
        }
    }

    pub async fn stdin_handler(mut writer: TlsWriter) -> Result<()> {
        // https://github.com/tokio-rs/tokio/issues/2466
        let mut stdin = tokio_fd::AsyncFd::try_from(libc::STDIN_FILENO)?;

        let mut buf = [0u8; 1024];
        loop {
            let bytes_read = stdin.read(&mut buf).await?;
            Message::new()
                .data_type(DataType::Tty)
                .data(buf[..bytes_read].to_vec())
                .push(&mut writer)
                .await?;
        }
    }

    pub async fn proxy_create(mut writer: TlsWriter, proxy_string: &str) -> Result<()> {
        Message::new()
            .data_type(DataType::Proxy)
            .header_type(ProxyHeaderType::Create)
            .header_proxy_type(ProxyType::Static)
            .data(proxy_string.as_bytes().to_vec())
            .push(&mut writer)
            .await?;
        Ok(())
    }

    pub async fn connection_create(
        mut writer: TlsWriter,
        id: u16,
        connection_string: &str,
    ) -> Result<()> {
        Message::new()
            .data_type(DataType::Connection)
            .header_type(ConnectionHeaderType::Create)
            .header_id(id)
            .data(connection_string.as_bytes().to_vec())
            .push(&mut writer)
            .await?;
        Ok(())
    }

    pub async fn connection_data(mut writer: TlsWriter, id: u16, data: &[u8]) -> Result<()> {
        Message::new()
            .data_type(DataType::Connection)
            .header_type(ConnectionHeaderType::Data)
            .header_id(id)
            .data(data.to_vec())
            .push(&mut writer)
            .await?;
        Ok(())
    }

    pub async fn proxy_handler(
        mut stream: TcpStream,
        proxy_connections: ProxyConnections,
        id: u16,
        writer: TlsWriter,
    ) -> Result<()> {
        let mut buf = [0u8; 1];

        stream.read_exact(&mut buf).await?;
        let version = u8::from_be_bytes(buf);
        if version != 4 {
            bail!("Wrong socks version");
        }

        stream.read_exact(&mut buf).await?;
        let command = u8::from_be_bytes(buf);
        if command != 1 {
            bail!("Wrong command");
        }

        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await?;
        let dst_port = u16::from_be_bytes(buf);

        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await?;
        let dst_ip = std::net::Ipv4Addr::from(buf);

        let mut buf = [0u8; 1];
        stream.read_exact(&mut buf).await?;

        let connection_string = format!("{}:{}", dst_ip, dst_port);

        Self::connection_create(writer.clone(), id, &connection_string).await?;

        stream.write_all(&u8::to_be_bytes(0)).await?;
        stream.write_all(&u8::to_be_bytes(90)).await?;
        stream.write_all(&u16::to_be_bytes(dst_port)).await?;
        stream.write_all(&dst_ip.octets()).await?;

        let (r, w) = tokio::io::split(stream);

        {
            let mut proxy_connections = proxy_connections.lock().await;
            proxy_connections.insert(
                id,
                ProxyConnection {
                    writer: Arc::new(Mutex::new(Some(w))),
                },
            );
        }

        tokio::spawn(Self::proxy_reader(
            r,
            id,
            proxy_connections.clone(),
            writer.clone(),
        ));

        Ok(())
    }

    pub async fn proxy_reader(
        mut local_reader: ReadHalf<TcpStream>,
        id: u16,
        proxy_connections: ProxyConnections,
        remote_writer: TlsWriter,
    ) -> Result<()> {
        loop {
            let mut buf = [0u8; 1024];
            tokio::select! {
                n = local_reader.read(&mut buf) => {
                    match n {
                        Ok(n) => {
                            if n < 1 {
                                break;
                            }
                            Self::connection_data(remote_writer.clone(), id, &buf[..n]).await?;
                        }
                        _ => break,
                    }
                },
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                    let proxy_connections = proxy_connections.lock().await;
                    if proxy_connections.get(&id).is_none() {
                        break;
                    }
                },
            }
        }
        let mut proxy_connections = proxy_connections.lock().await;
        proxy_connections.remove(&id);
        Ok(())
    }

    pub async fn proxy_listener(
        listen_address: String,
        proxy_connections: ProxyConnections,
        writer: TlsWriter,
    ) -> Result<()> {
        let listener = TcpListener::bind(listen_address).await?;

        loop {
            if let Ok((stream, address)) = listener.accept().await {
                tokio::spawn(Self::proxy_handler(
                    stream,
                    proxy_connections.clone(),
                    address.port(),
                    writer.clone(),
                ));
            }
        }
    }

    pub async fn run(self) -> Result<()> {
        if let Some(proxy_address) = self.proxy_address {
            Self::proxy_create(
                self.writer.clone(),
                &format!("{}:127.0.0.1:1081", proxy_address.port()),
            )
            .await?;
        }
        let message_handler = tokio::spawn(Self::message_handler(
            self.reader.clone(),
            self.writer.clone(),
            self.proxy_connections.clone(),
            #[cfg(feature = "tty")]
            self.tty,
        ));
        if let Some(proxy_address) = self.proxy_address {
            tokio::spawn(Self::proxy_listener(
                format!("{}:{}", proxy_address.ip(), proxy_address.port()),
                self.proxy_connections.clone(),
                self.writer.clone(),
            ));
        }
        let stdin_handler = tokio::spawn(Self::stdin_handler(self.writer));

        tokio::select! {
            _ = stdin_handler => {
                debug!("stdin_handler() exited");
            }
            _ = message_handler => {
                debug!("message_handler() exited");
            }
        };

        Ok(())
    }
}
