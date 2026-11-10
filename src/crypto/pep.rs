//! XEP-0384 PEP publication/discovery, matching mobile's read/merge/verify policy.
use super::{
    bundle::{self, Bundle, NS},
    *,
};
use crate::iq::{self, Kind};
use std::collections::{BTreeMap, BTreeSet};
use tokio_xmpp::minidom::Element;
pub const DEVICES: &str = "urn:xmpp:omemo:2:devices";
pub const BUNDLES: &str = "urn:xmpp:omemo:2:bundles";
const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const FORMS: &str = "jabber:x:data";
#[derive(Debug)]
pub enum Error {
    Crypto(super::Error),
    Request(iq::Error),
    PublicAccess,
}
impl Error {
    pub fn access_denied(&self) -> bool {
        matches!(self, Self::Request(iq::Error::Rejected { condition, pubsub })
            if matches!(condition.as_str(), "forbidden" | "not-authorized" | "subscription-required")
                || pubsub.as_deref() == Some("presence-subscription-required"))
    }
    pub fn unsupported(&self) -> bool {
        matches!(self, Self::Request(iq::Error::Rejected { condition, .. })
            if condition == "feature-not-implemented")
    }
}
impl From<super::Error> for Error {
    fn from(e: super::Error) -> Self {
        Self::Crypto(e)
    }
}
impl From<iq::Error> for Error {
    fn from(e: iq::Error) -> Self {
        Self::Request(e)
    }
}
type Result<T> = std::result::Result<T, Error>;
pub struct Pep {
    requests: iq::Client,
    owner: String,
    device: u32,
}
fn element(name: &str, ns: &str) -> Element {
    Element::builder(name, ns).build()
}
fn attr(mut root: Element, name: &str, value: impl Into<String>) -> Element {
    root.set_attr("".into(), name.try_into().unwrap(), value.into());
    root
}
fn invalid() -> Error {
    super::Error::Invalid("OMEMO 2 PEP response").into()
}
pub fn ids(root: Option<&Element>) -> super::Result<BTreeSet<u32>> {
    let mut ids = BTreeSet::new();
    let Some(root) = root else {
        return Ok(ids);
    };
    if !root.is("devices", NS) {
        return Err(super::Error::Invalid("device list namespace"));
    }
    bundle::xml(root, 65536)?;
    for child in root.children().filter(|c| c.is("device", NS)) {
        if ids.len() >= 500 || !ids.insert(bundle::positive(child.attr("id"))?) {
            return Err(super::Error::Invalid("device list"));
        }
    }
    Ok(ids)
}
pub fn add_own(root: Option<&Element>, device: u32) -> super::Result<Element> {
    let current = ids(root)?;
    bundle::positive(Some(&device.to_string()))?;
    let mut merged = root.cloned().unwrap_or_else(|| element("devices", NS));
    if !current.contains(&device) {
        merged.append_child(attr(element("device", NS), "id", device.to_string()));
    }
    ids(Some(&merged))?;
    Ok(merged)
}
fn form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = attr(element("x", FORMS), "type", "submit");
    let mut field = attr(
        attr(element("field", FORMS), "var", "FORM_TYPE"),
        "type",
        "hidden",
    );
    field.append_child(Element::builder("value", FORMS).append(form_type).build());
    form.append_child(field);
    for (name, value) in fields {
        let mut field = attr(element("field", FORMS), "var", *name);
        field.append_child(Element::builder("value", FORMS).append(*value).build());
        form.append_child(field);
    }
    form
}
impl Pep {
    pub fn new(requests: iq::Client, owner: &str, device: u32) -> super::Result<Self> {
        address(owner, device)?;
        Ok(Self {
            requests,
            owner: owner.into(),
            device,
        })
    }
    async fn request(&self, peer: &str, kind: Kind, payload: Element) -> Result<Option<Element>> {
        address(peer, 1)?;
        Ok(self.requests.request(peer, kind, payload).await?)
    }
    pub(super) async fn item(
        &self,
        peer: &str,
        node: &str,
        id: Option<&str>,
    ) -> Result<Option<Element>> {
        let mut items = attr(element("items", PUBSUB), "node", node);
        if let Some(id) = id {
            items.append_child(attr(element("item", PUBSUB), "id", id));
        } else {
            items = attr(items, "max_items", "1");
        }
        let mut query = element("pubsub", PUBSUB);
        query.append_child(items);
        let root = match self.request(peer, Kind::Get, query).await {
            Ok(Some(root)) => root,
            Ok(None) => return Err(invalid()),
            Err(Error::Request(iq::Error::Rejected { condition, pubsub }))
                if condition == "item-not-found"
                    && pubsub.as_deref() != Some("presence-subscription-required") =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if !root.is("pubsub", PUBSUB) {
            return Err(invalid());
        }
        bundle::xml(&root, 512 * 1024)?;
        let items = bundle::single(&root, "items", PUBSUB)?;
        if items.attr("node") != Some(node) {
            return Err(invalid());
        }
        let children: Vec<_> = items.children().collect();
        if children.is_empty() {
            return Ok(None);
        }
        if children.len() != 1
            || !children[0].is("item", PUBSUB)
            || id.is_some_and(|id| children[0].attr("id") != Some(id))
        {
            return Err(invalid());
        }
        let values: Vec<_> = children[0].children().collect();
        if values.len() != 1 {
            return Err(invalid());
        }
        Ok(Some(values[0].clone()))
    }
    pub async fn devices(&self, peer: &str) -> Result<Option<Element>> {
        let root = self.item(peer, DEVICES, None).await?;
        ids(root.as_ref())?;
        Ok(root)
    }
    pub async fn bundle(&self, peer: &str, device: u32) -> Result<Bundle> {
        address(peer, device)?;
        let root = self
            .item(peer, BUNDLES, Some(&device.to_string()))
            .await?
            .ok_or_else(invalid)?;
        Ok(Bundle::parse(&root)?)
    }
    pub async fn discover(&self, peer: &str) -> Result<BTreeMap<u32, Bundle>> {
        let root = self.devices(peer).await?;
        let mut bundles = BTreeMap::new();
        for id in ids(root.as_ref())? {
            if peer == self.owner && id == self.device {
                continue;
            }
            bundles.insert(id, self.bundle(peer, id).await?);
        }
        Ok(bundles)
    }
    async fn publish(&self, node: &str, id: &str, value: Element) -> Result<()> {
        if !(node == DEVICES && id == "current" || node == BUNDLES && id == self.device.to_string())
        {
            return Err(invalid());
        }
        let maximum = if node == DEVICES { "1" } else { "max" };
        self.publish_item(node, id, value, maximum).await
    }
    pub(super) async fn publish_item(
        &self,
        node: &str,
        id: &str,
        value: Element,
        maximum: &str,
    ) -> Result<()> {
        let fields = [
            ("pubsub#access_model", "open"),
            ("pubsub#persist_items", "true"),
            ("pubsub#max_items", maximum),
        ];
        let mut item = attr(element("item", PUBSUB), "id", id);
        item.append_child(value);
        let mut publish = attr(element("publish", PUBSUB), "node", node);
        publish.append_child(item);
        let mut options = element("publish-options", PUBSUB);
        options.append_child(form(
            "http://jabber.org/protocol/pubsub#publish-options",
            &fields,
        ));
        let mut query = element("pubsub", PUBSUB);
        query.append_child(publish);
        query.append_child(options);
        match self.request(&self.owner, Kind::Set, query.clone()).await {
            Ok(_) => Ok(()),
            Err(Error::Request(iq::Error::Rejected { condition, .. }))
                if matches!(condition.as_str(), "conflict" | "not-acceptable") =>
            {
                self.configure(node, &fields).await?;
                self.request(&self.owner, Kind::Set, query).await?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    async fn configure(&self, node: &str, fields: &[(&str, &str)]) -> Result<()> {
        let mut config = attr(element("configure", OWNER), "node", node);
        config.append_child(form(
            "http://jabber.org/protocol/pubsub#node_config",
            fields,
        ));
        let mut root = element("pubsub", OWNER);
        root.append_child(config);
        self.request(&self.owner, Kind::Set, root).await?;
        Ok(())
    }
    async fn open_access(&self, node: &str) -> Result<bool> {
        let mut query = element("pubsub", OWNER);
        query.append_child(attr(element("configure", OWNER), "node", node));
        let root = self
            .request(&self.owner, Kind::Get, query)
            .await?
            .ok_or_else(invalid)?;
        if !root.is("pubsub", OWNER) {
            return Err(invalid());
        }
        bundle::xml(&root, 256 * 1024)?;
        let config = bundle::single(&root, "configure", OWNER)?;
        if config.attr("node") != Some(node) {
            return Err(invalid());
        }
        let form = bundle::single(config, "x", FORMS)?;
        if form.attr("type") != Some("form") {
            return Err(invalid());
        }
        let mut values = BTreeMap::new();
        for field in form.children().filter(|c| c.is("field", FORMS)) {
            let key = field.attr("var").ok_or_else(invalid)?;
            if key == "FORM_TYPE" || key == "pubsub#access_model" {
                let value = bundle::single(field, "value", FORMS)?;
                if value.children().next().is_some() || values.insert(key, value.text()).is_some() {
                    return Err(invalid());
                }
            }
        }
        if values.get("FORM_TYPE").map(String::as_str)
            != Some("http://jabber.org/protocol/pubsub#node_config")
        {
            return Err(invalid());
        }
        Ok(values.get("pubsub#access_model").map(String::as_str) == Some("open"))
    }
    pub(super) async fn ensure_open_access(&self, node: &str) -> Result<()> {
        if !self.open_access(node).await? {
            self.configure(node, &[("pubsub#access_model", "open")])
                .await?;
            if !self.open_access(node).await? {
                return Err(Error::PublicAccess);
            }
        }
        Ok(())
    }
    pub async fn replenish(&self, bundle: &Bundle) -> Result<()> {
        bundle.validate()?;
        self.publish(BUNDLES, &self.device.to_string(), bundle.xml())
            .await
    }
    pub async fn initialize(&self, bundle: &Bundle) -> Result<Element> {
        // Never turn unsupported/failed reads into an empty list and overwrite another client.
        let mut latest = self.devices(&self.owner).await?;
        self.replenish(bundle).await?;
        for _ in 0..3 {
            if ids(latest.as_ref())?.contains(&self.device) {
                break;
            }
            self.publish(DEVICES, "current", add_own(latest.as_ref(), self.device)?)
                .await?;
            latest = self.devices(&self.owner).await?;
        }
        if !ids(latest.as_ref())?.contains(&self.device) {
            return Err(super::Error::Changed.into());
        }
        self.ensure_open_access(DEVICES).await?;
        self.ensure_open_access(BUNDLES).await?;
        Ok(latest.unwrap())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Command;
    use tokio::sync::mpsc;
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    fn own_bundle() -> Bundle {
        let mut store =
            records::Store::open(None, "alice@example.invalid", 101, MasterKey::new([1; 32]))
                .unwrap();
        device::prepare(&mut store, 1).unwrap()
    }
    #[test]
    fn failed_own_list_read_never_publishes() {
        runtime().block_on(async {
            let(tx,mut rx)=mpsc::unbounded_channel();let pep=Pep::new(iq::Client::new(tx),"alice@example.invalid",101).unwrap();let own=own_bundle();let init=pep.initialize(&own);tokio::pin!(init);let mut count=0;
            loop {tokio::select! {
                result=&mut init=>{assert!(matches!(result,Err(Error::Request(iq::Error::Rejected{..}))));break},
                command=rx.recv()=>{count+=1;assert_eq!(count,1,"Publication after failed own list read");let Some(Command::Iq{kind:Kind::Get,reply,..})=command else {panic!("Unexpected request")};reply.send(Err(iq::Error::Rejected{condition:"service-unavailable".into(),pubsub:None})).unwrap();}
            }}assert_eq!(count,1);
        });
    }
    #[test]
    fn publication_races_preserve_new_devices_and_verify_access_repair() {
        runtime().block_on(async {
            let(tx,mut rx)=mpsc::unbounded_channel();let pep=Pep::new(iq::Client::new(tx),"alice@example.invalid",101).unwrap();let own=own_bundle();let init=pep.initialize(&own);tokio::pin!(init);
            let mut latest=add_own(None,9).unwrap();let mut writes=0;let mut repairs=0;let mut open=BTreeSet::new();
            loop {tokio::select! {
                result=&mut init=>{let result=result.unwrap();assert_eq!(ids(Some(&result)).unwrap(),BTreeSet::from([9,10,11,101]));break},
                command=rx.recv()=>{
                    let Some(Command::Iq{kind,payload,reply,..})=command else {panic!("Unexpected request")};
                    let result=if let Some(items)=payload.get_child("items",PUBSUB) {
                        assert_eq!(items.attr("node"),Some(DEVICES));let mut item=attr(element("item",PUBSUB),"id","current");item.append_child(latest.clone());let mut items=attr(element("items",PUBSUB),"node",DEVICES);items.append_child(item);let mut root=element("pubsub",PUBSUB);root.append_child(items);Some(root)
                    } else if let Some(publish)=payload.get_child("publish",PUBSUB) {
                        if publish.attr("node")==Some(DEVICES) { writes+=1;let merged=publish.get_child("item",PUBSUB).unwrap().get_child("devices",NS).unwrap();assert!(ids(Some(merged)).unwrap().contains(&101));
                            if writes<3 { latest=add_own(Some(&latest),9+writes).unwrap(); } else {latest=merged.clone();}
                        } None
                    } else if let Some(config)=payload.get_child("configure",OWNER) {
                        let node=config.attr("node").unwrap();match kind {
                            Kind::Get=>{let value=if open.contains(node){"open"}else{"presence"};let mut config=attr(element("configure",OWNER),"node",node);let mut fields=form("http://jabber.org/protocol/pubsub#node_config",&[("pubsub#access_model",value)]);fields.set_attr("".into(),"type".try_into().unwrap(),"form");config.append_child(fields);let mut root=element("pubsub",OWNER);root.append_child(config);Some(root)},
                            Kind::Set=>{let form=config.get_child("x",FORMS).unwrap();let fields:Vec<_>=form.children().collect();assert_eq!(fields.len(),2,"Access repair must preserve other settings");assert!(fields.iter().any(|f|f.attr("var")==Some("pubsub#access_model")&&f.get_child("value",FORMS).unwrap().text()=="open"));repairs+=1;open.insert(node.to_string());None}
                        }
                    } else {panic!("Unexpected PEP request")};reply.send(Ok(result)).unwrap();
                }
            }}assert_eq!(writes,3);assert_eq!(repairs,2);
        });
    }
    #[test]
    fn device_list_merge_preserves_foreign_metadata() {
        let root:Element="<devices xmlns='urn:xmpp:omemo:2' marker='retain'><device id='9' label='Mobile'/><other xmlns='other:namespace'>keep</other></devices>".parse().unwrap();
        let merged = add_own(Some(&root), 10).unwrap();
        assert_eq!(ids(Some(&merged)).unwrap(), BTreeSet::from([9, 10]));
        assert_eq!(merged.attr("marker"), Some("retain"));
        assert_eq!(
            merged.children().next().unwrap().attr("label"),
            Some("Mobile")
        );
        assert!(merged.get_child("other", "other:namespace").is_some());
        assert_eq!(add_own(Some(&merged), 10).unwrap(), merged);
        for xml in [
            "<devices xmlns='urn:xmpp:omemo:2'><device id='9'/><device id='9'/></devices>",
            "<devices xmlns='urn:xmpp:omemo:2'><device id='0'/></devices>",
        ] {
            assert!(ids(Some(&xml.parse().unwrap())).is_err());
        }
    }
}
