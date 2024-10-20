use anyhow::Result;
use libdns::config::{GeneralConfigBuilder, RecordBuilder, RecordType, RunConfigBuilder};
use libdns::Server;
use maplit::hashmap;
use std::time::Duration;
use tokio::signal;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = RunConfigBuilder::default()
        .general(
            GeneralConfigBuilder::default()
                .listen_udp("127.0.0.1:53")
                .build()?,
        )
        .zones(hashmap! {
            "et.internal".to_string() => vec![
                RecordBuilder::default()
                .rr_type(RecordType::A)
                .name("www.et.internal".to_string())
                .value("123.123.123.123".to_string())
                .ttl(Duration::from_secs(60))
                .build()?
            ]
        })
        .build()?;

    let mut server = Server::new(config);
    server.run().await?;
    info!("Server listening on {}", server.udp_local_addr().unwrap());
    info!("Try `nslookup www.et.internal 127.0.0.1` in another terminal session");
    signal::ctrl_c().await?;
    info!("received SIGINT, shutting down...");
    server.shutdown().await?;
    Ok(())
}
