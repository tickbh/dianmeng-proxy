// Copyright 2022 - 2023 Wenmeng See the COPYRIGHT
// file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
//
// Author: tickbh
// -----
// Created Date: 2023/09/25 10:08:56

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{collections::HashMap, io};
use lazy_static::lazy_static;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::Receiver;
use tokio::{io::split, net::TcpStream, sync::mpsc::channel};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::Sender,
};
use tokio_rustls::{client::TlsStream, TlsConnector};

use webparse::{BinaryMut, Buf};

use crate::proxy::ProxyServer;
use crate::{
    HealthCheck, Helper, MappingConfig, ProtClose, ProtCreate, ProtFrame, ProxyConfig, ProxyResult,
    TransStream, VirtualStream,
};

lazy_static!{
    /// 绑定的下一个sock_map映射，为单数
    static ref NEXT_ID: Mutex<u32> = Mutex::new(1);
}

/// 中心客户端
/// 负责与服务端建立连接，断开后自动再重连
pub struct CenterClient {
    option: ProxyConfig,
    /// tls的客户端连接信息
    tls_client: Option<Arc<rustls::ClientConfig>>,
    /// tls的客户端连接域名
    domain: Option<String>,
    /// 连接中心服务器的地址
    server_addr: String,
    /// 内网映射的相关消息
    mappings: Vec<MappingConfig>,

    /// 存在普通连接和加密连接，此处不为None则表示普通连接
    stream: Option<TcpStream>,
    /// 存在普通连接和加密连接，此处不为None则表示加密连接
    tls_stream: Option<TlsStream<TcpStream>>,

    /// 发送Create，并将绑定的Sender发到做绑定
    sender_work: Sender<(ProtCreate, Sender<ProtFrame>)>,
    /// 接收的Sender绑定，开始服务时这值move到工作协程中，所以不能二次调用服务
    receiver_work: Option<Receiver<(ProtCreate, Sender<ProtFrame>)>>,

    /// 发送协议数据，接收到服务端的流数据，转发给相应的Stream
    sender: Sender<ProtFrame>,
    /// 接收协议数据，并转发到服务端。
    receiver: Option<Receiver<ProtFrame>>,
}

impl CenterClient {
    pub fn new(
        option: ProxyConfig,
        server_addr: String,
        tls_client: Option<Arc<rustls::ClientConfig>>,
        domain: Option<String>,
        mappings: Vec<MappingConfig>,
    ) -> Self {
        let (sender, receiver) = channel::<ProtFrame>(100);
        let (sender_work, receiver_work) = channel::<(ProtCreate, Sender<ProtFrame>)>(10);

        Self {
            option,
            tls_client,
            domain,
            server_addr,
            mappings,
            stream: None,
            tls_stream: None,

            sender_work,
            receiver_work: Some(receiver_work),
            sender,
            receiver: Some(receiver),
        }
    }

    async fn inner_connect(
        tls_client: Option<Arc<rustls::ClientConfig>>,
        server_addr: String,
        domain: Option<String>,
    ) -> ProxyResult<(Option<TcpStream>, Option<TlsStream<TcpStream>>)> {
        if tls_client.is_some() {
            let connector = TlsConnector::from(tls_client.unwrap());
            let stream = HealthCheck::connect(&server_addr).await?;
            // 这里的域名只为认证设置
            let domain =
                rustls::pki_types::ServerName::try_from(domain.unwrap_or("soft.wm-proxy.com".to_string()))
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dnsname"))?;

            let outbound = connector.connect(domain, stream).await?;
            Ok((None, Some(outbound)))
        } else {
            let outbound = HealthCheck::connect(&server_addr).await?;
            Ok((Some(outbound), None))
        }
    }

    pub async fn connect(&mut self) -> ProxyResult<bool> {
        let (stream, tls_stream) = Self::inner_connect(
            self.tls_client.clone(),
            self.server_addr.clone(),
            self.domain.clone(),
        )
        .await?;
        self.stream = stream;
        self.tls_stream = tls_stream;
        Ok(self.stream.is_some() || self.tls_stream.is_some())
    }

