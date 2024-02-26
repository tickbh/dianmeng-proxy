// Copyright 2022 - 2024 Wenmeng See the COPYRIGHT
// file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
//
// Author: tickbh
// -----
// Created Date: 2024/01/16 10:59:37

use std::{
    fs::File,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::exit,
};

use bpaf::*;
use log::{Level, LevelFilter};
use webparse::{Request, Url};
use wenmeng::Client;

use crate::{
    option::proxy_config,
    reverse::{HttpConfig, LocationConfig, ServerConfig, UpstreamConfig},
    ConfigHeader, ConfigLog, ConfigOption, FileServer, ProxyConfig, ProxyResult,
};
use crate::{reverse::StreamConfig, WrapVecAddr};
use crate::{ConfigDuration, WrapAddr};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct Shared {
    /// 输入控制台的监听地址
    #[bpaf(
        fallback(WrapAddr(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8837))),
        display_fallback
    )]
    pub(crate) control: WrapAddr,
    /// 禁用默认输出
    pub(crate) disable_stdout: bool,
    /// 禁用控制微端
    pub(crate) disable_control: bool,
    /// 后台运行
    pub(crate) daemon: bool,
    /// 守护程序运行，正常退出结束
    pub(crate) forever: bool,
    /// 是否显示更多日志
    #[bpaf(short, long)]
    pub(crate) verbose: bool,
    /// 设置默认等级
    pub(crate) default_level: Option<LevelFilter>,
    /// 写入进程id文件
    #[bpaf(long, fallback("wmproxy.pid".to_string()))]
    pub(crate) pidfile: String,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct RunConfig {
    /// 配置文件路径
    #[bpaf(short, long)]
    pub(crate) config: String,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct CheckConfig {
    /// 配置文件路径
    #[bpaf(short, long)]
    pub(crate) config: String,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct StopConfig {
    /// 配置文件路径
    #[bpaf(short, long)]
    pub(crate) config: Option<String>,

    /// 控制微端地址
    #[bpaf(short, long)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct ReloadConfig {
    /// 配置文件路径
    #[bpaf(short, long)]
    pub(crate) config: Option<String>,

    /// 控制微端地址
    #[bpaf(short, long)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct FileServerConfig {
    /// 静态文件根目录路径
    #[bpaf(short, long, fallback(String::new()))]
    pub(crate) root: String,
    #[bpaf(
        short,
        long,
        fallback(WrapVecAddr(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8869)])),
        display_fallback
    )]
    /// 监听地址
    pub(crate) listen: WrapVecAddr,
    #[bpaf(long)]
    /// 监听地址
    pub(crate) listen_ssl: Option<WrapVecAddr>,
    /// ssl证书cert
    pub cert: Option<String>,
    /// ssl证书key
    pub key: Option<String>,
    /// 域名地址
    #[bpaf(short, long)]
    pub(crate) domain: Option<String>,
    /// 是否支持目录
    #[bpaf(short, long)]
    pub(crate) browse: bool,
    /// 设置robots.txt返回
    #[bpaf(long)]
    pub(crate) robots: Option<String>,
    /// 设置404文件返回
    #[bpaf(long)]
    pub(crate) path404: Option<String>,
    /// 设置robots.txt返回
    #[bpaf(short, long)]
    pub(crate) cache_time: Option<ConfigDuration>,
    /// 设置robots.txt返回
    #[bpaf(short, long)]
    pub(crate) ext_mimetype: Vec<String>,
    /// 通过"Access-Control-Allow-Origin"标头启用 CORS
    #[bpaf(long, fallback(false))]
    pub(crate) cors: bool,
    /// 头部信息修改如 "proxy x-forward-for {client_ip}"
    #[bpaf(short('H'), long)]
    pub(crate) header: Vec<ConfigHeader>,
    /// 访问日志放的位置如"logs/access.log trace"
    #[bpaf(long)]
    pub(crate) access_log: Option<String>,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct ReverseProxyConfig {
    /// 负载均衡来源地址
    #[bpaf(
        short,
        long,
        fallback(WrapVecAddr(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8869)])),
        display_fallback
    )]
    pub(crate) from: WrapVecAddr,
    /// 负载均衡来源地址SSL
    #[bpaf(
        long,
        fallback(WrapVecAddr(vec![])),
        display_fallback
    )]
    pub(crate) from_ssl: WrapVecAddr,
    /// 负载均衡映射地址
    #[bpaf(short, long)]
    pub(crate) to: WrapAddr,
    /// 头部信息修改如 "proxy x-forward-for {client_ip}"
    #[bpaf(short('H'), long)]
    pub(crate) header: Vec<ConfigHeader>,
    /// 访问日志放的位置如"logs/access.log trace"
    #[bpaf(long)]
    pub(crate) access_log: Option<String>,
    /// 是否映射到https上
    #[bpaf(long)]
    pub(crate) tls: bool,
    /// 证书cert
    #[bpaf(long)]
    pub(crate) cert: Option<String>,
    /// 证书key
    #[bpaf(long)]
    pub(crate) key: Option<String>,
    /// 是否支持websocket
    #[bpaf(long)]
    pub(crate) ws: bool,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct WsProxyConfig {
    /// 负载均衡来源地址
    #[bpaf(
        short,
        long,
        fallback(WrapVecAddr(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8869)])),
        display_fallback
    )]
    pub(crate) from: WrapVecAddr,
    /// 负载均衡映射地址
    #[bpaf(short, long)]
    pub(crate) to: WrapAddr,
    /// 访问日志放的位置如"logs/access.log trace"
    #[bpaf(long)]
    pub(crate) access_log: Option<String>,

    /// 当前代理的模式
    #[bpaf(long, argument("ws2tcp,tcp2ws,tcp2wss"))]
    pub(crate) mode: String,
    /// 当前域名
    #[bpaf(long)]
    pub(crate) domain: Option<String>,
    /// 是否支持websocket
    #[bpaf(long)]
    pub(crate) ws: bool,
}

