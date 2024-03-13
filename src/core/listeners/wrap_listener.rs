use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
};

use tokio::net::TcpListener;

use crate::{
    core::{Stream, WrapStream},
    Helper,
};

use super::wrap_tls_accepter::WrapTlsAccepter;

pub struct WrapListener {
    pub addrs: Option<Vec<SocketAddr>>,
    pub listener: Option<TcpListener>,
    pub accepter: Option<WrapTlsAccepter>,
    pub desc: &'static str
}

impl WrapListener {
    pub fn new<T: ToSocketAddrs>(bind: T) -> io::Result<WrapListener>  {
        let socks = bind.to_socket_addrs()?;
        let addrs = socks.collect::<Vec<SocketAddr>>();
        Ok(Self {
            addrs: Some(addrs),
            listener: None,
            accepter: None,
            desc: "",
        })
    }

    pub fn new_listener(listener: TcpListener) -> WrapListener {
        Self {
            addrs: None,
            listener: Some(listener),
            accepter: None,
            desc: "",
        }
    }

    pub fn new_tls<T: ToSocketAddrs>(bind: T, cert: &str, key: &str) -> io::Result<WrapListener> {
        let socks = bind.to_socket_addrs()?;
        let addrs = socks.collect::<Vec<SocketAddr>>();
        let accepter = WrapTlsAccepter::new_cert(&Some(cert.to_string()), &Some(key.to_string()))?;
        Ok(Self {
            addrs: Some(addrs),
            listener: None,
            accepter: Some(accepter),
            desc: "",
        })
    }

    pub fn new_listener_tls(
        listener: TcpListener,
        cert: &str,
        key: &str,
    ) -> io::Result<WrapListener> {
        let accepter = WrapTlsAccepter::new_cert(&Some(cert.to_string()), &Some(key.to_string()))?;
        Ok(Self {
            addrs: None,
            listener: Some(listener),
            accepter: Some(accepter),
            desc: "",
        })
    }

    pub fn new_tls_multi<T: ToSocketAddrs>(
        bind: T,
        infos: Vec<(String, String, String)>,
    ) -> io::Result<WrapListener> {
        let socks = bind.to_socket_addrs()?;
        let addrs = socks.collect::<Vec<SocketAddr>>();
        let accepter = WrapTlsAccepter::new_multi(infos)?;
        Ok(Self {
            addrs: Some(addrs),
            listener: None,
            accepter: Some(accepter),
            desc: "",
        })
    }

    pub fn new_listener_tls_multi(
        listener: TcpListener,
        infos: Vec<(String, String, String)>,
    ) -> io::Result<WrapListener> {
        let accepter = WrapTlsAccepter::new_multi(infos)?;
        Ok(Self {
            addrs: None,
            listener: Some(listener),
            accepter: Some(accepter),
            desc: "",
        })
    }

    pub fn local_desc(&self) -> String {
        match &self.listener {
            Some(l) => l
                .local_addr()
                .map(|s| format!("{s}"))
                .unwrap_or("unknown".to_string()),
            None => "unknown".to_string(),
        }
    }

    pub fn set_desc(&mut self, desc: &'static str) {
        self.desc = desc;
    }

    pub async fn try_init(&mut self) -> io::Result<()> {
        if self.listener.is_some() {
            Ok(())
        } else {
            match &self.addrs {
                Some(addrs) => {
                    let l = Helper::bind(&addrs[..]).await?;
                    self.listener = Some(l);
                    Ok(())
                }
                None => {
                    Err(io::Error::new(io::ErrorKind::Other, "unknow addrs"))
                }
            }
        }
    }

    pub async fn accept(&mut self) -> io::Result<Stream> {
        match &self.listener {
            Some(l) => {
                let (stream, addr) = l.accept().await?;
                if let Some(accept) = &self.accepter {
                    let stream = accept.accept(stream)?.await?;
                    let mut stream = WrapStream::with_addr(stream, addr);
                    stream.set_desc(self.desc);
                    Ok(Box::new(stream))
                } else {
                    let mut stream = WrapStream::with_addr(stream, addr);
                    stream.set_desc(self.desc);
                    Ok(Box::new(stream))
                }
            }
            None => Err(io::Error::new(io::ErrorKind::Other, "not init listener")),
        }
    }
}
