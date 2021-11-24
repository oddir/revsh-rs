use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

pub trait HeaderType {
    fn value(&self) -> u16;
}

#[repr(u8)]
#[derive(Debug, PartialEq)]
#[allow(unused)]
pub enum ProxyType {
    Static = 0,
    Dynamic = 1,
    Tun = 2,
    Tap = 3,
}

impl HeaderType for ProxyType {
    fn value(&self) -> u16 {
        match *self {
            ProxyType::Static => 0,
            ProxyType::Dynamic => 1,
            ProxyType::Tun => 2,
            ProxyType::Tap => 3,
        }
    }
}

#[repr(u8)]
#[derive(Debug, PartialEq)]
pub enum ProxyHeaderType {
    Create = 0,
    Destroy = 1,
    Report = 2,
    Unknown = 3,
}

impl HeaderType for ProxyHeaderType {
    fn value(&self) -> u16 {
        match *self {
            ProxyHeaderType::Create => 0,
            ProxyHeaderType::Destroy => 1,
            ProxyHeaderType::Report => 2,
            _ => 3,
        }
    }
}

impl From<u16> for ProxyHeaderType {
    fn from(n: u16) -> ProxyHeaderType {
        match n {
            0 => ProxyHeaderType::Create,
            1 => ProxyHeaderType::Destroy,
            2 => ProxyHeaderType::Report,
            _ => ProxyHeaderType::Unknown,
        }
    }
}

#[repr(u8)]
#[derive(Debug, PartialEq)]
pub enum ConnectionHeaderType {
    Create = 0,
    Destroy = 1,
    Data = 2,
    Dormant = 3,
    Active = 4,
    Unknown = 5,
}

impl HeaderType for ConnectionHeaderType {
    fn value(&self) -> u16 {
        match *self {
            ConnectionHeaderType::Create => 0,
            ConnectionHeaderType::Destroy => 1,
            ConnectionHeaderType::Data => 2,
            ConnectionHeaderType::Dormant => 3,
            ConnectionHeaderType::Active => 4,
            _ => 5,
        }
    }
}

impl From<u16> for ConnectionHeaderType {
    fn from(n: u16) -> ConnectionHeaderType {
        match n {
            0 => ConnectionHeaderType::Create,
            1 => ConnectionHeaderType::Destroy,
            2 => ConnectionHeaderType::Data,
            3 => ConnectionHeaderType::Dormant,
            4 => ConnectionHeaderType::Active,
            _ => ConnectionHeaderType::Unknown,
        }
    }
}

#[repr(u8)]
#[derive(Debug, PartialEq)]
pub enum DataType {
    Init = 0,
    Tty = 1,
    Winresize = 2,
    Proxy = 3,
    Connection = 4,
    Nop = 5,
    Error = 6,
    Unknown = 7,
}

impl DataType {
    pub fn value(&self) -> u8 {
        match *self {
            DataType::Init => 0,
            DataType::Tty => 1,
            DataType::Winresize => 2,
            DataType::Proxy => 3,
            DataType::Connection => 4,
            DataType::Nop => 5,
            DataType::Error => 6,
            DataType::Unknown => 7,
        }
    }
}

impl From<u8> for DataType {
    fn from(n: u8) -> DataType {
        match n {
            0 => DataType::Init,
            1 => DataType::Tty,
            2 => DataType::Winresize,
            3 => DataType::Proxy,
            4 => DataType::Connection,
            5 => DataType::Nop,
            6 => DataType::Error,
            _ => DataType::Unknown,
        }
    }
}

#[derive(Debug)]
pub struct Message {
    pub data_type: DataType,
    pub data: Vec<u8>,
    pub header_type: u16,
    pub header_origin: u16,
    pub header_id: u16,
    pub header_proxy_type: u16,
}

impl Message {
    pub fn new() -> Self {
        Self {
            data_type: DataType::Unknown,
            data: vec![],
            header_type: 0,
            header_origin: 0,
            header_id: 0,
            header_proxy_type: 0,
        }
    }

