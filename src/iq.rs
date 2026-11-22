//! Bounded, account-scoped IQ requests. A guessed ID cannot authorize a peer reply.
#[cfg(test)]
use crate::protocol;
use crate::{model::new_id, protocol::Command};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_xmpp::minidom::Element;
const CLIENT: &str = "jabber:client";
/// XEP-0077, used to ask the account's own server to cancel the registration.
pub const REGISTER_NS: &str = "jabber:iq:register";
const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
fn valid_to(to: &str) -> bool {
    to.parse::<tokio_xmpp::jid::Jid>()
        .ok()
        .is_some_and(|jid| jid.to_string() == to)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Closed,
    Timeout,
    Invalid,
    Busy,
    Rejected {
        condition: String,
        pubsub: Option<String>,
    },
}
pub type Reply = Result<Option<Element>, Error>;
#[derive(Debug)]
pub enum Kind {
    Get,
    Set,
}
#[derive(Clone)]
pub struct Client {
    tx: mpsc::UnboundedSender<Command>,
}
impl Client {
    pub fn new(tx: mpsc::UnboundedSender<Command>) -> Self {
        Self { tx }
    }
    fn dispatch(&self, to: &str, kind: Kind, payload: Element) -> Result<oneshot::Receiver<Reply>, Error> {
        if !valid_to(to) {
            return Err(Error::Invalid);
        }
        let (reply, received) = oneshot::channel();
        self.tx
            .send(Command::Iq {
                to: to.into(),
                kind,
                payload,
                reply,
            })
            .map_err(|_| Error::Closed)?;
        Ok(received)
    }
    /// For code on a Tokio task: gives up after 25 seconds.
    pub async fn request(&self, to: &str, kind: Kind, payload: Element) -> Reply {
        let received = self.dispatch(to, kind, payload)?;
        tokio::time::timeout(Duration::from_secs(25), received)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Closed)?
    }
    /// For the GTK main loop, which has no Tokio runtime and so no Tokio timer (`request` panics
    /// there). No timer is needed: the session sweeps stale jobs and answers them with
    /// `Error::Timeout`, and drops them with `Error::Closed` when the connection ends.
    pub async fn request_on_main_loop(&self, to: &str, kind: Kind, payload: Element) -> Reply {
        self.dispatch(to, kind, payload)?
            .await
            .map_err(|_| Error::Closed)?
    }
}
struct Job {
    peer: String,
    local_private: bool,
    /// Addressed to the account's own server (XEP-0077): sent without `to`, and the answer may
    /// name the account, nothing, or the server's domain.
    server_request: bool,
    reply: oneshot::Sender<Reply>,
    started: Instant,
}
#[derive(Default)]
pub struct Pending {
    jobs: HashMap<String, Job>,
}
impl Pending {
    pub fn start(
        &mut self,
        to: String,
        kind: Kind,
        payload: Element,
        reply: oneshot::Sender<Reply>,
    ) -> Option<Element> {
        if !valid_to(&to) {
            let _ = reply.send(Err(Error::Invalid));
            return None;
        }
        self.expire();
        if self.jobs.len() >= 256 {
            let _ = reply.send(Err(Error::Busy));
            return None;
        }
        let id = format!("request-{}", new_id());
        let local_private = payload.is("query", "jabber:iq:private");
        let server_request = payload.is("query", REGISTER_NS);
        let mut request = Element::builder("iq", CLIENT).attr("id".try_into().unwrap(), &id);
        if !local_private
            && !server_request
            && !(payload.ns() == crate::blocking::NS
                && matches!(payload.name(), "blocklist" | "block" | "unblock"))
        {
            request = request.attr("to".try_into().unwrap(), &to);
        }
        let request = request
            .attr(
                "type".try_into().unwrap(),
                match kind {
                    Kind::Get => "get",
                    Kind::Set => "set",
                },
            )
            .append(payload)
            .build();
        self.jobs.insert(
            id,
            Job {
                peer: to,
                local_private,
                server_request,
                reply,
                started: Instant::now(),
            },
        );
        Some(request)
    }
    pub fn receive(&mut self, root: &Element, owner: &str) -> bool {
        if !root.is("iq", CLIENT) || !matches!(root.attr("type"), Some("result" | "error")) {
            return false;
        }
        let id = root.attr("id").unwrap_or("");
        let Some(job) = self.jobs.get(id) else {
            return false;
        };
        let from = root.attr("from").unwrap_or("");
        // XEP-0049 is addressed to the local server. Some servers echo the bound resource.
        let own_resource = job.local_private
            && job.peer == owner
            && root.attr("to") == Some(from)
            && from
                .parse::<tokio_xmpp::jid::Jid>()
                .ok()
                .is_some_and(|jid| jid.resource().is_some() && jid.to_bare().as_str() == owner);
        let own_server = job.server_request
            && job.peer == owner
            && owner.split_once('@').is_some_and(|(_, domain)| from == domain);
        if from != job.peer
            && !(from.is_empty() && job.peer == owner)
            && !own_resource
            && !own_server
        {
            return false;
        }
        if root
            .attr("to")
            .is_some_and(|to| to.split('/').next() != Some(owner))
        {
            return false;
        }
        let children: Vec<_> = root.children().collect();
        let result = if root.attr("type") == Some("result") {
            if children.len() > 1 {
                Err(Error::Invalid)
            } else {
                Ok(children.first().map(|c| (*c).clone()))
            }
        } else {
            let errors: Vec<_> = children.iter().filter(|c| c.is("error", CLIENT)).collect();
            if errors.len() != 1 {
                Err(Error::Invalid)
            } else {
                let conditions: Vec<_> = errors[0]
                    .children()
                    .filter(|c| c.ns() == STANZAS && c.name() != "text")
                    .collect();
                if conditions.len() != 1 || conditions[0].name().len() > 64 {
                    Err(Error::Invalid)
                } else {
                    let pubsub = errors[0]
                        .children()
                        .find(|c| c.ns() == "http://jabber.org/protocol/pubsub#errors")
                        .filter(|c| c.name().len() <= 64)
                        .map(|c| c.name().to_string());
                    Err(Error::Rejected {
                        condition: conditions[0].name().into(),
                        pubsub,
                    })
                }
            }
        };
        let job = self.jobs.remove(id).unwrap();
        let _ = job.reply.send(result);
        true
    }
    pub fn fail(&mut self, id: &str) {
        if let Some(job) = self.jobs.remove(id) {
            let _ = job.reply.send(Err(Error::Closed));
        }
    }
    pub fn disconnect(&mut self) {
        for (_, job) in self.jobs.drain() {
            let _ = job.reply.send(Err(Error::Closed));
        }
    }
    pub fn expire(&mut self) {
        let expired: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| {
                job.reply.is_closed() || job.started.elapsed() >= Duration::from_secs(20)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(job) = self.jobs.remove(&id) {
                let _ = job.reply.send(Err(Error::Timeout));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request(pending: &mut Pending, to: &str) -> (Element, oneshot::Receiver<Reply>) {
        let (tx, rx) = oneshot::channel();
        (
            pending
                .start(
                    to.into(),
                    Kind::Get,
                    protocol::el("pubsub", "http://jabber.org/protocol/pubsub"),
                    tx,
                )
                .unwrap(),
            rx,
        )
    }
    #[test]
    fn sender_and_account_fences() {
        let mut pending = Pending::default();
        let (req, mut rx) = request(&mut pending, "bob@example.org");
        let mut reply = Element::builder("iq", CLIENT)
            .attr("type".try_into().unwrap(), "result")
            .attr("id".try_into().unwrap(), req.attr("id").unwrap())
            .build();
        assert!(!pending.receive(&reply, "alice@example.org"));
        reply.set_attr("".into(), "from".try_into().unwrap(), "eve@example.org");
        assert!(!pending.receive(&reply, "alice@example.org"));
        reply.set_attr("".into(), "from".try_into().unwrap(), "bob@example.org");
        reply.set_attr("".into(), "to".try_into().unwrap(), "other@example.org");
        assert!(!pending.receive(&reply, "alice@example.org"));
        reply.set_attr(
            "".into(),
            "to".try_into().unwrap(),
            "alice@example.org/resource",
        );
        assert!(pending.receive(&reply, "alice@example.org"));
        assert_eq!(rx.try_recv().unwrap(), Ok(None));
        assert!(!pending.receive(&reply, "alice@example.org"));
    }
    #[test]
    fn full_resource_and_service_requests_keep_exact_response_authority() {
        let mut pending = Pending::default();
        let (request, mut received) = request(&mut pending, "bob@example.org/phone");
        let mut reply = Element::builder("iq", CLIENT)
            .attr("type".try_into().unwrap(), "result")
            .attr("id".try_into().unwrap(), request.attr("id").unwrap())
            .build();
        for wrong in [
            "bob@example.org",
            "bob@example.org/laptop",
            "eve@example.org/phone",
        ] {
            reply.set_attr("".into(), "from".try_into().unwrap(), wrong);
            assert!(!pending.receive(&reply, "alice@example.org"));
        }
        reply.set_attr(
            "".into(),
            "from".try_into().unwrap(),
            "bob@example.org/phone",
        );
        assert!(pending.receive(&reply, "alice@example.org"));
        assert_eq!(received.try_recv().unwrap(), Ok(None));
        assert!(valid_to("upload.example.org"));
        assert!(!valid_to(" bob@example.org"));
        assert!(!valid_to(""));
    }
    #[test]
    fn disconnect_expiry_and_cancel_are_terminal() {
        let mut pending = Pending::default();
        let (_, mut rx) = request(&mut pending, "alice@example.org");
        pending.disconnect();
        assert_eq!(rx.try_recv().unwrap(), Err(Error::Closed));
        let (req, mut rx) = request(&mut pending, "alice@example.org");
        pending
            .jobs
            .get_mut(req.attr("id").unwrap())
            .unwrap()
            .started = Instant::now() - Duration::from_secs(21);
        pending.expire();
        assert_eq!(rx.try_recv().unwrap(), Err(Error::Timeout));
        let (_, rx) = request(&mut pending, "alice@example.org");
        drop(rx);
        pending.expire();
        assert!(pending.jobs.is_empty());
    }
    fn cancel() -> Element {
        Element::builder("query", REGISTER_NS)
            .append(Element::builder("remove", REGISTER_NS).build())
            .build()
    }
    fn register_request(pending: &mut Pending, owner: &str) -> (Element, oneshot::Receiver<Reply>) {
        let (tx, rx) = oneshot::channel();
        (
            pending
                .start(owner.into(), Kind::Set, cancel(), tx)
                .unwrap(),
            rx,
        )
    }
    fn result(id: &str, from: Option<&str>) -> Element {
        let mut reply = Element::builder("iq", CLIENT)
            .attr("type".try_into().unwrap(), "result")
            .attr("id".try_into().unwrap(), id)
            .build();
        if let Some(from) = from {
            reply.set_attr("".into(), "from".try_into().unwrap(), from);
        }
        reply
    }
    #[test]
    fn account_deletion_goes_to_the_own_server_without_a_to_address() {
        let mut pending = Pending::default();
        let (req, _rx) = register_request(&mut pending, "bob@example.com");
        assert_eq!(req.attr("type"), Some("set"));
        assert_eq!(req.attr("to"), None, "XEP-0077 is addressed to the connected server");
        let query = req.children().next().unwrap();
        assert!(query.is("query", REGISTER_NS));
        assert!(query.children().next().unwrap().is("remove", REGISTER_NS));
    }
    #[test]
    fn account_deletion_accepts_the_servers_answer_from_the_account_or_its_domain() {
        for from in [None, Some("bob@example.com"), Some("example.com")] {
            let mut pending = Pending::default();
            let (req, mut rx) = register_request(&mut pending, "bob@example.com");
            let reply = result(req.attr("id").unwrap(), from);
            assert!(pending.receive(&reply, "bob@example.com"), "from {from:?}");
            assert_eq!(rx.try_recv().unwrap(), Ok(None));
        }
    }
    #[test]
    fn account_deletion_ignores_an_answer_from_anyone_else() {
        for from in ["eve@example.com", "example.org", "bob@example.org", "example.com.evil.net"] {
            let mut pending = Pending::default();
            let (req, mut rx) = register_request(&mut pending, "bob@example.com");
            let reply = result(req.attr("id").unwrap(), Some(from));
            assert!(!pending.receive(&reply, "bob@example.com"), "from {from}");
            assert!(rx.try_recv().is_err(), "a forged answer must not confirm the deletion");
        }
    }
    #[test]
    fn the_server_domain_is_not_trusted_for_other_requests() {
        let mut pending = Pending::default();
        let (req, mut rx) = request(&mut pending, "bob@example.com");
        let reply = result(req.attr("id").unwrap(), Some("example.com"));
        assert!(!pending.receive(&reply, "alice@example.org"));
        assert!(!pending.receive(&reply, "bob@example.com"));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn account_deletion_reports_a_server_refusal() {
        let mut pending = Pending::default();
        let (req, mut rx) = register_request(&mut pending, "bob@example.com");
        let error = Element::builder("error", CLIENT)
            .append(Element::builder("forbidden", STANZAS).build())
            .build();
        let mut reply = Element::builder("iq", CLIENT)
            .attr("type".try_into().unwrap(), "error")
            .attr("id".try_into().unwrap(), req.attr("id").unwrap())
            .append(error)
            .build();
        reply.set_attr("".into(), "from".try_into().unwrap(), "example.com");
        assert!(pending.receive(&reply, "bob@example.com"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Err(Error::Rejected { condition: "forbidden".into(), pubsub: None })
        );
    }
    #[test]
    fn a_dropped_connection_fails_the_deletion_request_as_closed() {
        let mut pending = Pending::default();
        let (_req, mut rx) = register_request(&mut pending, "bob@example.com");
        pending.disconnect();
        assert_eq!(rx.try_recv().unwrap(), Err(Error::Closed));
    }
    #[test]
    fn the_main_loop_request_needs_no_tokio_runtime() {
        use futures_util::FutureExt;
        // No runtime exists on this thread, as on the GTK main loop: `request` would panic here.
        assert!(tokio::runtime::Handle::try_current().is_err());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let client = Client::new(tx);
        let mut pending = Box::pin(client.request_on_main_loop("example.com", Kind::Get, cancel()));
        assert!((&mut pending).now_or_never().is_none(), "waits for the server");
        let Some(Command::Iq { to, reply, .. }) = rx.try_recv().ok() else {
            panic!("the request was sent to the session");
        };
        assert_eq!(to, "example.com");
        reply.send(Ok(None)).unwrap();
        assert_eq!(pending.now_or_never(), Some(Ok(None)));
    }
    /// The panic this guards against: `thread 'main' panicked at src/iq.rs:63:9: there is no
    /// reactor running, must be called from the context of a Tokio 1.x runtime`.
    #[test]
    fn the_tokio_request_panics_without_a_runtime_which_is_why_the_gtk_loop_must_not_use_it() {
        use futures_util::FutureExt;
        assert!(tokio::runtime::Handle::try_current().is_err());
        let (tx, _rx) = mpsc::unbounded_channel();
        let client = Client::new(tx);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = client.request("example.com", Kind::Get, cancel()).now_or_never();
        }));
        let message = match caught {
            Ok(()) => panic!("`request` must need a Tokio runtime; if it no longer does, the main-loop variant can go"),
            Err(payload) => payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default(),
        };
        assert!(message.contains("no reactor running"), "{message}");
    }

    #[test]
    fn the_tokio_request_works_on_a_tokio_task_and_the_main_loop_one_works_there_too() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            for main_loop in [false, true] {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let client = Client::new(tx);
                let asked = async {
                    if main_loop {
                        client.request_on_main_loop("example.com", Kind::Get, cancel()).await
                    } else {
                        client.request("example.com", Kind::Get, cancel()).await
                    }
                };
                let answer = async {
                    let Some(Command::Iq { reply, .. }) = rx.recv().await else { panic!("sent to the session") };
                    reply.send(Ok(None)).unwrap();
                };
                let (got, ()) = tokio::join!(asked, answer);
                assert_eq!(got, Ok(None), "main_loop: {main_loop}");
            }
        });
    }

    #[test]
    fn both_requests_report_a_closed_session_and_a_bad_address_the_same_way() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(async {
            let (tx, rx) = mpsc::unbounded_channel();
            let client = Client::new(tx);
            assert_eq!(client.request("not a jid", Kind::Get, cancel()).await, Err(Error::Invalid));
            assert_eq!(client.request_on_main_loop("not a jid", Kind::Get, cancel()).await, Err(Error::Invalid));
            drop(rx);
            assert_eq!(client.request("example.com", Kind::Get, cancel()).await, Err(Error::Closed));
            assert_eq!(client.request_on_main_loop("example.com", Kind::Get, cancel()).await, Err(Error::Closed));
        });
    }

    #[test]
    fn the_main_loop_request_reports_a_dropped_session_and_a_bad_address() {
        use futures_util::FutureExt;
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Client::new(tx);
        let bad = client.request_on_main_loop("not a jid", Kind::Get, cancel());
        assert_eq!(bad.now_or_never(), Some(Err(Error::Invalid)));
        drop(rx);
        let gone = client.request_on_main_loop("example.com", Kind::Get, cancel());
        assert_eq!(gone.now_or_never(), Some(Err(Error::Closed)));
    }
    #[test]
    fn a_session_that_drops_the_job_answers_closed() {
        use futures_util::FutureExt;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let client = Client::new(tx);
        let mut pending = Box::pin(client.request_on_main_loop("example.com", Kind::Get, cancel()));
        assert!((&mut pending).now_or_never().is_none());
        drop(rx.try_recv().unwrap());
        assert_eq!(pending.now_or_never(), Some(Err(Error::Closed)));
    }
}
