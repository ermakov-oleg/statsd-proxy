#![warn(rust_2018_idioms)]

use std::net::SocketAddr;
use std::str::FromStr;

use async_std::net::UdpSocket;
use hash_ring::HashRing;
use log::{debug, error, warn};
use structopt::StructOpt;
use trust_dns_resolver::Resolver;

#[async_std::main]
async fn main() -> Result<(), std::io::Error> {
    env_logger::init();
    let opt = ApplicationArguments::from_args();

    match opt.command {
        Command::Serve(params) => {
            warn!("{:?}", params);
            proxy(params).await?
        }
    };

    Ok(())
}

#[derive(Debug, StructOpt)]
struct Serve {
    /// Run on host
    #[structopt(short, long, default_value = "127.0.0.1")]
    host: String,

    /// Listen port
    #[structopt(short, long, default_value = "8125")]
    port: u16,

    /// List of statsd hosts (host:port)
    #[structopt(short, long)]
    statsd_host: Vec<String>,
}

#[derive(Debug, StructOpt)]
enum Command {
    #[structopt(name = "serve")]
    Serve(Serve),
}

#[derive(Debug, StructOpt)]
#[structopt(name = "classify")]
struct ApplicationArguments {
    #[structopt(subcommand)]
    command: Command,
}

struct Host {
    host: String,
    port: u16,
}

impl FromStr for Host {
    type Err = String;

    fn from_str(s: &str) -> Result<Host, Self::Err> {
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

async fn prepare_statsd_hosts(hosts: Vec<String>) -> Result<Vec<StatsdNode>, String> {
    let resolver = Resolver::default().unwrap();

    let mut nodes = vec![];

    for host_raw in hosts {
        let host: Host = host_raw.parse()?;
        let response = resolver.lookup_ip(&host.host).map_err(|e| e.to_string())?;
        warn!("Resolving {}", &host_raw);
        for addr in response {
            warn!("    {:?}", addr);
            nodes.push(StatsdNode::new(SocketAddr::from((addr, host.port))));
        }
    }

    Ok(nodes)
}

#[derive(Debug, Clone)]
struct StatsdNode {
    addrs: SocketAddr,
}

impl ToString for StatsdNode {
    fn to_string(&self) -> String {
        format!("{}:{}", self.addrs.ip(), self.addrs.port())
    }
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

async fn proxy(params: Serve) -> std::io::Result<()> {
    let nodes = prepare_statsd_hosts(params.statsd_host).await.unwrap();

    let mut hash_ring = HashRing::new(nodes, 256);

    let socket = UdpSocket::bind(format!("{}:{}", params.host, params.port)).await?;
    warn!("Listening on {}", socket.local_addr()?);

    let mut buf = vec![0u8; 1024 * 5];

    loop {
        let (recv, _) = socket.recv_from(&mut buf).await?;
        let chunk = String::from_utf8_lossy(&buf[..recv]);
        for metric in chunk.split('\n').filter(|&x| !x.is_empty()) {
            if let Some(key_size) = metric.find(':') {
                let key = &metric[..key_size];
                let node = hash_ring.get_node(key.parse().unwrap()).unwrap();
                node.send(&socket, metric.as_bytes())
                    .await
                    .unwrap_or_else(|e| error!("Error when send stats to {:?} Err: {:?}", node, e));

                debug!("[node] {:?} | key -> {:?}", node, key,)
            } else {
                debug!("Invalid metric format {:?}", metric)
            }
        }
    }
}
