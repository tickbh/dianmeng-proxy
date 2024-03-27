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
// Created Date: 2023/10/25 03:36:36

use std::{io, sync::Arc};

use crate::{arg, core::{AppTrait, ShutdownWatch, Stream}, ConfigOption, Helper, ProxyApp, ProxyResult, WMCore};
use async_trait::async_trait;
use tokio::{
    net::TcpListener,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
};
use webparse::{HeaderName, Request, Response};
use wenmeng::{Body, HttpTrait, ProtResult, RecvRequest, RecvResponse, Server};

/// 控制端，可以对配置进行热更新
pub struct ControlApp {
    /// 控制端当前的配置文件，如果部分修改将直接修改数据进行重启
    option: ConfigOption,
    /// 通知服务进行关闭的Sender，服务相关如果收到该消息则停止Accept
    server_sender_close: Option<Sender<()>>,
    /// 通知中心服务的Sender，每个服务拥有一个该Sender，可反向通知中控关闭
    control_sender_close: Sender<()>,
    /// 通知中心服务的Receiver，收到一次则将当前的引用计数-1，如果为0则表示需要关闭服务器
    control_receiver_close: Option<Receiver<()>>,
    /// 服务的引用计数
    count: i32,
}

struct Operate {
    control: Arc<Mutex<ControlApp>>,
}
#[async_trait]
impl HttpTrait for Operate {
    async fn operate(&mut self, req: &mut RecvRequest) -> ProtResult<RecvResponse> {
        // body的内容可能重新解密又再重新再加过密, 后续可考虑直接做数据
        let mut value = ControlApp::inner_operate(req, &mut self.control).await?;
        value.headers_mut().insert("server", "wmproxy");
        Ok(value)
    }
}

impl ControlApp {
    pub fn new(option: ConfigOption) -> Self {
        let (sender, receiver) = channel::<()>(1);
        Self {
            option,
            server_sender_close: None,
            control_sender_close: sender,
            control_receiver_close: Some(receiver),
            count: 0,
        }
    }

    pub async fn start_serve(mut self) -> ProxyResult<()> {
        let option = self.option.clone();
        self.inner_start_server(option).await?;
        Self::start_control(Arc::new(Mutex::new(self))).await?;
        Ok(())
    }

    pub async fn do_restart_serve(&mut self) -> ProxyResult<()> {
        let option = arg::parse_env()?;
        Helper::try_init_log(&option);
        self.inner_start_server(option).await?;
        Ok(())
    }

    async fn inner_start_server(&mut self, option: ConfigOption) -> ProxyResult<()> {
        let sender = self.control_sender_close.clone();
        let (sender_no_listen, receiver_no_listen) = channel::<()>(1);
        let sender_close = self.server_sender_close.take();
        // 每次启动的时候将让控制计数+1
        self.count += 1;
        tokio::spawn(async move {
            let mut proxy = WMCore::new(option);
            // 将上一个进程的关闭权限交由下一个服务，只有等下一个服务准备完毕的时候才能关闭上一个服务
            // if let Err(e) = proxy.start_serve(receiver_no_listen, sender_close).await {
            //     log::info!("处理失败服务进程失败: {:?}", e);
            // }
            // 每次退出的时候将让控制计数-1，减到0则退出
            let _ = sender.send(()).await;
        });
        self.server_sender_close = Some(sender_no_listen);
        Ok(())
    }

    async fn inner_operate(
        req: &mut Request<Body>,
        data: &mut Arc<Mutex<ControlApp>>,
    ) -> ProtResult<Response<Body>> {
        let mut value = data.lock().await;
        match &**req.path() {
            "/reload" => {
                // 将重新启动服务器
                let _ = value.do_restart_serve().await;
                return Ok(Response::text()
                    .body("重新加载配置成功")
                    .unwrap()
                    .into_type());
            }
            "/stop" => {
                // 通知控制端关闭，控制端阻塞主线程，如果控制端退出后进程退出
                if let Some(sender) = &value.server_sender_close {
                    let _ = sender.send(()).await;
                }
                return Ok(Response::text().body("关闭进程成功").unwrap().into_type());
            }
            "/now" => {
                if let Ok(data) = serde_json::to_string_pretty(&value.option) {
                    return Ok(Response::text()
                        .header(HeaderName::CONTENT_TYPE, "application/json; charset=utf-8")
                        .body(data)
                        .unwrap()
                        .into_type());
                }
            }
            _ => {}
        };
        if req.path() == "/reload" {}

        if req.path() == "/stop" {
            // 通知控制端关闭，控制端阻塞主线程，如果控制端退出后进程退出
            if let Some(sender) = &value.server_sender_close {
                let _ = sender.send(()).await;
            }
            return Ok(Response::text().body("关闭进程成功").unwrap().into_type());
        }

        return Ok(Response::status503()
            .body("请选择您要的操作")
            .unwrap()
            .into_type());
    }

