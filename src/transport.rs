//! RFC 6120 connection lifecycle using upstream TLS, SASL, XML and stanza code.
//! We retain failures/cancellation explicitly: tokio-xmpp's high-level client
//! currently hides failed authentication behind its internal reconnect loop.
use crate::model::Account;
use futures_util::{SinkExt, StreamExt};
use sasl::common::Credentials;
use std::{borrow::Cow, future::Future, io, pin::Pin, time::Duration};
use tokio_xmpp::{
    Error,
    connect::{
        AsyncReadAndWrite, DirectTlsServerConnector, DnsConfig, ServerConnector,
        StartTlsServerConnector,
    },
    parsers::{
        bind::{BindQuery, BindResponse},
        iq::Iq,
        stanza::Stanza,
    },
    xmlstream::{
        FallibleStreamElement, ReadError, StreamHeader, Timeouts, XmppStream, XmppStreamElement,
    },
};

type Stream = XmppStream<Box<dyn AsyncReadAndWrite + Send>>;
type Connecting = Pin<Box<dyn Future<Output = Result<Stream, Error>>>>;
pub enum Event {
    Online,
    Disconnected(Error),
    Stanza(Stanza),
}
pub struct Client {
    config: Account,
    password: String,
    stream: Option<Stream>,
    connecting: Option<Connecting>,
    delay: Duration,
}
impl Client {
    pub fn new(config: Account, password: String) -> Self {
        Self {
            config,
            password,
            stream: None,
            connecting: None,
            delay: Duration::ZERO,
        }
    }
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            if self.stream.is_none() {
                if self.connecting.is_none() {
                    let config = self.config.clone();
                    let password = self.password.clone();
                    let delay = self.delay;
                    self.connecting = Some(Box::pin(async move {
                        tokio::time::sleep(delay).await;
                        tokio::time::timeout(Duration::from_secs(40), connect(config, password))
                            .await
                            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
                    }));
                }
                let result = self.connecting.as_mut().unwrap().await;
                self.connecting.take();
                match result {
                    Ok(stream) => {
                        self.stream = Some(stream);
                        self.delay = Duration::ZERO;
                        return Some(Event::Online);
                    }
                    Err(error) => {
                        self.backoff();
                        return Some(Event::Disconnected(error));
                    }
                }
            }
            match self.stream.as_mut().unwrap().next().await {
                Some(Ok(FallibleStreamElement::Ok(XmppStreamElement::Stanza(stanza)))) => {
                    return Some(Event::Stanza(stanza));
                }
                Some(Ok(FallibleStreamElement::Ok(XmppStreamElement::StreamError(error)))) => {
                    self.stream.take();
                    self.backoff();
                    return Some(Event::Disconnected(Error::StreamError(error)));
                }
                Some(Err(ReadError::SoftTimeout)) => {
                    // XEP-0199 keeps an idle connection observable without read receipts.
                    let ping = Iq::from_get("keepalive", tokio_xmpp::parsers::ping::Ping).into();
                    if let Err(error) = self.send_stanza(ping).await {
                        self.stream.take();
                        self.backoff();
                        return Some(Event::Disconnected(error));
                    }
                }
                Some(Err(ReadError::HardError(error))) => {
                    self.stream.take();
                    self.backoff();
                    return Some(Event::Disconnected(error.into()));
                }
                None | Some(Err(ReadError::StreamFooterReceived)) => {
                    self.stream.take();
                    self.backoff();
                    return Some(Event::Disconnected(Error::Disconnected));
                }
                Some(Err(_)) | Some(Ok(_)) => {}
            }
        }
    }
    fn backoff(&mut self) {
        self.delay = if self.delay.is_zero() {
            Duration::from_secs(1)
        } else {
            (self.delay * 2).min(Duration::from_secs(30))
        };
    }
    pub async fn send_stanza(&mut self, stanza: Stanza) -> Result<(), Error> {
        let Some(stream) = self.stream.as_mut() else {
            return Err(Error::Disconnected);
        };
        stream.send(&stanza).await?;
        Ok(())
    }
    pub async fn send_end(mut self) -> Result<(), Error> {
        if let Some(mut stream) = self.stream.take() {
            <Stream as SinkExt<&Stanza>>::close(&mut stream).await?;
        }
        Ok(())
    }
}
async fn connect(config: Account, password: String) -> Result<Stream, Error> {
    let jid = config.jid.parse::<tokio_xmpp::jid::BareJid>()?;
    let dns = if config.host.is_empty() {
        if config.direct_tls {
            DnsConfig::srv_xmpps(jid.domain().as_str())
        } else {
            DnsConfig::srv_default_client(jid.domain().as_str())
        }
    } else {
        DnsConfig::no_srv(&config.host, config.port)
    };
    let (pending, binding) = if config.direct_tls {
        DirectTlsServerConnector::from(dns)
            .connect(&jid.clone().into(), "jabber:client", Timeouts::default())
            .await?
    } else {
        StartTlsServerConnector::from(dns)
            .connect(&jid.clone().into(), "jabber:client", Timeouts::default())
            .await?
    };
    let (features, stream) = pending.recv_features().await?;
    let credentials = Credentials::default()
        .with_username(jid.node().unwrap().as_str())
        .with_password(password)
        .with_channel_binding(binding);
    let authenticated =
        tokio_xmpp::client_login(stream, features.sasl_mechanisms, credentials).await?;
    let pending = authenticated
        .send_header(StreamHeader {
            to: Some(Cow::Borrowed(jid.domain().as_str())),
            from: None,
            id: None,
        })
        .await?;
    let (_, mut stream) = pending.recv_features().await?;
    let bind_id = crate::model::new_id();
    let bind = Stanza::Iq(Iq::from_set(
        &bind_id,
        BindQuery::new(Some(format!("monogram-{}", crate::model::new_id()))),
    ));
    stream.send(&bind).await?;
    loop {
        match stream.next().await {
            Some(Ok(FallibleStreamElement::Ok(XmppStreamElement::Stanza(Stanza::Iq(iq)))))
                if iq.id() == bind_id =>
            {
                if let Iq::Result {
                    payload: Some(payload),
                    ..
                } = iq
                {
                    if let Ok(bound) = BindResponse::try_from(payload) {
                        if bound.jid.to_bare() == jid {
                            return Ok(stream.box_stream());
                        }
                    }
                }
                return Err(Error::Protocol(
                    tokio_xmpp::error::ProtocolError::InvalidBindResponse,
                ));
            }
            Some(Err(ReadError::HardError(error))) => return Err(error.into()),
            None => return Err(Error::Disconnected),
            _ => {}
        }
    }
}