#[derive(Debug, Clone, Bpaf)]
#[allow(dead_code)]
struct VersionConfig {}

#[derive(Debug, Clone)]
enum Command {
    Proxy(ProxyConfig),
    Run(RunConfig),
    Stop(StopConfig),
    Reload(ReloadConfig),
    Check(CheckConfig),
    FileServer(FileServerConfig),
    ReverseProxy(ReverseProxyConfig),
    WsProxy(WsProxyConfig),
    Version(VersionConfig),
}

fn parse_command() -> impl Parser<(Command, Shared)> {
    let run = run_config().map(Command::Run);
    let run = construct!(run, shared())
        .to_options()
        .command("run")
        .help("启动命令");

    let stop = stop_config().map(Command::Stop);
    let stop = construct!(stop, shared())
        .to_options()
        .command("stop")
        .help("关闭命令");

    let check = check_config().map(Command::Check);
    let check = construct!(check, shared())
        .to_options()
        .command("check")
        .help("检查配置是否正确");

    let reload = reload_config().map(Command::Reload);
    let reload = construct!(reload, shared())
        .to_options()
        .command("reload")
        .help("进行重载配置");

    let action = proxy_config().map(Command::Proxy);
    let action = construct!(action, shared())
        .to_options()
        .command("proxy")
        .help("代理及内网穿透相关功能");

    let file_config = file_server_config().map(Command::FileServer);
    let file_config = construct!(file_config, shared())
        .to_options()
        .command("file-server")
        .help("启动文件服务器");

    let reverse_config = reverse_proxy_config().map(Command::ReverseProxy);
    let reverse_config = construct!(reverse_config, shared())
        .to_options()
        .command("reverse-proxy")
        .help("启动负载均衡服务器");

    let ws_config = ws_proxy_config().map(Command::WsProxy);
    let ws_config = construct!(ws_config, shared())
        .to_options()
        .command("ws-proxy")
        .help("Websocket协议转发相关");

    let version_config = version_config().map(Command::Version);
    let version_config = construct!(version_config, shared())
        .to_options()
        .command("version")
        .help("打印当前版本号");
    construct!([
        action,
        run,
        stop,
        reload,
        check,
        file_config,
        reverse_config,
        ws_config,
        version_config
    ])
}

