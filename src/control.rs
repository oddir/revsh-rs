use anyhow::{bail, Context, Result};
use log::debug;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
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
#[cfg(feature = "tty")]
use crate::tty::Tty;

type MyTlsStream = Arc<Mutex<Option<TlsStream<TcpStream>>>>;

pub struct Control {
    message_data_size: u16,
    shell: String,
    env: Vec<String>,
    pub proxy_address: Option<SocketAddr>,
    listener: TcpListener,
    acceptor: TokioTlsAcceptor,
    pub stream: MyTlsStream,
}

impl Control {
    pub async fn new(address: SocketAddr, key_file: &Path) -> Result<Self> {
        let listener: TcpListener = TcpListener::bind(&address).await?;

        // openssl pkcs12 -export -out identity.pfx -inkey key.pem -in cert.pem
        let mut file = File::open(key_file)?;
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
            proxy_address: None,
            listener,
            acceptor,
            stream: Arc::new(Mutex::new(None)),
        })
    }

    pub fn shell<'a>(&'a mut self, shell: String) -> &'a mut Self {
        self.shell = shell;
        self
    }

    pub fn env<'a>(&'a mut self, env: Vec<String>) -> &'a mut Self {
        self.env = env;
        self
    }

    pub fn proxy<'a>(&'a mut self, proxy_address: Option<SocketAddr>) -> &'a mut Self {
        self.proxy_address = proxy_address;
        self
    }

    pub async fn accept(&mut self) -> Result<Broker> {
        let (stream, remote_address) = self.listener.accept().await?;
        let acceptor = self.acceptor.clone();
        let stream = acceptor.accept(stream).await?;
        self.stream = Arc::new(Mutex::new(Some(stream)));
        self.handle_client().await?;
        Broker::new(self, remote_address).await
    }

    pub async fn handle_client(&mut self) -> Result<()> {
        debug!("Got connection");

        self.negotiate_protocol().await?;
        debug!("Protocol ok");

        // Send interactive
        Message::new()
            .data_type(DataType::Init)
            .data(vec![0x1])
            .push(&mut self.stream)
            .await?;

        let _message = Message::pull(&mut self.stream).await?;

        // Initial shell data
        Message::new()
            .data_type(DataType::Init)
            .data(self.shell.as_bytes().to_vec())
            .push(&mut self.stream)
            .await?;

        // Env
        Message::new()
            .data_type(DataType::Init)
            .data(self.env.join(" ").as_bytes().to_vec())
            .push(&mut self.stream)
            .await?;

        // Termios
        #[cfg(feature = "tty")]
        let (term_width, term_height) = Tty::get_term_size();
        #[cfg(not(feature = "tty"))]
        let (term_width, term_height) = (0, 0);

        let mut data = Vec::new();
        data.append(&mut u16::to_be_bytes(term_width).to_vec());
        data.append(&mut u16::to_be_bytes(term_height).to_vec());

        Message::new()
            .data_type(DataType::Init)
            .data(data)
            .push(&mut self.stream)
            .await?;

        Ok(())
    }

    pub async fn negotiate_protocol(&mut self) -> Result<()> {
        let stream = self.stream.clone();
        let mut stream = stream.lock().await;
        let stream = stream.as_mut().context("error")?;

        // Send proto major
        stream.write_all(&u16::to_be_bytes(1)).await?;

        // Send proto minor
        stream.write_all(&u16::to_be_bytes(0)).await?;

        let mut buf = [0u8; 2];

        // Recv proto major
        stream.read_exact(&mut buf).await?;
        let proto_major = u16::from_be_bytes(buf);

        // Recv proto minor
        stream.read_exact(&mut buf).await?;
        let proto_minor = u16::from_be_bytes(buf);

        debug!("Proto {}.{}", proto_major, proto_minor);

        // Send desired data size
        stream
            .write_all(&u16::to_be_bytes(self.message_data_size))
            .await?;

        // Recv desired data size
        stream.read_exact(&mut buf).await?;
        let data_size = u16::from_be_bytes(buf);

        if data_size < 1024 {
            bail!("Can't agree on a message size");
        }

        if data_size < self.message_data_size {
            self.message_data_size = data_size;
        }

        debug!("Data size {}", self.message_data_size);

        Ok(())
    }
}
