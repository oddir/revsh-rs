use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_native_tls::native_tls::TlsAcceptor as NativeTlsAcceptor;
use tokio_native_tls::native_tls::{Identity, Protocol};
use tokio_native_tls::TlsAcceptor as TokioTlsAcceptor;
use tokio_native_tls::TlsStream;

use crate::broker::Broker;
use crate::message::{DataType, Message};

pub struct Control {
    message_data_size: u16,
    shell: String,
    env: Vec<String>,
    term_width: u16,
    term_height: u16,
    proxy_addr: Option<SocketAddr>,
    listener: TcpListener,
    acceptor: TokioTlsAcceptor,
    stream: Arc<Mutex<Option<TlsStream<TcpStream>>>>,
}

impl Control {
    pub async fn new(addr: SocketAddr, keyfile: &str) -> Result<Self> {
        let listener: TcpListener = TcpListener::bind(&addr).await?;

        // openssl pkcs12 -export -out identity.pfx -inkey key.pem -in cert.pem
        let mut file = File::open(keyfile)?;
        let mut identity = vec![];
        file.read_to_end(&mut identity)?;
        let identity = Identity::from_pkcs12(&identity, "")?;

        let acceptor = NativeTlsAcceptor::builder(identity)
            .min_protocol_version(Some(Protocol::Sslv3))
            .build()?;

        let acceptor = TokioTlsAcceptor::from(acceptor);

        Ok(Self {
            message_data_size: u16::MAX,
            shell: "/bin/sh".to_string(),
            env: vec!["PATH=/bin:/usr/bin/".to_string()],
            proxy_addr: None,
            term_width: 1000,  // ??
            term_height: 1000, // ??
            listener: listener,
            acceptor: acceptor,
            stream: Arc::new(Mutex::new(None)),
        })
    }

    pub fn shell(mut self, shell: String) -> Self {
        self.shell = shell;
        self
    }

    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    pub fn proxy(mut self, proxy_addr: Option<SocketAddr>) -> Self {
        self.proxy_addr = proxy_addr;
        self
    }

    pub async fn accept(&mut self) -> Result<()> {
        let (stream, _remote_addr) = self.listener.accept().await?;
        let acceptor = self.acceptor.clone();
        let stream = acceptor.accept(stream).await?;
        self.stream = Arc::new(Mutex::new(Some(stream)));
        self.handle_client().await?;
        Ok(())
    }

    pub async fn handle_client(&mut self) -> Result<()> {
        println!("got connection");

        self.negotiate_protocol().await?;
        println!("protocol ok");

        // send interactive
        Message::new()
            .data_type(DataType::Init)
            .data(vec![0x1])
            .push(&mut self.stream)
            .await?;

        let _message = Message::pull(&mut self.stream).await?;

        // initial shell data
        Message::new()
            .data_type(DataType::Init)
            .data(self.shell.as_bytes().to_vec())
            .push(&mut self.stream)
            .await?;

        // env
        Message::new()
            .data_type(DataType::Init)
            .data(self.env.join(" ").as_bytes().to_vec())
            .push(&mut self.stream)
            .await?;

        // termios
        let mut winsize = Vec::new();
        winsize.append(&mut u16::to_be_bytes(self.term_width).to_vec());
        winsize.append(&mut u16::to_be_bytes(self.term_height).to_vec());

        Message::new()
            .data_type(DataType::Init)
            .data(winsize)
            .push(&mut self.stream)
            .await?;

        Ok(())
    }

    pub async fn broker(self) -> Result<()> {
        Broker::new()?
            .proxy(self.proxy_addr)
            .run(self.stream)
            .await?;
        Ok(())
    }

    pub async fn negotiate_protocol(&mut self) -> Result<()> {
        let stream = self.stream.clone();
        let mut stream = stream.lock().await;
        let stream = stream.as_mut().context("error")?;

        // send proto major
        stream.write(&u16::to_be_bytes(1)).await?;

        // send proto minor
        stream.write(&u16::to_be_bytes(0)).await?;

        let mut buf = [0u8; 2];

        // recv proto major
        stream.read_exact(&mut buf).await?;
        let proto_major = u16::from_be_bytes(buf);

        // recv proto minor
        stream.read_exact(&mut buf).await?;
        let proto_minor = u16::from_be_bytes(buf);

        println!("proto {}.{}", proto_major, proto_minor);

        // send desired data size
        stream
            .write(&u16::to_be_bytes(self.message_data_size))
            .await?;

        // recv desired data size
        stream.read_exact(&mut buf).await?;
        let data_size = u16::from_be_bytes(buf);

        if data_size < 1024 {
            bail!("Can't agree on a message size");
        }

        if data_size < self.message_data_size {
            self.message_data_size = data_size;
        }

        println!("data size {}", self.message_data_size);

        Ok(())
    }
}
