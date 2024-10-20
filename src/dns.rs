use crate::config;
use crate::config::GeneralConfig;
use anyhow::Result;
use hickory_proto::op::Edns;
use hickory_proto::rr;
use hickory_proto::rr::LowerName;
use hickory_server::authority::{AuthorityObject, Catalog, ZoneType};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::store::in_memory::InMemoryAuthority;
use hickory_server::ServerFuture;
use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub struct Server {
    server: ServerFuture<CatalogRequestHandler>,
    catalog: Arc<RwLock<Catalog>>,
    general_config: GeneralConfig,
    udp_local_addr: Option<SocketAddr>,
}

struct CatalogRequestHandler {
    catalog: Arc<RwLock<Catalog>>,
}

impl CatalogRequestHandler {
    fn new(catalog: Arc<RwLock<Catalog>>) -> CatalogRequestHandler {
        Self { catalog }
    }
}

#[async_trait::async_trait]
impl RequestHandler for CatalogRequestHandler {
    async fn handle_request<R: ResponseHandler>(
        &self,
        request: &Request,
        response_handle: R,
    ) -> ResponseInfo {
        self.catalog
            .read()
            .await
            .handle_request(request, response_handle)
            .await
    }
}

impl Server {
    pub fn new(config: config::RunConfig) -> Self {
        Self::try_new(config).unwrap()
    }

    fn try_new(config: config::RunConfig) -> Result<Self> {
        let mut catalog = Catalog::new();
        for (domain, records) in config.zones().iter() {
            let zone = rr::Name::from_str(domain.as_str())?;
            let mut authorities = InMemoryAuthority::empty(zone.clone(), ZoneType::Primary, false);
            for record in records.iter() {
                let r = record.try_into()?;
                authorities.upsert_mut(r, 0);
            }
            catalog.upsert(zone.clone().into(), Box::new(Arc::new(authorities)));
        }

        let catalog = Arc::new(RwLock::new(catalog));
        let handler = CatalogRequestHandler::new(catalog.clone());
        let server = ServerFuture::new(handler);
        Ok(Self {
            server,
            catalog,
            general_config: config.general().clone(),
            udp_local_addr: None,
        })
    }

    pub fn udp_local_addr(&mut self) -> Option<SocketAddr> {
        self.udp_local_addr
    }

    pub async fn run(&mut self) -> Result<()> {
        if let Some(address) = self.general_config.listen_udp() {
            let socket = UdpSocket::bind(address).await?;
            self.udp_local_addr = Some(socket.local_addr()?);
            self.server.register_socket(socket);
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.server.shutdown_gracefully().await?;
        Ok(())
    }

    pub async fn upsert(&self, name: LowerName, authority: Box<dyn AuthorityObject>) {
        self.catalog.write().await.upsert(name, authority);
    }

    pub async fn remove(&self, name: &LowerName) -> Option<Box<dyn AuthorityObject>> {
        self.catalog.write().await.remove(name)
    }

    pub async fn update<R: ResponseHandler>(
        &self,
        update: &Request,
        response_edns: Option<Edns>,
        response_handle: R,
    ) -> io::Result<ResponseInfo> {
        self.catalog
            .write()
            .await
            .update(update, response_edns, response_handle)
            .await
    }

    pub async fn contains(&self, name: &LowerName) -> bool {
        self.catalog.read().await.contains(name)
    }

    pub async fn lookup<R: ResponseHandler>(
        &self,
        request: &Request,
        response_edns: Option<Edns>,
        response_handle: R,
    ) -> ResponseInfo {
        self.catalog
            .read()
            .await
            .lookup(request, response_edns, response_handle)
            .await
    }

    pub async fn read_catalog(&self) -> RwLockReadGuard<'_, Catalog> {
        self.catalog.read().await
    }

    pub async fn write_catalog(&self) -> RwLockWriteGuard<'_, Catalog> {
        self.catalog.write().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GeneralConfigBuilder, RecordBuilder, RecordType, RunConfigBuilder};
    use anyhow::Result;
    use hickory_client::client::{AsyncClient, ClientHandle};
    use hickory_proto::rr;
    use hickory_proto::udp::UdpClientStream;
    use maplit::hashmap;
    use std::time::Duration;

    #[tokio::test]
    async fn it_works() -> Result<()> {
        let mut server = Server::new(
            RunConfigBuilder::default()
                .general(GeneralConfigBuilder::default().build()?)
                .build()?,
        );
        server.run().await?;
        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn can_resolve_records() -> Result<()> {
        let configured_record = RecordBuilder::default()
            .rr_type(RecordType::A)
            .name("www.et.internal".to_string())
            .value("123.123.123.123".to_string())
            .ttl(Duration::from_secs(60))
            .build()?;
        let config = RunConfigBuilder::default()
            .general(
                GeneralConfigBuilder::default()
                    .listen_udp("127.0.0.1:0")
                    .build()?,
            )
            .zones(hashmap! {
                "et.internal".to_string() => vec![configured_record.clone()],
            })
            .build()?;
        
        let mut server = Server::new(config);
        server.run().await?;

        let local_addr = server.udp_local_addr().unwrap();
        let stream = UdpClientStream::<UdpSocket>::with_timeout(local_addr, Duration::from_secs(5));
        let (mut client, background) = AsyncClient::connect(stream).await?;
        let background_task = tokio::spawn(background);
        let response = client
            .query(
                rr::Name::from_str("www.et.internal")?,
                rr::DNSClass::IN,
                rr::RecordType::A,
            )
            .await?;
        drop(background_task);

        assert_eq!(response.answers().len(), 1);
        let expected_record: rr::Record = configured_record.try_into()?;
        assert_eq!(response.answers().first().unwrap(), &expected_record);

        server.shutdown().await?;
        Ok(())
    }
}
