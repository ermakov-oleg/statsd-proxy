use async_std::net::UdpSocket;
use async_std::task;
use rand::Rng;
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

    task::sleep(Duration::from_millis(500)).await;

    let mut send_tasks = vec![];

    let num_tasks = 3;
    let metrics_count = 30_000;

    for _ in 0..num_tasks {
        let handle = task::spawn(async move {
            for i in 0..metrics_count {
                send_to_socket(18125, format!("some_key_{}:-100|c", i).as_bytes()).await;
                let num = rand::thread_rng().gen_range(0..100);
                if num < 10 {
                    task::sleep(Duration::from_millis(num)).await;
                };
            }
        });
        send_tasks.push(handle);
    }

    for handle in send_tasks {
        handle.await
    }

    send_to_socket(18125, "<stop>".as_ref()).await;

    let _ = proxy_handle.await;

    task::sleep(Duration::from_secs(1)).await;

    send_to_socket(8125, "<stop>".as_ref()).await;
    send_to_socket(8126, "<stop>".as_ref()).await;
    send_to_socket(8127, "<stop>".as_ref()).await;

    let count: u64 = futures::future::join_all(vec![server1, server2, server3])
        .await
        .iter()
        .sum();

    assert_eq!(count, metrics_count * num_tasks);
    Ok(())
}

async fn send_to_socket(port: u16, buf: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket
        .send_to(buf, format!("127.0.0.1:{}", port))
        .await
        .unwrap();
}

async fn listen(port: u16) -> u64 {
    let socket = UdpSocket::bind(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let mut buf = vec![0u8; 1024 * 5];
    let mut count: u64 = 0;

    let mut stop = false;

    while !stop {
        let (recv, _) = socket.recv_from(&mut buf).await.unwrap();
        if &buf[..recv] == b"<stop>" {
            stop = true
        } else {
            count += &buf[..recv]
                .iter()
                .fold(0, |acc, &x| if x == b':' { acc + 1 } else { acc });
        }
    }

    count
}