    async fn receiver_await(receiver: &mut Option<Receiver<()>>) -> Option<()> {
        if receiver.is_some() {
            receiver.as_mut().unwrap().recv().await
        } else {
            let pend = std::future::pending();
            let () = pend.await;
            None
        }
    }

    pub async fn start_control(control: Arc<Mutex<ControlApp>>) -> ProxyResult<()> {
        let listener = {
            let value = &mut control.lock().await;
            if value.option.disable_control {
                let mut receiver = value.control_receiver_close.take();
                let _ = Self::receiver_await(&mut receiver).await;
                return Ok(());
            }
            log::info!("控制端口绑定：{:?}，提供中控功能。", value.option.control);
            match TcpListener::bind(value.option.control).await {
                Ok(tcp) => tcp,
                Err(_) => {
                    log::info!("控制端口绑定失败：{}，请配置不同端口。", value.option.control);
                    let pending = std::future::pending();
                    let () = pending.await;
                    return Ok(());
                }
            }
        };

        loop {
            let mut receiver = {
                let value = &mut control.lock().await;
                value.control_receiver_close.take()
            };

            tokio::select! {
                Ok((conn, addr)) = listener.accept() => {
                    log::info!("控制端口请求：{:?}，开始处理。", addr);
                    let cc = control.clone();
                    tokio::spawn(async move {
                        let mut server = Server::new(conn, Some(addr));
                        server.set_callback_http(Box::new(Operate {
                            control: cc
                        }));
                        if let Err(e) = server.incoming().await {
                            log::info!("控制中心：处理信息时发生错误：{:?}", e);
                        }
                    });
                    let value = &mut control.lock().await;
                    value.control_receiver_close = receiver;
                }
                _ = Self::receiver_await(&mut receiver) => {
                    let value = &mut control.lock().await;
                    value.count -= 1;
                    log::info!("反向代理：控制端收到关闭信号，当前:{}", value.count);
                    if value.count <= 0 {
                        break;
                    }
                    value.control_receiver_close = receiver;
                }
            }
        }
        Ok(())
    }
}


#[async_trait]
impl AppTrait for ControlApp {
    async fn process_new(
        self: &Arc<Self>,
        session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {

        // loop {
        //     let mut receiver = {
        //         let value = &mut control.lock().await;
        //         value.control_receiver_close.take()
        //     };

        //     tokio::select! {
        //         Ok((conn, addr)) = listener.accept() => {
        //             log::info!("控制端口请求：{:?}，开始处理。", addr);
        //             let cc = control.clone();
        //             tokio::spawn(async move {
        //                 let mut server = Server::new(conn, Some(addr));
        //                 server.set_callback_http(Box::new(Operate {
        //                     control: cc
        //                 }));
        //                 if let Err(e) = server.incoming().await {
        //                     log::info!("控制中心：处理信息时发生错误：{:?}", e);
        //                 }
        //             });
        //             let value = &mut control.lock().await;
        //             value.control_receiver_close = receiver;
        //         }
        //         _ = Self::receiver_await(&mut receiver) => {
        //             let value = &mut control.lock().await;
        //             value.count -= 1;
        //             log::info!("反向代理：控制端收到关闭信号，当前:{}", value.count);
        //             if value.count <= 0 {
        //                 break;
        //             }
        //             value.control_receiver_close = receiver;
        //         }
        //     }
        // }
        None
    }

    async fn ready_init(&mut self) -> io::Result<()> {
        Ok(())
    }
}
