# Simple proxy for statsd

Used a consistent hash of the metric key to balance metrics across backends.  

```bash
USAGE:
    statsd-proxy serve [OPTIONS]

FLAGS:
        --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -h, --host <host>                     Run on host [default: 127.0.0.1]
    -p, --port <port>                     Listen port [default: 8125]
    -s, --statsd-host <statsd-host>...    List of statsd hosts (host:port)
```