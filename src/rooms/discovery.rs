//! XEP-0030 public-room directories, with optional XEP-0059 paging.
use super::{Error, Result, bounded, child, el, iq, manage};
use std::collections::{BTreeSet, VecDeque};
use tokio_xmpp::{jid::Jid, minidom::Element};

const INFO: &str = "http://jabber.org/protocol/disco#info";
const ITEMS: &str = "http://jabber.org/protocol/disco#items";
const MUC: &str = "http://jabber.org/protocol/muc";
const RSM: &str = "http://jabber.org/protocol/rsm";
pub const PAGE_SIZE: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub jid: String,
    pub name: String,
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub service: String,
    pub error: String,
}
#[derive(Debug)]
pub struct Batch {
    pub rooms: Vec<Room>,
    pub failures: Vec<Failure>,
    pub more: bool,
}
#[derive(Debug)]
struct Service {
    address: String,
    paging: Option<bool>,
    after: Option<String>,
    cursors: BTreeSet<String>,
}
#[derive(Debug)]
pub struct Directory {
    services: VecDeque<Service>,
    pending: VecDeque<Room>,
    seen: BTreeSet<String>,
    failures: Vec<Failure>,
}
impl Directory {
    pub async fn discover(iq: &iq::Client, account: &str) -> Result<Self> {
        let own = account.parse::<Jid>().map_err(|_| Error::Invalid)?;
        if own.node().is_none() || own.resource().is_some() || own.as_str() != account {
            return Err(Error::Invalid);
        }
        Ok(Self::new(manage::services(iq, account).await?))
    }
    pub fn for_service(service: &str) -> Result<Self> {
        domain(service)?;
        Ok(Self::new(vec![service.into()]))
    }
    fn new(services: Vec<String>) -> Self {
        Self {
            services: services
                .into_iter()
                .map(|address| Service {
                    address,
                    paging: None,
                    after: None,
                    cursors: BTreeSet::new(),
                })
                .collect(),
            pending: VecDeque::new(),
            seen: BTreeSet::new(),
            failures: Vec::new(),
        }
    }
    pub fn has_more(&self) -> bool {
        !self.pending.is_empty() || !self.services.is_empty()
    }
    fn batch(&mut self, failures: Vec<Failure>) -> Batch {
        self.failures.extend(failures);
        let rooms = self
            .pending
            .drain(..self.pending.len().min(PAGE_SIZE))
            .collect();
        Batch {
            rooms,
            failures: self.failures.clone(),
            more: self.has_more(),
        }
    }
    fn failed(&mut self, address: String, error: impl ToString) -> Batch {
        self.services.pop_front();
        self.batch(vec![Failure {
            service: address,
            error: error.to_string(),
        }])
    }
    /// Fetches at most one service response. Dropping the future retains its cursor.
    pub async fn next_page(&mut self, iq: &iq::Client) -> Batch {
        if !self.pending.is_empty() || self.services.is_empty() {
            return self.batch(Vec::new());
        }
        let address = self.services.front().unwrap().address.clone();
        if self.services.front().unwrap().paging.is_none() {
            let checked = match query(iq, &address, el("query", INFO)).await {
                Ok(xml) => service_info(&xml),
                Err(error) => Err(error),
            };
            match checked {
                Ok(paging) => self.services.front_mut().unwrap().paging = Some(paging),
                Err(error) => return self.failed(address, error),
            }
        }
        let service = self.services.front().unwrap();
        let requested_paging = service.paging == Some(true);
        let payload = items_query(requested_paging, service.after.as_deref());
        let parsed = match query(iq, &address, payload).await {
            Ok(xml) => match page(&xml, requested_paging) {
                Ok(page) => page,
                Err(error) => return self.failed(address, error),
            },
            Err(error) => return self.failed(address, error),
        };
        let mut failure = parsed.warning;
        if parsed.retry_unpaged {
            // RSM can support limits without supporting cursors; retry mobile's full query.
            let service = self.services.front_mut().unwrap();
            service.paging = Some(false);
            service.after = None;
            failure = None;
        } else if let Some(cursor) = parsed.next {
            let service = self.services.front_mut().unwrap();
            if !service.cursors.insert(cursor.clone()) {
                failure = Some("The directory repeated a page. Refresh to browse it again.");
                self.services.pop_front();
            } else {
                service.paging = Some(true);
                service.after = Some(cursor);
            }
        } else {
            self.services.pop_front();
        }
        for room in parsed.rooms {
            if self.seen.insert(room.jid.clone()) {
                self.pending.push_back(room);
            }
        }
        self.batch(
            failure
                .map(|error| Failure {
                    service: address,
                    error: error.into(),
                })
                .into_iter()
                .collect(),
        )
    }
}