fn read_config_from_path(path: &String) -> ProxyResult<ConfigOption> {
    let path = PathBuf::from(path);
    let mut file = File::open(&path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let extension = path.extension().unwrap().to_string_lossy().to_string();
    let option = match &*extension {
        "yaml" => serde_yaml::from_str::<ConfigOption>(&contents).map_err(|e| {
            println!("解析文件错误: {}", e);
            io::Error::new(io::ErrorKind::Other, "parse yaml error")
        })?,
        "toml" => toml::from_str::<ConfigOption>(&contents).map_err(|e| {
            println!("解析文件错误: {}", e);
            io::Error::new(io::ErrorKind::Other, "parse toml error")
        })?,
        _ => {
            let e = io::Error::new(io::ErrorKind::Other, "unknow format error");
            return Err(e.into());
        }
    };
    Ok(option)
}

fn kill_process_by_id(id: String) -> Option<i32> {
    if id == String::new() {
        return Some(-1);
    }
    let child = if cfg!(target_os = "windows") {
        ::std::process::Command::new("taskkill")
            .args(["/f".to_string(), "/pid".to_string(), id.clone()])
            .output()
            .expect("failed to execute process")
    } else {
        ::std::process::Command::new("kill")
            .args(["-TERM".to_string(), id.clone()])
            .output()
            .expect("failed to execute process")
    };
    return child.status.code();
}

pub async fn parse_env() -> ProxyResult<ConfigOption> {
    let (command, shared) = parse_command().run();
    if shared.daemon && shared.forever {
        println!("daemon与forever不能同时被设置");
        exit(0);
    }
    if shared.daemon {
        let args = std::env::args()
            .filter(|s| s != "--daemon")
            .collect::<Vec<String>>();
        let mut command = std::process::Command::new(&args[0]);
        for value in &args[1..] {
            command.arg(&*value);
        }
        command.spawn().expect("failed to start wmproxy");
        exit(0);
    } else if shared.forever {
        let args = std::env::args()
            .filter(|s| s != "--forever")
            .collect::<Vec<String>>();
        loop {
            let mut command = std::process::Command::new(&args[0]);
            for value in &args[1..] {
                command.arg(&*value);
            }
            let mut child = command.spawn().expect("failed to start wmproxy");
            match child.wait() {
                Ok(ex) => {
                    if ex.success() {
                        exit(0);
                    }
                    log::error!("子进程异常退出：{}", ex);
                }
                Err(e) => log::error!("子进程异常退出：{:?}", e),
            }
        }
    }
    // let args = std::env::args().collect::<Vec<String>>();
    // let mut command = std::process::Command::new(&args[0]);
    // command.arg("run");
    // for value in &args[2..] {
    //     command.arg(&*value);
    // }
    // command.spawn().expect("failed to start wmproxy");
    // exit(0);
    let mut option = ConfigOption::default();
    option.default_level = shared.default_level;
    option.disable_control = shared.disable_control;
    option.disable_stdout = shared.disable_stdout;
    option.pidfile = shared.pidfile.clone();
    option.control = shared.control.0;
    if shared.verbose {
        option.default_level = Some(LevelFilter::Trace);
    }
    match command {
        Command::Proxy(proxy) => {
            option.proxy = Some(proxy);
            option.after_load_option()?;
            return Ok(option);
        }
        Command::Check(config) => match read_config_from_path(&config.config) {
            Ok(_) => {
                println!("配置文件正确");
                exit(0);
            }
            Err(e) => {
                println!("配置文件错误:{:?}", e);
                exit(0);
            }
        },
        Command::Run(config) => {
            let mut option = read_config_from_path(&config.config)?;
            if shared.verbose {
                option.default_level = Some(LevelFilter::Trace);
            }
            option.after_load_option()?;
            return Ok(option);
        }
        Command::Stop(config) => {
            let url = if let Some(config) = config.config {
                let option = read_config_from_path(&config)?;
                format!("http://{}", option.control)
            } else if let Some(url) = config.url {
                url
            } else {
                let mut file = File::open(shared.pidfile)?;
                let mut content = String::new();
                file.read_to_string(&mut content)?;
                exit(kill_process_by_id(content).unwrap_or(0));
                // println!("必须传入参数pidfile或者config或者url之一");
                // exit(0);
            };

            let mut url = Url::parse(url.into_bytes())?;
            url.path = "/stop".to_string();

            let req = Request::builder().method("GET").url(url.clone()).body("")?;
            println!("url = {:?}", req.get_connect_url());
            let client = Client::builder().http2(false).url(url)?.connect().await?;

            let (mut recv, _sender) = client.send2(req.into_type()).await?;
            let res = recv.recv().await.unwrap()?;
            if res.status() == 200 {
                println!("关闭成功!");
            } else {
                println!("微端响应:{}!", res.status());
            }
            exit(0);
        }

        Command::Reload(config) => {
            let url = if let Some(config) = config.config {
                let option = read_config_from_path(&config)?;
                format!("http://{}", option.control)
            } else if let Some(url) = config.url {
                url
            } else {
                println!("必须传入参数pidfile或者config或者url之一");
                exit(0);
            };

            let mut url = Url::parse(url.into_bytes())?;
            url.path = "/reload".to_string();

            let req = Request::builder().method("GET").url(url.clone()).body("")?;
            println!("url = {:?}", req.get_connect_url());
            let client = Client::builder().http2(false).url(url)?.connect().await?;

            let (mut recv, _sender) = client.send2(req.into_type()).await?;
            let res = recv.recv().await.unwrap()?;
            if res.status() == 200 {
                println!("重载文件成功!");
            } else {
                println!("重载文件失败: 微端响应:{}!", res.status());
            }
            exit(0);
        }
        Command::FileServer(file) => {
            let mut http = HttpConfig::new();
            let mut server = ServerConfig::new(file.listen.clone());
            if file.listen_ssl.is_some() {
                server.bind_ssl = file.listen_ssl.unwrap();
                if file.cert.is_none() || file.key.is_none() {
                    println!("配置ssl监听但未配置证书");
                    exit(0);
                }
                // if file.domain.is_none() {
                //     println!("配置ssl监听未配置域名");
                //     exit(0);
                // }
                server.cert = file.cert;
                server.key = file.key;
                server.comm.domain = file.domain;
            }
            
            let mut location = LocationConfig::new();
            let mut file_server = FileServer::new(file.root, "".to_string());
            file_server.robots = file.robots;
            file_server.cache_time = file.cache_time;
            file_server.cors = file.cors;
            file_server.path404 = file.path404;
            location.headers = file.header;
            location.file_server = Some(file_server);
            if let Some(access) = file.access_log {
                http.comm.access_log = Some(ConfigLog::new(
                    "access".to_string(),
                    "main".to_string(),
                    Level::Trace,
                ));
                http.comm.log_names.insert("access".to_string(), access);
            }
            server.location.push(location);
            http.server.push(server);
            option.http = Some(http);
            option.disable_control = true;
            option.after_load_option()?;
            return Ok(option);
        }
        Command::WsProxy(ws) => {
            let mut stream = StreamConfig::new();
            let mut server = ServerConfig::new(ws.from.clone());
            let up_name = "server".to_string();
            let upstream = UpstreamConfig::new_single(up_name.clone(), ws.to.0);
            server.up_name = up_name.to_string();
            let mode = ws.mode.to_ascii_lowercase();
            if mode != "ws2tcp" && mode != "tcp2ws" && mode != "tcp2wss" {
                println!("Websocket转发模式的mode必须为ws2tcp或者tcp2ws或者tcp2wss");
                exit(0);
            }
            server.bind_mode = ws.mode;
            stream.upstream.push(upstream);
            if let Some(access) = ws.access_log {
                server.comm.access_log = Some(ConfigLog::new(
                    "access".to_string(),
                    "main".to_string(),
                    Level::Trace,
                ));
                server.comm.log_names.insert("access".to_string(), access);
            }
            server.comm.domain = ws.domain;
            stream.server.push(server);
            option.stream = Some(stream);
            option.disable_control = true;
            option.after_load_option()?;
            return Ok(option);
        }
        Command::ReverseProxy(reverse) => {
            let mut http = HttpConfig::new();
            let mut server = ServerConfig::new(reverse.from.clone());
            server.bind_ssl = reverse.from_ssl;
            let mut location = LocationConfig::new();
            let up_name = "server".to_string();
            let upstream = UpstreamConfig::new_single(up_name.clone(), reverse.to.0);
            let url = if reverse.tls {
                let name = format!("https://{}", up_name);
                Url::parse(name.into_bytes())?
            } else {
                let name = format!("http://{}", up_name);
                Url::parse(name.into_bytes())?
            };
            server.cert = reverse.cert;
            server.key = reverse.key;
            location.comm.proxy_url = Some(url);
            location.headers = reverse.header;
            location.is_ws = reverse.ws;
            http.upstream.push(upstream);
            if let Some(access) = reverse.access_log {
                http.comm.access_log = Some(ConfigLog::new(
                    "access".to_string(),
                    "main".to_string(),
                    Level::Trace,
                ));
                http.comm.log_names.insert("access".to_string(), access);
            }
            server.location.push(location);
            http.server.push(server);
            option.http = Some(http);
            option.disable_control = true;
            option.after_load_option()?;
            return Ok(option);
        }
        Command::Version(_) => {
            println!("当前版本号:{}", VERSION);
            exit(0);
        }
    }
}
