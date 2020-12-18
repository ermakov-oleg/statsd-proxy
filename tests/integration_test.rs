use async_std::net::UdpSocket;
use async_std::task;
use statsd_proxy::{proxy, Host, Serve};
use std::time::Duration;

#[async_std::test]
async fn test_proxy() -> std::io::Result<()> {
    env_logger::init();

    let proxy_handle = task::spawn(proxy(Serve {
        host: "0.0.0.0".to_string(),
        port: 18125,
        statsd_host: vec![
            Host {
                host: "127.0.0.1".to_string(),
                port: 8125,
            },
            Host {
                host: "127.0.0.1".to_string(),
                port: 8126,
            },
            Host {
                host: "127.0.0.1".to_string(),
                port: 8127,
            },
        ],
    }));

    let server1 = task::spawn(listen(8125));
    let server2 = task::spawn(listen(8126));
    let server3 = task::spawn(listen(8127));

    task::sleep(Duration::from_secs(1)).await;

    for i in 0..50_000 {
        send_to_socket(18125, format!("some_key_{}:-100|c", i).as_bytes()).await;
    }
    send_to_socket(18125, "<stop>".as_ref()).await;

    let _ = proxy_handle.await;

    send_to_socket(8125, "<stop>".as_ref()).await;
    send_to_socket(8126, "<stop>".as_ref()).await;
    send_to_socket(8127, "<stop>".as_ref()).await;

    let results1 = server1.await;
    let results2 = server2.await;
    let results3 = server3.await;

    assert_eq!(results1.len() + results2.len() + results3.len(), 50_000);

    assert_eq!(results1.len(), 11988);
    assert_eq!(results2.len(), 21179);
    assert_eq!(results3.len(), 16833);

    Ok(())
}

async fn send_to_socket(port: u16, buf: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket
        .send_to(buf, format!("127.0.0.1:{}", port))
        .await
        .unwrap();
}

async fn listen(port: u16) -> Vec<String> {
    let socket = UdpSocket::bind(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let mut buf = vec![0u8; 1024 * 5];
    let mut data: Vec<String> = vec![];

    let mut stop = false;

    while !stop {
        let (recv, _) = socket.recv_from(&mut buf).await.unwrap();
        let chunk = String::from_utf8_lossy(&buf[..recv]);
        for metric in chunk.split('\n').filter(|&x| !x.is_empty()) {
            if metric.eq("<stop>") {
                stop = true
            } else {
                data.push(metric.parse().unwrap())
            }
        }
    }

    data
}
