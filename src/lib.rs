#![warn(rust_2018_idioms)]

use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use async_std::net::{ToSocketAddrs, UdpSocket};
use async_std::task;
use fasthash::murmur3;
use futures::{channel::mpsc, future, select, stream, FutureExt, SinkExt, StreamExt};
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

    /// Max udp paket size
    #[structopt(short, long, default_value = "1024")]
    pub mtu: usize,
}

pub async fn proxy(params: Serve) -> Result<()> {
    // todo: refactor graceful shutdown
    let nodes = prepare_statsd_hosts(params.statsd_host).await.unwrap();
    let addr = format!("{}:{}", params.host, params.port);

    let (sender, mut receiver) = mpsc::unbounded::<String>();
    let _listen_handle = task::spawn(listen(addr, sender));

    split_stream(nodes, receiver.borrow_mut(), params.host, params.mtu).await;

    Ok(())
}

async fn split_stream(
    nodes: Vec<StatsdNode>,
    metrics: &mut Receiver<String>,
    host: String,
    mtu: usize,
) {
    let mut ring: HashRing<(StatsdNode, i32), murmur3::Hash32> =
        HashRing::with_hasher(murmur3::Hash32);
    let mut sender_map: HashMap<StatsdNode, Sender<String>> = HashMap::new();
    let mut send_handlers = Vec::new();

    for node in nodes {
        let (sender, receiver) = mpsc::unbounded::<String>();
        sender_map.insert(node.clone(), sender);

        // insert fake nodes for a more even distribution
        for i in 0..100 {
            ring.add((node.clone(), i));
        }

        let handle = task::spawn(send_to_node(node, receiver, host.clone(), mtu));
        send_handlers.push(handle);
    }

    let mut metrics = metrics.fuse();

    while let Some(metric) = metrics.next().fuse().await {
        if let Some(key_size) = metric.find(':') {
            let key = &metric[..key_size];

            match ring.get(&key) {
                Some((n, _)) => {
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

    drop(sender_map);
    for handle in send_handlers {
        handle.await.unwrap()
    }
}

async fn send_to_node(
    node: StatsdNode,
    metrics: Receiver<String>,
    host: String,
    mtu: usize,
) -> Result<()> {
    let socket = UdpSocket::bind(format!("{}:0", host)).await?;
    let mut metrics = metrics.fuse();

    let flush_interval = Duration::from_millis(500);

    let mut buf: Vec<u8> = Vec::with_capacity(mtu * 2);

    loop {
        select! {
            metric = metrics.next().fuse() => match metric {
                Some(metric) => {
                    if metric.len() + buf.len() + 1 >= mtu {
                        node.send(&socket, &buf).await;

                        buf.clear();
                        buf.extend(metric.bytes());

                    } else {
                        buf.extend(metric.bytes());
                        buf.push(b'\n');
                    }
                },
                None => break,
            },
            _ = task::sleep(flush_interval).fuse() => {
                if !buf.is_empty() {
                    node.send(&socket, &buf).await;

                    buf.clear();
                }
            }
        }
    }

    node.send(&socket, &buf).await;

    Ok(())
}

async fn listen(addr: impl ToSocketAddrs, mut sender: Sender<String>) -> Result<()> {
    let socket = UdpSocket::bind(addr).await?;
    warn!("Listening on {}", socket.local_addr()?);

    read_from_socket(socket)
        .flat_map(move |chunk| {
            stream::iter(chunk.split('\n').map(str::to_owned).collect::<Vec<_>>())
        })
        .filter(|x| future::ready(!x.is_empty()))
        .take_while(|x| future::ready(x != "<stop>"))
        .map(Ok)
        .forward(&mut sender)
        .await
        .unwrap();

    Ok(())
}

async fn prepare_statsd_hosts(hosts: Vec<Host>) -> std::result::Result<Vec<StatsdNode>, String> {
    if hosts.is_empty() {
        return Err("Statsd hosts should not be empty".to_string());
    };

    let resolver = Resolver::from_system_conf().unwrap();

    let mut nodes = vec![];

    for host in hosts {
        warn!("Resolving {:?}", &host);
        let response = resolver.lookup_ip(host.host).map_err(|e| e.to_string())?;
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

    async fn send(&self, socket: &UdpSocket, buf: &[u8]) {
        debug!(
            "Send to {} -> {:?}",
            self.addrs,
            String::from_utf8_lossy(&buf)
        );
        let _ = socket
            .send_to(buf, self.addrs)
            .await
            .map_err(|e| error!("Error when send stats to {:?} Err: {:?}", &self, e));
    }
}

impl ToString for StatsdNode {
    fn to_string(&self) -> String {
        format!("{}:{}", self.addrs.ip(), self.addrs.port())
    }
}

fn read_from_socket(s: UdpSocket) -> impl StreamExt<Item = String> {
    stream::unfold(s, |s| async {
        let data = read_chunk(&s).await;
        Some((data, s))
    })
}

async fn read_chunk(s: &UdpSocket) -> String {
    let mut buf = vec![0; 1024 * 5];
    let (recv, _) = s
        .recv_from(&mut buf)
        .await
        .expect("Error when read form udp socket");

    let data = String::from_utf8_lossy(&buf[..recv]).into_owned();
    debug!("Received {:?}", data);
    return data;
}
