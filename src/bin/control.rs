use anyhow::{Context, Result};
use clap::{App, Arg};

use revsh::control::Control;

#[tokio::main]
async fn main() -> Result<()> {
    // parse arguments
    let matches = App::new("revsh-rs control")
        .arg(
            Arg::with_name("keyfile")
                .required(true)
                .long("keyfile")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("listen-addr")
                .required(true)
                .long("listen-addr")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("proxy-addr")
                .long("proxy-addr")
                .takes_value(true),
        )
        .get_matches();

    let proxy_addr = match matches.value_of("proxy-addr") {
        Some(proxy_addr) => Some(proxy_addr.parse()?),
        _ => None,
    };
    let keyfile = matches.value_of("keyfile").context("error")?;
    let listen_addr = matches.value_of("listen-addr").context("error")?;

    // start listener
    let mut control = Control::new(listen_addr.parse()?, keyfile)
        .await?
        .shell("/bin/sh".to_string())
        .env(vec![
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        ])
        .proxy(proxy_addr);

    // accept
    while let Err(e) = control.accept().await {
        println!("accept failed: {}", e);
    }

    // run broker
    control.broker().await?;

    Ok(())
}
