//! Bounded, account-scoped IQ requests. A guessed ID cannot authorize a peer reply.
use crate::{
    model::new_id,
    protocol::{self, Command},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_xmpp::minidom::Element;
const CLIENT: &str = "jabber:client";
const STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
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
    pub async fn request(&self, to: &str, kind: Kind, payload: Element) -> Reply {
        if protocol::bare(to).ok().as_deref() != Some(to) {
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
        tokio::time::timeout(Duration::from_secs(25), received)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Closed)?
    }
}
struct Job {
    peer: String,
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
        if protocol::bare(&to).ok().as_deref() != Some(&to) {
            let _ = reply.send(Err(Error::Invalid));
            return None;
        }
        self.expire();
        if self.jobs.len() >= 256 {
            let _ = reply.send(Err(Error::Busy));
            return None;
        }
        let id = format!("request-{}", new_id());
        let request = Element::builder("iq", CLIENT)
            .attr("id".try_into().unwrap(), &id)
            .attr("to".try_into().unwrap(), &to)
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
        if from != job.peer && !(from.is_empty() && job.peer == owner) {
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
}