    pub fn data_type(mut self, data_type: DataType) -> Self {
        self.data_type = data_type;
        self
    }

    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.data = data;
        self
    }

    pub fn header_type<T: HeaderType>(mut self, header_type: T) -> Self {
        self.header_type = header_type.value();
        self
    }

    pub fn header_id(mut self, header_id: u16) -> Self {
        self.header_id = header_id;
        self
    }

    pub fn header_proxy_type(mut self, header_proxy_type: ProxyType) -> Self {
        self.header_proxy_type = header_proxy_type.value();
        self
    }

    pub async fn push<T>(self, stream: &mut Arc<Mutex<Option<T>>>) -> Result<()>
    where
        T: AsyncWriteExt + std::marker::Unpin,
    {
        let stream = stream.clone();
        let mut stream = stream.lock().await;
        let stream = stream.as_mut().context("error")?;

        let mut header_len: u16 = (std::mem::size_of::<DataType>() + 2).try_into()?;

        match self.data_type {
            DataType::Proxy | DataType::Connection => {
                // sizeof(message->header_type) + sizeof(message->header_origin) + sizeof(message->header_id)
                header_len += 3 * 2;
                if ProxyHeaderType::from(self.header_type) == ProxyHeaderType::Create
                    || ProxyHeaderType::from(self.header_type) == ProxyHeaderType::Report
                    || ConnectionHeaderType::from(self.header_type) == ConnectionHeaderType::Create
                {
                    header_len += 2;
                }
            }
            _ => {}
        }

        stream.write(&u16::to_be_bytes(header_len)).await?;
        stream
            .write(&u8::to_be_bytes(self.data_type.value()))
            .await?;
        stream
            .write(&u16::to_be_bytes(self.data.len().try_into()?))
            .await?;

        match self.data_type {
            DataType::Proxy | DataType::Connection => {
                stream.write(&u16::to_be_bytes(self.header_type)).await?;
                stream.write(&u16::to_be_bytes(self.header_origin)).await?;
                stream.write(&u16::to_be_bytes(self.header_id)).await?;
                if ProxyHeaderType::from(self.header_type) == ProxyHeaderType::Create
                    || ProxyHeaderType::from(self.header_type) == ProxyHeaderType::Report
                    || ConnectionHeaderType::from(self.header_type) == ConnectionHeaderType::Create
                {
                    stream
                        .write(&u16::to_be_bytes(self.header_proxy_type))
                        .await?;
                }
            }
            _ => {}
        }

        stream.write(&self.data).await?;
        Ok(())
    }

    pub async fn pull<T>(stream: &mut Arc<Mutex<Option<T>>>) -> Result<Self>
    where
        T: AsyncReadExt + std::marker::Unpin,
    {
        let stream = stream.clone();
        let mut stream = stream.lock().await;
        let stream = stream.as_mut().context("error")?;

        let mut message = Self::new();

        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await?;
        let mut header_len = u16::from_be_bytes(buf);

        let mut buf = [0u8; 1];
        stream.read_exact(&mut buf).await?;
        message.data_type = u8::from_be_bytes(buf).into();
        header_len -= 1;

        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await?;
        let data_len = u16::from_be_bytes(buf);
        header_len -= 2;

        match message.data_type {
            DataType::Proxy | DataType::Connection => {
                let mut buf = [0u8; 2];
                stream.read_exact(&mut buf).await?;
                message.header_type = u16::from_be_bytes(buf);
                header_len -= 2;

                let mut buf = [0u8; 2];
                stream.read_exact(&mut buf).await?;
                message.header_origin = u16::from_be_bytes(buf);
                header_len -= 2;

                let mut buf = [0u8; 2];
                stream.read_exact(&mut buf).await?;
                message.header_id = u16::from_be_bytes(buf);
                header_len -= 2;

                if ProxyHeaderType::from(message.header_type) == ProxyHeaderType::Create
                    || ProxyHeaderType::from(message.header_type) == ProxyHeaderType::Report
                    || ConnectionHeaderType::from(message.header_type)
                        == ConnectionHeaderType::Create
                {
                    let mut buf = [0u8; 2];
                    stream.read_exact(&mut buf).await?;
                    message.header_proxy_type = u16::from_be_bytes(buf);
                    header_len -= 2;
                }
            }
            _ => {}
        }

        if header_len > 0 {
            let mut buf = vec![0u8; header_len.into()];
            stream.read_exact(&mut buf).await?;
        }

        message.data = vec![0u8; data_len.into()];
        stream.read_exact(&mut message.data).await?;

        Ok(message)
    }
}
