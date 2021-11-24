use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_native_tls::TlsStream;

use crate::message::{ConnectionHeaderType, DataType, Message, ProxyHeaderType, ProxyType};

type TlsReader = Arc<Mutex<Option<ReadHalf<TlsStream<TcpStream>>>>>;
type TlsWriter = Arc<Mutex<Option<WriteHalf<TlsStream<TcpStream>>>>>;
type StreamWriter = Arc<Mutex<Option<WriteHalf<TcpStream>>>>;
type ProxyConnections = Arc<Mutex<HashMap<u16, ProxyConnection>>>;

pub struct Broker {
    proxy_addr: Option<SocketAddr>,
    reader: TlsReader,
    writer: TlsWriter,
    proxy_connections: ProxyConnections,
}

pub struct ProxyConnection {
    writer: StreamWriter,
}

impl Broker {
    async fn message_handler(
        mut reader: TlsReader,
        proxy_connections: ProxyConnections,
    ) -> Result<()> {
        loop {
            let message = Message::pull(&mut reader).await?;
            match message.data_type {
                DataType::Tty => {
                    let s = std::str::from_utf8(&message.data)?;
                    print!("{}", s);
                    std::io::stdout().flush()?;
                }
                DataType::Error => {
                    //let s = std::str::from_utf8(&message.data)?;
                    //print!("{}", s);
                    //std::io::stdout().flush()?;
                }
                DataType::Connection => {
                    let id = message.header_id;
                    let mut proxy_connections = proxy_connections.lock().await;
                    if let Some(proxy_connection) = proxy_connections.get(&id) {
                        if message.data.len() < 1 {
                            proxy_connections.remove(&id);
                        } else {
                            let mut writer = proxy_connection.writer.lock().await;
                            let writer = writer.as_mut().context("error")?;
                            writer.write(&message.data).await?;
                        }
                    }
                }
                _ => {
                    //dbg!(message);
                }
            }
        }
    }

    pub async fn stdin_handler(&mut self) -> Result<()> {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line? + "\n";
            Message::new()
                .data_type(DataType::Tty)
                .data(line.as_bytes().to_vec())
                .push(&mut self.writer)
                .await?;
        }
        Ok(())
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

        stream.write(&u8::to_be_bytes(0)).await?;
        stream.write(&u8::to_be_bytes(90)).await?;
        stream.write(&u16::to_be_bytes(dst_port)).await?;
        stream.write(&dst_ip.octets()).await?;

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
                    if let None = proxy_connections.get(&id) {
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
        listen_addr: String,
        proxy_connections: ProxyConnections,
        writer: TlsWriter,
    ) -> Result<()> {
        let listener = TcpListener::bind(listen_addr).await?;

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    tokio::spawn(Self::proxy_handler(
                        stream,
                        proxy_connections.clone(),
                        addr.port(),
                        writer.clone(),
                    ));
                }
                Err(_) => {}
            }
        }
    }

    pub fn new() -> Result<Self> {
        Ok(Self {
            proxy_addr: None,
            reader: Arc::new(Mutex::new(None)),
            writer: Arc::new(Mutex::new(None)),
            proxy_connections: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn proxy(mut self, proxy_addr: Option<SocketAddr>) -> Self {
        self.proxy_addr = proxy_addr;
        self
    }

    pub async fn run(&mut self, stream: Arc<Mutex<Option<TlsStream<TcpStream>>>>) -> Result<()> {
        let stream = stream.lock().await.take().context("error")?;
        let (r, w) = tokio::io::split(stream);
        self.writer = Arc::new(Mutex::new(Some(w)));
        self.reader = Arc::new(Mutex::new(Some(r)));
        if let Some(proxy_addr) = self.proxy_addr {
            Self::proxy_create(
                self.writer.clone(),
                &format!("{}:127.0.0.1:1081", proxy_addr.port()),
            )
            .await?;
        }
        tokio::spawn(Self::message_handler(
            self.reader.clone(),
            self.proxy_connections.clone(),
        ));
        if let Some(proxy_addr) = self.proxy_addr {
            tokio::spawn(Self::proxy_listener(
                format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
                self.proxy_connections.clone(),
                self.writer.clone(),
            ));
        }
        self.stdin_handler().await?;
        Ok(())
    }
}