    async fn inner_serve<T>(
        option: &ProxyConfig,
        stream: T,
        sender: &mut Sender<ProtFrame>,
        receiver_work: &mut Receiver<(ProtCreate, Sender<ProtFrame>)>,
        receiver: &mut Receiver<ProtFrame>,
        mappings: &mut Vec<MappingConfig>,
    ) -> ProxyResult<()>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let mut map = HashMap::<u64, Sender<ProtFrame>>::new();
        let mut read_buf = BinaryMut::new();
        let mut write_buf = BinaryMut::new();
        let (mut reader, mut writer) = split(stream);
        let mut vec = Vec::with_capacity(4096);
        vec.resize(4096, 0);
        let is_closed;
        if option.username.is_some() && option.password.is_some() {
            ProtFrame::new_token(
                option.username.clone().unwrap(),
                option.password.clone().unwrap(),
            )
            .encode(&mut write_buf)?;
        }
        if mappings.len() > 0 {
            ProtFrame::new_mapping(0, mappings.clone()).encode(&mut write_buf)?;
        }
        loop {
            let _ = tokio::select! {
                // 严格的顺序流
                biased;
                // 新的流建立，这里接收Create并进行绑定
                r = receiver_work.recv() => {
                    if let Some((create, sender)) = r {
                        map.insert(create.sock_map(), sender);
                        let _ = create.encode(&mut write_buf);
                    }
                }
                // 数据的接收，并将数据写入给远程端
                r = receiver.recv() => {
                    if let Some(p) = r {
                        let _ = p.encode(&mut write_buf);
                    }
                }
                // 数据的等待读取，一旦流可读则触发，读到0则关闭主动关闭所有连接
                r = reader.read(&mut vec) => {
                    match r {
                        Ok(0)=>{
                            is_closed=true;
                            break;
                        }
                        Ok(n) => {
                            read_buf.put_slice(&vec[..n]);
                        }
                        Err(_err) => {
                            is_closed = true;
                            break;
                        },
                    }
                }
                // 一旦有写数据，则尝试写入数据，写入成功后扣除相应的数据
                r = writer.write(write_buf.chunk()), if write_buf.has_remaining() => {
                    match r {
                        Ok(n) => {
                            write_buf.advance(n);
                            if !write_buf.has_remaining() {
                                write_buf.clear();
                            }
                        }
                        Err(e) => {
                            log::info!("写入的时候发生错误，错误内容为:{:?}", e);
                        },
                    }
                }
            };

            loop {
                // 将读出来的数据全部解析成ProtFrame并进行相应的处理，如果是0则是自身消息，其它进行转发
                match Helper::decode_frame(&mut read_buf)? {
                    Some(p) => {
                        match p {
                            ProtFrame::Create(p) => {
                                let domain = p.domain().clone().unwrap_or(String::new());
                                let mut mapping = None;
                                for m in &*mappings {
                                    if m.domain == domain || m.name == domain {
                                        mapping = Some(m);
                                    }
                                }
                                if mapping.is_none() {
                                    log::info!("本地地址为空，无法做内网映射");
                                    log::warn!("local addr is none, can't mapping");
                                    let _ = sender.send(ProtFrame::new_close(p.sock_map())).await;
                                    continue;
                                }
                                let (virtual_sender, virtual_receiver) = channel::<ProtFrame>(10);
                                map.insert(p.sock_map(), virtual_sender);

                                if mapping.as_ref().unwrap().is_proxy() {
                                    let stream = VirtualStream::new(
                                        p.sock_map(),
                                        sender.clone(),
                                        virtual_receiver,
                                    );

                                    let proxy_server = ProxyServer::new(
                                        option.flag,
                                        option.username.clone(),
                                        option.password.clone(),
                                        option.udp_bind.clone(),
                                        Some(mapping.as_ref().unwrap().headers.clone()),
                                    );
                                    tokio::spawn(async move {
                                        // 处理代理的能力
                                        let _ = proxy_server.deal_proxy(stream).await;
                                    });
                                } else {
                                    if mapping.as_ref().unwrap().local_addr.is_none() {
                                        log::info!("本地地址为空，无法做内网映射");
                                        log::warn!("local addr is none, can't mapping");
                                        continue;
                                    }

                                    let domain = mapping.as_ref().unwrap().local_addr.unwrap();
                                    let sock_map = p.sock_map();
                                    let sender = sender.clone();
                                    tokio::spawn(async move {
                                        match HealthCheck::connect(&domain).await {
                                            Ok(tcp) => {
                                                let trans = TransStream::new(
                                                    tcp,
                                                    sock_map,
                                                    sender,
                                                    virtual_receiver,
                                                );
                                                let _ = trans.copy_wait().await;
                                            }
                                            Err(e) => {
                                                log::trace!(
                                                    "连接地址:{}，发生错误：{:?}",
                                                    domain,
                                                    e
                                                );
                                                let _ = sender
                                                    .send(ProtFrame::new_close(sock_map))
                                                    .await;
                                            }
                                        }
                                    });
                                }
                            }
                            ProtFrame::Data(_) => {
                                if let Some(sender) = map.get(&p.sock_map()) {
                                    let _ = sender.try_send(p);
                                }
                            }
                            ProtFrame::Close(p) => {
                                if p.sock_map() == 0 {
                                    log::warn!("客户端被服务端关闭:{}", p.reason());
                                } else if let Some(sender) = map.get(&p.sock_map()) {
                                    let _ = sender.try_send(ProtFrame::Close(p));
                                }
                            }
                            ProtFrame::Mapping(_) => {}
                            ProtFrame::Token(_) => todo!(),
                        }
                    }
                    None => {
                        break;
                    }
                }
            }
            if !read_buf.has_remaining() {
                read_buf.clear();
            }
        }
        if is_closed {
            for v in map {
                let _ = v.1.try_send(ProtFrame::Close(ProtClose::new(v.0)));
            }
        }
        Ok(())
    }

    pub async fn serve(&mut self) -> ProxyResult<()> {
        let tls_client = self.tls_client.clone();
        let server = self.server_addr.clone();
        let domain = self.domain.clone();
        let option = self.option.clone();

        let stream = self.stream.take();
        let tls_stream = self.tls_stream.take();
        let mut client_sender = self.sender.clone();
        let mut client_receiver = self.receiver.take().unwrap();
        let mut receiver_work = self.receiver_work.take().unwrap();
        let mut mappings = self.mappings.clone();
        tokio::spawn(async move {
            let mut stream = stream;
            let mut tls_stream = tls_stream;
            loop {
                if stream.is_some() {
                    let _ = Self::inner_serve(
                        &option,
                        stream.take().unwrap(),
                        &mut client_sender,
                        &mut receiver_work,
                        &mut client_receiver,
                        &mut mappings,
                    )
                    .await;
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                } else if tls_stream.is_some() {
                    let _ = Self::inner_serve(
                        &option,
                        tls_stream.take().unwrap(),
                        &mut client_sender,
                        &mut receiver_work,
                        &mut client_receiver,
                        &mut mappings,
                    )
                    .await;
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                };
                match Self::inner_connect(tls_client.clone(), server.clone(), domain.clone()).await
                {
                    Ok((s, tls)) => {
                        stream = s;
                        tls_stream = tls;
                    }
                    Err(_err) => {
                        tokio::time::sleep(Duration::from_millis(1000)).await;
                    }
                }
            }
        });

        Ok(())
    }

    fn calc_next_id(&self) -> u64 {
        let mut lock = NEXT_ID.lock().unwrap();
        let id = *lock;
        *lock = lock.wrapping_add(2);
        Helper::calc_sock_map(self.option.server_id, id)
    }

    pub async fn deal_new_stream<T>(&self, inbound: T) -> ProxyResult<()>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    {
        let id = self.calc_next_id();
        let sender = self.sender.clone();
        let (stream_sender, stream_receiver) = channel::<ProtFrame>(10);
        let _ = self
            .sender_work
            .send((ProtCreate::new(id, None), stream_sender))
            .await;
        tokio::spawn(async move {
            let trans = TransStream::new(inbound, id, sender, stream_receiver);
            let _ = trans.copy_wait().await;
        });
        Ok(())
    }
}
