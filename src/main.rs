#![warn(rust_2018_idioms)]

use log::warn;
use structopt::StructOpt;

use statsd_proxy::{proxy, Result, Serve};

#[derive(Debug, StructOpt)]
pub enum Command {
    #[structopt(name = "serve")]
    Serve(Serve),
}

#[derive(Debug, StructOpt)]
#[structopt(name = "classify")]
pub struct ApplicationArguments {
    #[structopt(subcommand)]
    pub command: Command,
}

#[async_std::main]
async fn main() -> Result<()> {
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
