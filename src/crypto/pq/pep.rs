// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from Monogram/Monocles PEP policy; see native/NOTICE.md.
//! XEP-0384 PEP publication/discovery, matching mobile's read/merge/verify policy.
use super::{
    bundle::{self, Bundle, NS},
    *,
};
use crate::iq::{self, Kind};
use std::collections::{BTreeMap, BTreeSet};
use tokio_xmpp::minidom::Element;
pub const DEVICES: &str = "urn:monocles:omemo-pq:1:devices";
pub const BUNDLES: &str = "urn:monocles:omemo-pq:1:bundles";
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
    super::Error::Invalid("PQ OMEMO PEP response").into()
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
        if ids.len() >= 128 || !ids.insert(bundle::positive(child.attr("id"))?) {
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
        let id = id.unwrap_or("current");
        items.append_child(attr(element("item", PUBSUB), "id", id));
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
            || children[0].attr("id") != Some(id)
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
    pub async fn announce(&self) -> Result<()> {
        let mut latest = self.devices(&self.owner).await?;
        for _ in 0..3 {
            if ids(latest.as_ref())?.contains(&self.device) {
                return Ok(());
            }
            self.publish(DEVICES, "current", add_own(latest.as_ref(), self.device)?)
                .await?;
            latest = self.devices(&self.owner).await?;
        }
        if !ids(latest.as_ref())?.contains(&self.device) {
            return Err(super::Error::Changed.into());
        }
        Ok(())
    }
}
