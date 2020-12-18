#![warn(rust_2018_idioms)]

use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::SocketAddr;
use std::str::FromStr;

use async_std::net::{ToSocketAddrs, UdpSocket};
use async_std::task;
use fasthash::murmur3;
use futures::{channel::mpsc, FutureExt, SinkExt, StreamExt};
use hashring::HashRing;
use log::{debug, error, warn};
use structopt::StructOpt;
use trust_dns_resolver::Resolver;

// todo: refactor errors
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

#[derive(Debug, StructOpt)]
pub struct Serve {
    /// Run on host
    #[structopt(short, long, default_value = "127.0.0.1")]
    pub host: String,

    /// Listen port
    #[structopt(short, long, default_value = "8125")]
    pub port: u16,

    /// List of statsd hosts (host:port)
    #[structopt(short, long)]
    pub statsd_host: Vec<Host>,
}

pub async fn proxy(params: Serve) -> Result<()> {
    // todo: graceful shutdown
    let nodes = prepare_statsd_hosts(params.statsd_host).await.unwrap();

    let (sender, mut receiver) = mpsc::unbounded::<String>();
    let _listen_handle = task::spawn(listen(format!("{}:{}", params.host, params.port), sender));

    split_stream(nodes, receiver.borrow_mut()).await;

    Ok(())
}

async fn split_stream(nodes: Vec<StatsdNode>, metrics: &mut Receiver<String>) {
    let mut ring: HashRing<StatsdNode, murmur3::Hash32> = HashRing::with_hasher(murmur3::Hash32);
    let mut sender_map: HashMap<StatsdNode, Sender<String>> = HashMap::new();

    for node in nodes {
        let (sender, receiver) = mpsc::unbounded::<String>();
        sender_map.insert(node.clone(), sender);
        ring.add(node.clone());

        let _handle = task::spawn(send_to_node(node, receiver));
    }

    let mut metrics = metrics.fuse();

    while let Some(metric) = metrics.next().fuse().await {
        if let Some(key_size) = metric.find(':') {
            let key = &metric[..key_size];

            match ring.get(&key) {
                Some(n) => {
                    debug!("[node] {:?} | key -> {:?}", n, &key);

                    match sender_map.get(n) {
                        Some(mut sender) => sender.send(metric).await.unwrap(),
                        None => error!("Sender not found"),
                    };
                }
                _ => error!("Node not found"),
            }
        } else {
            debug!("Invalid metric format {:?}", metric)
        }
    }
}

async fn send_to_node(node: StatsdNode, metrics: Receiver<String>) -> Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let mut metrics = metrics.fuse();

    loop {
        match metrics.next().fuse().await {
            Some(metric) => {
                node.send(&socket, metric.as_bytes())
                    .await
                    .unwrap_or_else(|e| error!("Error when send stats to {:?} Err: {:?}", node, e));
            }
            None => return Ok(()),
        }
    }
}

async fn listen(addr: impl ToSocketAddrs, mut sender: Sender<String>) -> Result<()> {
    let socket = UdpSocket::bind(addr).await?;
    warn!("Listening on {}", socket.local_addr()?);

    let mut buf = vec![0u8; 1024 * 5];

    // todo: make correct stop
    'outer: loop {
        let (recv, _) = socket.recv_from(&mut buf).await?;
        let chunk = String::from_utf8_lossy(&buf[..recv]);
        for metric in chunk.split('\n').filter(|&x| !x.is_empty()) {
            if metric.eq("<stop>") {
                break 'outer;
            }

            debug!("Received {:?}", metric);
            sender.send(metric.parse().unwrap()).await?;
        }
    }

    Ok(())
}

async fn prepare_statsd_hosts(hosts: Vec<Host>) -> std::result::Result<Vec<StatsdNode>, String> {
    if hosts.is_empty() {
        return Err("Statsd hosts should not be empty".to_string());
    };

    let resolver = Resolver::from_system_conf().unwrap();

    let mut nodes = vec![];

    for host in hosts {
        let response = resolver.lookup_ip(&host.host).map_err(|e| e.to_string())?;
        warn!("Resolving {:?}", &host);
        for addr in response {
            warn!("    {:?}", addr);
            nodes.push(StatsdNode::new(SocketAddr::from((addr, host.port))));
        }
    }

    Ok(nodes)
}

#[derive(Debug)]
pub struct Host {
    pub host: String,
    pub port: u16,
}

impl FromStr for Host {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Host, Self::Err> {
        let ss = s.to_string();
        let parts: Vec<&str> = ss.split(':').collect();
        match *parts.as_slice() {
            [host, port] => Ok(Host {
                host: host.to_string(),
                port: port.parse().expect("Invalid port"),
            }),
            _ => Err(String::from("Invalid host format")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StatsdNode {
    addrs: SocketAddr,
}

impl StatsdNode {
    fn new(addrs: SocketAddr) -> Self {
        Self { addrs }
    }

    async fn send(&self, socket: &UdpSocket, buf: &[u8]) -> std::io::Result<()> {
        socket.send_to(buf, self.addrs).await?;
        Ok(())
    }
}

impl ToString for StatsdNode {
    fn to_string(&self) -> String {
        format!("{}:{}", self.addrs.ip(), self.addrs.port())
    }
}