fn domain(value: &str) -> Result<()> {
    let jid = value.parse::<Jid>().map_err(|_| Error::Invalid)?;
    if jid.node().is_some() || jid.resource().is_some() || jid.as_str() != value {
        return Err(Error::Invalid);
    }
    Ok(())
}
async fn query(iq: &iq::Client, to: &str, payload: Element) -> Result<Element> {
    iq.request(to, iq::Kind::Get, payload)
        .await?
        .ok_or(Error::Invalid)
}
fn service_info(xml: &Element) -> Result<bool> {
    bounded(xml)?;
    if !xml.is("query", INFO)
        || xml.attr("node").is_some()
        || !xml.children().any(|item| {
            item.is("identity", INFO)
                && item.attr("category") == Some("conference")
                && item.attr("type") == Some("text")
        })
        || !xml
            .children()
            .any(|item| item.is("feature", INFO) && item.attr("var") == Some(MUC))
    {
        return Err(Error::Invalid);
    }
    Ok(xml
        .children()
        .any(|item| item.is("feature", INFO) && item.attr("var") == Some(RSM)))
}
fn items_query(paging: bool, after: Option<&str>) -> Element {
    let mut query = Element::builder("query", ITEMS);
    if paging {
        let mut set = Element::builder("set", RSM).append(
            Element::builder("max", RSM)
                .append(PAGE_SIZE.to_string())
                .build(),
        );
        if let Some(after) = after {
            set = set.append(Element::builder("after", RSM).append(after).build());
        }
        query = query.append(set.build());
    }
    query.build()
}
struct Parsed {
    rooms: Vec<Room>,
    next: Option<String>,
    warning: Option<&'static str>,
    retry_unpaged: bool,
}
fn cursor(xml: &Element) -> Result<String> {
    let value = xml.text();
    if xml.children().next().is_some() || value.is_empty() || value.len() > 4096 {
        return Err(Error::Invalid);
    }
    Ok(value)
}
fn number(xml: &Element) -> Result<u64> {
    if xml.children().next().is_some() {
        return Err(Error::Invalid);
    }
    xml.text().parse().map_err(|_| Error::Invalid)
}
fn page(xml: &Element, requested_paging: bool) -> Result<Parsed> {
    bounded(xml)?;
    if !xml.is("query", ITEMS) || xml.attr("node").is_some() {
        return Err(Error::Invalid);
    }
    let set = child(xml, "set", RSM)?;
    let mut rooms = Vec::new();
    let mut raw_count = 0;
    for item in xml.children() {
        if item.is("set", RSM) {
            continue;
        }
        if !item.is("item", ITEMS)
            || item.children().next().is_some()
            || !item.text().trim().is_empty()
        {
            return Err(Error::Invalid);
        }
        raw_count += 1;
        // A disco node is a hierarchy entry, not a room that can be joined.
        if item.attr("node").is_some() {
            continue;
        }
        let address = item.attr("jid").ok_or(Error::Invalid)?;
        let jid = address.parse::<Jid>().map_err(|_| Error::Invalid)?;
        if jid.node().is_none() || jid.resource().is_some() || jid.as_str() != address {
            return Err(Error::Invalid);
        }
        let name = item.attr("name").unwrap_or(address);
        if name.len() > 1024 {
            return Err(Error::Invalid);
        }
        rooms.push(Room {
            jid: address.into(),
            name: name.into(),
        });
    }
    let mut next = None;
    let mut warning = None;
    if let Some(set) = set {
        if set
            .children()
            .any(|item| !item.is("first", RSM) && !item.is("last", RSM) && !item.is("count", RSM))
        {
            return Err(Error::Invalid);
        }
        let first = child(set, "first", RSM)?;
        let last = child(set, "last", RSM)?;
        let count = child(set, "count", RSM)?.map(number).transpose()?;
        if let Some(first) = first {
            if let Some(index) = first.attr("index") {
                index.parse::<u64>().map_err(|_| Error::Invalid)?;
            }
            cursor(first)?;
        }
        if raw_count == 0 {
            if first.is_some() || last.is_some() {
                return Err(Error::Invalid);
            }
        } else if first.is_some() && last.is_some() {
            next = Some(cursor(last.unwrap())?);
        } else if first.is_some() || last.is_some() {
            return Err(Error::Invalid);
        } else if requested_paging || count.is_some_and(|count| count > raw_count) {
            warning = Some(
                "The service limited its directory without a paging cursor. Some rooms may be missing.",
            );
        }
    } else if requested_paging && raw_count >= PAGE_SIZE as u64 {
        warning = Some("The service omitted paging information. Some rooms may be missing.");
    }
    let retry_unpaged = requested_paging && raw_count > 0 && next.is_none();
    Ok(Parsed {
        rooms,
        next,
        warning,
        retry_unpaged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_validates_rooms_and_preserves_opaque_paging() {
        let xml: Element = format!(
            "<query xmlns='{ITEMS}'><item jid='a@rooms.example' name='A &amp; B'/><item jid='b@rooms.example'/><set xmlns='{RSM}'><first index='0'>opaque-first</first><last>opaque:last &amp; value</last><count>999</count></set></query>"
        ).parse().unwrap();
        let parsed = page(&xml, true).unwrap();
        assert_eq!(parsed.rooms.len(), 2);
        assert_eq!(parsed.rooms[0].name, "A & B");
        assert_eq!(parsed.next.as_deref(), Some("opaque:last & value"));
        let query = items_query(true, parsed.next.as_deref());
        assert_eq!(
            query
                .get_child("set", RSM)
                .unwrap()
                .get_child("after", RSM)
                .unwrap()
                .text(),
            "opaque:last & value"
        );
        for jid in ["other.example", "a@rooms.example/resource"] {
            let invalid: Element = format!("<query xmlns='{ITEMS}'><item jid='{jid}'/></query>")
                .parse()
                .unwrap();
            assert!(page(&invalid, false).is_err());
        }
        let alias: Element =
            format!("<query xmlns='{ITEMS}'><item jid='a@elsewhere.example'/></query>")
                .parse()
                .unwrap();
        assert_eq!(
            page(&alias, false).unwrap().rooms[0].jid,
            "a@elsewhere.example"
        );
        let limited: Element = format!("<query xmlns='{ITEMS}'><item jid='a@rooms.example'/><set xmlns='{RSM}'><count>20</count></set></query>").parse().unwrap();
        let parsed = page(&limited, false).unwrap();
        assert!(parsed.warning.is_some());
        assert_eq!(parsed.rooms.len(), 1);
        let empty: Element =
            format!("<query xmlns='{ITEMS}'><set xmlns='{RSM}'><count>20</count></set></query>")
                .parse()
                .unwrap();
        assert!(page(&empty, true).unwrap().next.is_none());
        assert!(Directory::for_service("room@rooms.example").is_err());
        assert!(Directory::for_service("rooms.example/resource").is_err());
    }

    #[tokio::test]
    async fn directory_pages_deduplicates_and_stops_repeated_cursor() {
        use crate::protocol::Command;
        let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let iq = iq::Client::new(tx);
        let server = tokio::spawn(async move {
            for step in 0..4 {
                let Command::Iq {
                    to, payload, reply, ..
                } = requests.recv().await.unwrap()
                else {
                    panic!("unexpected command")
                };
                assert_eq!(to, "rooms.example");
                let xml = if step == 0 {
                    assert!(payload.is("query", INFO));
                    format!(
                        "<query xmlns='{INFO}'><identity category='conference' type='text'/><feature var='{MUC}'/><feature var='{RSM}'/></query>"
                    )
                } else {
                    assert!(payload.is("query", ITEMS));
                    let set = payload.get_child("set", RSM).unwrap();
                    assert_eq!(set.get_child("max", RSM).unwrap().text(), "64");
                    if step > 1 {
                        assert_eq!(set.get_child("after", RSM).unwrap().text(), "opaque-one");
                    }
                    let jid = if step == 1 { "a" } else { "b" };
                    format!(
                        "<query xmlns='{ITEMS}'><item jid='{jid}@rooms.example'/><item jid='a@rooms.example'/><set xmlns='{RSM}'><first>opaque-first</first><last>opaque-one</last></set></query>"
                    )
                };
                reply.send(Ok(Some(xml.parse().unwrap()))).unwrap();
                if step == 2 {
                    break;
                }
            }
        });
        let mut directory = Directory::for_service("rooms.example").unwrap();
        let first = directory.next_page(&iq).await;
        assert_eq!(first.rooms.len(), 1);
        assert!(first.more);
        let second = directory.next_page(&iq).await;
        assert_eq!(second.rooms.len(), 1);
        assert_eq!(second.rooms[0].jid, "b@rooms.example");
        assert!(!second.more);
        assert_eq!(second.failures.len(), 1);
        assert_eq!(directory.next_page(&iq).await.failures.len(), 1);
        server.await.unwrap();

        let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let iq = iq::Client::new(tx);
        let server = tokio::spawn(async move {
            let Command::Iq { to, reply, .. } = requests.recv().await.unwrap() else {
                panic!("unexpected command")
            };
            assert_eq!(to, "broken.example");
            reply.send(Err(iq::Error::Timeout)).unwrap();
            let Command::Iq { to, reply, .. } = requests.recv().await.unwrap() else {
                panic!("unexpected command")
            };
            assert_eq!(to, "rooms.example");
            reply.send(Ok(Some(format!("<query xmlns='{INFO}'><identity category='conference' type='text'/><feature var='{MUC}'/><feature var='{RSM}'/></query>").parse().unwrap()))).unwrap();
            let Command::Iq { payload, reply, .. } = requests.recv().await.unwrap() else {
                panic!("unexpected command")
            };
            assert!(payload.get_child("set", RSM).is_some());
            let limited = (0..PAGE_SIZE)
                .map(|n| format!("<item jid='room{n}@rooms.example'/>"))
                .collect::<String>();
            reply.send(Ok(Some(format!("<query xmlns='{ITEMS}'>{limited}<set xmlns='{RSM}'><count>{}</count></set></query>", PAGE_SIZE+2).parse().unwrap()))).unwrap();
            let Command::Iq { payload, reply, .. } = requests.recv().await.unwrap() else {
                panic!("unexpected command")
            };
            assert!(payload.get_child("set", RSM).is_none());
            let items = (0..PAGE_SIZE + 2)
                .map(|n| format!("<item jid='room{n}@rooms.example'/>"))
                .collect::<String>();
            reply
                .send(Ok(Some(
                    format!("<query xmlns='{ITEMS}'>{items}</query>")
                        .parse()
                        .unwrap(),
                )))
                .unwrap();
        });
        let mut directory = Directory::new(vec!["broken.example".into(), "rooms.example".into()]);
        let failed = directory.next_page(&iq).await;
        assert!(failed.more && failed.rooms.is_empty());
        assert_eq!(failed.failures.len(), 1);
        let first = directory.next_page(&iq).await;
        assert_eq!(first.rooms.len(), PAGE_SIZE);
        assert!(first.more);
        assert_eq!(first.failures.len(), 1);
        let last = directory.next_page(&iq).await;
        assert_eq!(last.rooms.len(), 2);
        assert!(!last.more);
        assert_eq!(last.failures.len(), 1);
        server.await.unwrap();
    }
}
