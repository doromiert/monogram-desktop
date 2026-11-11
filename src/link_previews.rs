// SPDX-License-Identifier: GPL-3.0-or-later
//! Device-local previews. Port of mobile MonogramLinkPreviews/MonogramPreviewHttp;
//! metadata never becomes an attachment or an outgoing XMPP payload.
use crate::{model::Message, storage::Store};
use gtk::{gio, glib, prelude::*};
use reqwest::{Url, header};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    net::{IpAddr, SocketAddr},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const HTML_LIMIT: usize = 512 * 1024;
const IMAGE_LIMIT: usize = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);
const POSITIVE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const NEGATIVE_MS: i64 = 60 * 60 * 1000;
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Article,
    Photo,
    Video,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Thumbnail {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Media {
    pub url: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preview {
    pub url: String,
    pub display_url: String,
    pub site_name: String,
    pub title: String,
    pub description: String,
    pub kind: Kind,
    pub thumbnail: Option<Thumbnail>,
    /// A validated direct MP4/WebM or locally constructed YouTube embed. Load only on explicit play.
    pub media: Option<Media>,
}
struct Policy {
    owner: String,
    state: AtomicU64,
    changed: tokio::sync::watch::Sender<u64>,
}
#[derive(Clone)]
pub struct Privacy(Arc<Policy>);
#[derive(Clone)]
pub struct Permit {
    policy: Arc<Policy>,
    epoch: u64,
    encrypted: bool,
    cancelled: Arc<tokio::sync::watch::Sender<bool>>,
}
impl Privacy {
    pub fn new(owner: &str, enabled: bool, encrypted: bool) -> Self {
        let (changed, _) = tokio::sync::watch::channel(0);
        Self(Arc::new(Policy {
            owner: owner.into(),
            state: AtomicU64::new(u64::from(enabled) | (u64::from(encrypted) << 1)),
            changed,
        }))
    }
    pub fn owner(&self) -> &str {
        &self.0.owner
    }
    pub fn set_permissions(&self, enabled: bool, encrypted: bool) {
        let flags = u64::from(enabled) | (u64::from(encrypted) << 1);
        let previous = self
            .0
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some((state & !3).wrapping_add(4) | flags)
            })
            .unwrap();
        self.0
            .changed
            .send_replace((previous & !3).wrapping_add(4) | flags);
    }
    pub fn cancel(&self) {
        let epoch = self.0.state.fetch_add(4, Ordering::AcqRel).wrapping_add(4);
        self.0.changed.send_replace(epoch);
    }
    pub fn ticket(&self, encrypted: bool) -> Option<Permit> {
        let (cancelled, _) = tokio::sync::watch::channel(false);
        let permit = Permit {
            policy: self.0.clone(),
            epoch: self.0.state.load(Ordering::Acquire),
            encrypted,
            cancelled: Arc::new(cancelled),
        };
        permit.allowed().then_some(permit)
    }
}
impl Permit {
    pub fn owner(&self) -> &str {
        &self.policy.owner
    }
    pub fn allowed(&self) -> bool {
        !self.policy.owner.is_empty()
            && self.policy.state.load(Ordering::Acquire) == self.epoch
            && self.epoch & 1 != 0
            && (!self.encrypted || self.epoch & 2 != 0)
            && !*self.cancelled.borrow()
    }
    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }
    fn check(&self) -> Result<()> {
        self.allowed().then_some(()).ok_or("Preview cancelled")
    }
    async fn cancellation(&self) {
        let mut policy = self.policy.changed.subscribe();
        let mut job = self.cancelled.subscribe();
        while self.allowed() {
            tokio::select! { _ = policy.changed() => {}, _ = job.changed() => {} }
        }
    }
}

pub fn safe_url(address: &str) -> Result<Url> {
    if address.encode_utf16().count() > 4096
        || address.chars().any(|c| "\\<>\"{}|^`".contains(c))
        || address.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("Invalid preview URL");
    }
    let Some((scheme, rest)) = address.split_once("://") else {
        return Err("Invalid preview URL");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if !scheme.eq_ignore_ascii_case("https") || authority.is_empty() || authority.contains('@') {
        return Err("Invalid preview URL");
    }
    for (index, byte) in address.bytes().enumerate() {
        if byte == b'%'
            && address
                .as_bytes()
                .get(index + 1..index + 3)
                .is_none_or(|hex| !hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("Invalid preview URL");
        }
    }
    let mut url = Url::parse(address).map_err(|_| "Invalid preview URL")?;
    let host = url.host_str().ok_or("Invalid preview URL")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || host.len() > 253
        || host.ends_with('.')
        || !(host.contains('.') || host.contains(':'))
    {
        return Err("Preview requires a public HTTPS address");
    }
    url.set_fragment(None);
    Ok(url)
}
// IANA suffix labels from mobile LinkifyPort.java (factual interoperability data).
// Source attribution: Telegram/Monogram LinkifyPort.java; mobile/LICENSE is GPL-2.0.
const MOBILE_TLDS: &str = concat!(
    "aaa aarp abb abbott abogado ac academy accenture accountant accountants aco active actor ad adnl ads adult ae ",
    "aeg aero af afl ag agency ai aig airforce airtel al allfinanz alsace am amica amsterdam android ao apartments ",
    "app apple aq aquarelle ar aramco archi army arpa arte as asia associates at attorney au auction audio auto ",
    "autos aw ax axa az azure ba band bank bar barcelona barclaycard barclays bargains bauhaus bayern bb bbc bbva ",
    "bcn bd be beats beer bentley berlin best bet bf bg bh bharti bi bible bid bike bing bingo bio biz bj black ",
    "blackfriday bloomberg blue bm bms bmw bn bnl bnpparibas bo boats bom bond boo boots boutique br bradesco ",
    "bridgestone broadway broker brother brussels bs bt budapest build builders business buzz bv bw by bz bzh ca ",
    "cab cafe cal camera camp cancerresearch canon capetown capital car caravan cards care career careers cars ",
    "cartier casa cash casino cat catering cba cbn cc cd ceb center ceo cern cf cfa cfd cg ch chanel channel chat ",
    "cheap chloe christmas chrome church ci cipriani cisco citic city cityeats ck cl claims cleaning click clinic ",
    "clothing cloud club clubmed cm cn co coach codes coffee college cologne com commbank community company ",
    "computer comsec condos construction consulting contractors cooking cool coop corsica country coupons courses ",
    "cr credit creditcard creditunion cricket crown crs cruises csc cu cuisinella cv cw cx cy cymru cyou cz dabur ",
    "dad dance date dating datsun day dclk de deals degree delivery dell delta democrat dental dentist desi design ",
    "dev diamonds diet digital direct directory discount dj dk dm dnp do docs dog doha domains doosan download ",
    "drive durban dvag dz earth eat ec edu education ee eg email emerck energy engineer engineering enterprises ",
    "epson equipment er erni es esq estate et eu eurovision eus events everbank exchange expert exposed express ",
    "fage fail fairwinds faith family fan fans farm fashion feedback ferrero fi film final finance financial ",
    "firmdale fish fishing fit fitness fj fk flights florist flowers flsmidth fly fm fo foo football forex forsale ",
    "forum foundation fr frl frogans fund furniture futbol fyi ga gal gallery game garden gb gbiz gd gdn ge gea ",
    "gent genting gf gg ggee gh gi gift gifts gives giving gl glass gle global globo gm gmail gmo gmx gn gold ",
    "goldpoint golf goo goog google gop gov gp gq gr grainger graphics gratis green gripe group gs gt gu gucci guge ",
    "guide guitars guru gw gy hamburg hangout haus healthcare help here hermes hiphop hitachi hiv hk hm hn hockey ",
    "holdings holiday homedepot homes honda horse host hosting hoteles hotmail house how hr hsbc ht hu hyundai ibm ",
    "icbc ice icu id ie ifm iinet il im immo immobilien in industries infiniti info ing ink institute insure int ",
    "international investments io ipiranga iq ir irish is ist istanbul it itau iwc jaguar java jcb je jetzt jewelry ",
    "jlc jll jm jo jobs joburg jp jprs juegos kaufen kddi ke kg kh ki kia kim kinder kitchen kiwi km kn koeln ",
    "komatsu kp kr krd kred kw ky kyoto kz la lacaixa lancaster land landrover lasalle lat latrobe law lawyer lb lc ",
    "lds lease leclerc legal lexus lgbt li liaison lidl life lifestyle lighting limited limo linde link live lixil ",
    "lk loan loans lol london lotte lotto love lr ls lt ltd ltda lu lupin luxe luxury lv ly ma madrid maif maison ",
    "man management mango market marketing markets marriott mba mc md me media meet melbourne meme memorial men ",
    "menu meo mg mh miami microsoft mil mini mk ml mm mma mn mo mobi moda moe moi mom monash money montblanc mormon ",
    "mortgage moscow motorcycles mov movie movistar mp mq mr ms mt mtn mtpc mtr mu museum mutuelle mv mw mx my mz ",
    "na nadex nagoya name navy nc ne nec net netbank network neustar new news nexus nf ng ngo nhk ni nico ninja ",
    "nissan nl no nokia np nr nra nrw ntt nu nyc nz obi office okinawa om omega one ong onl online ooo oracle ",
    "orange org organic osaka otsuka ovh pa page panerai paris partners parts party pe pet pf pg ph pharmacy ",
    "philips photo photography photos physio piaget pics pictet pictures ping pink pizza pk pl place play ",
    "playstation plumbing plus pm pn pohl poker porn post pr praxi press pro prod productions prof properties ",
    "property protection ps pt pub pw py qa qpon quebec racing re realtor realty recipes red redstone rehab reise ",
    "reisen reit ren rent rentals repair report republican rest restaurant review reviews rich ricoh rio rip ro ",
    "rocher rocks rodeo rs rsvp ru ruhr run rw rwe ryukyu sa saarland sakura sale samsung sandvik sandvikcoromant ",
    "sanofi sap sapo sarl saxo sb sbs sc sca scb schmidt scholarships school schule schwarz science scor scot sd se ",
    "seat security seek sener services seven sew sex sexy sg sh shiksha shoes show shriram si singles site sj sk ",
    "ski sky skype sl sm sn sncf so soccer social software sohu solar solutions sony soy space spiegel ",
    "spreadbetting sr srl st stada starhub statoil stc stcgroup stockholm studio study style su sucks supplies ",
    "supply support surf surgery suzuki sv swatch swiss sx sy sydney systems sz tab taipei tatamotors tatar tattoo ",
    "tax taxi tc td team tech technology tel telefonica temasek tennis tf tg th thd theater theatre tickets tienda ",
    "tips tires tirol tj tk tl tm tn to today tokyo ton tools top toray toshiba tours town toyota toys tr trade ",
    "trading training travel trust tt tui tv tw tz ua ubs ug uk university uno uol us uy uz va vacations vana vc ve ",
    "vegas ventures versicherung vet vg vi viajes video villas vin virgin vision vista vistaprint viva vlaanderen ",
    "vn vodka vote voting voto voyage vu wales walter wang watch webcam website wed wedding weir wf whoswho wien ",
    "wiki williamhill win windows wine wme work works world ws wtc wtf xbox xerox xin xn--11b4c3d xn--1qqw23a ",
    "xn--30rr7y xn--3bst00m xn--3ds443g xn--3e0b707e xn--3pxu8k xn--42c2d9a xn--45brj9c xn--45q11c xn--4gbrim ",
    "xn--55qw42g xn--55qx5d xn--6frz82g xn--6qq986b3xl xn--80adxhks xn--80ao21a xn--80asehdb xn--80aswg xn--90a3ac ",
    "xn--90ais xn--9dbq2a xn--9et52u xn--b4w605ferd xn--c1avg xn--c2br7g xn--cg4bki xn--clchc0ea0b2g2a9gcd ",
    "xn--czr694b xn--czrs0t xn--czru2d xn--d1acj3b xn--d1alf xn--efvy88h xn--estv75g xn--fhbei xn--fiq228c5hs ",
    "xn--fiq64b xn--fiqs8s xn--fiqz9s xn--fjq720a xn--flw351e xn--fpcrj9c3d xn--fzc2c9e2c xn--gecrj9c xn--h2brj9c ",
    "xn--hxt814e xn--i1b6b1a6a2e xn--imr513n xn--io0a7i xn--j1aef xn--j1amh xn--j6w193g xn--kcrx77d1x4a xn--kprw13d ",
    "xn--kpry57d xn--kput3i xn--l1acc xn--lgbbat1ad8j xn--mgb9awbf xn--mgba3a3ejt xn--mgba3a4f16a xn--mgbaam7a8h ",
    "xn--mgbab2bd xn--mgbayh7gpa xn--mgbbh1a71e xn--mgbc0a9azcg xn--mgberp4a5d4ar xn--mgbpl2fh xn--mgbtx2b ",
    "xn--mgbx4cd0ab xn--mk1bu44c xn--mxtq1m xn--ngbc5azd xn--node xn--nqv7f xn--nqv7fs00ema xn--nyqy26a xn--o3cw4h ",
    "xn--ogbpf8fl xn--p1acf xn--p1ai xn--pgbs0dh xn--pssy2u xn--q9jyb4c xn--qcka1pmc xn--qxam xn--rhqv96g ",
    "xn--s9brj9c xn--ses554g xn--t60b56a xn--tckwe xn--unup4y xn--vermgensberater-ctb xn--vermgensberatung-pwb ",
    "xn--vhquv xn--vuq861b xn--wgbh1c xn--wgbl6a xn--xhq521b xn--xkc2al3hye2a xn--xkc2dl3a5ee0h xn--y9a3aq ",
    "xn--yfro4i67o xn--ygbi2ammx xn--zfr164b xperia xxx xyz yachts yamaxun yandex ye yodobashi yoga yokohama ",
    "youtube yt za zara zip zm zone zuerich zw ελ бел дети ком мкд мон москва онлайн орг рус рф сайт срб укр қаз ",
    "հայ קום ارامكو الاردن الجزائر السعودية المغرب امارات ایران بازار بھارت تونس سودان سورية شبكة عراق عمان فلسطين ",
    "قطر كوم مصر مليسيا موقع कॉम नेट भारत संगठन ভারত ਭਾਰਤ ભારત இந்தியா இலங்கை சிங்கப்பூர் భారత్ ලංකා คอม ไทย გე みんな ",
    "グーグル コム 世界 中信 中国 中國 中文网 企业 佛山 信息 健康 八卦 公司 公益 台湾 台灣 商城 商店 商标 在线 大拿 娱乐 工行 广东 慈善 我爱你 手机 政务 政府 新加坡 新闻 时尚 机构 淡马锡 游戏 ",
    "点看 移动 组织机构 网址 网店 网络 谷歌 集团 飞利浦 餐厅 香港 닷넷 닷컴 삼성 한국 ",
);
fn bare_host(host: &str) -> bool {
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    let Some((_, suffix)) = host.rsplit_once('.') else {
        return false;
    };
    let suffix = suffix.to_ascii_lowercase();
    static TLDS: std::sync::OnceLock<std::collections::HashSet<&'static str>> =
        std::sync::OnceLock::new();
    TLDS.get_or_init(|| MOBILE_TLDS.split_whitespace().collect())
        .contains(suffix.as_str())
        || suffix.strip_prefix("xn--").is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 59
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && name
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickableUrl {
    /// UTF-8 bytes, matching Pango's attribute indices.
    pub range: std::ops::Range<usize>,
    pub uri: String,
}
fn code_ranges(body: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = body.as_bytes();
    let (mut index, mut opening, mut ranges) = (0, None, Vec::new());
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b'`' {
            index += 1;
        }
        let mut slash = start;
        while slash > 0 && bytes[slash - 1] == b'\\' {
            slash -= 1;
        }
        if (start - slash) % 2 == 1 {
            continue;
        }
        let width = index - start;
        match opening {
            None => opening = Some((start, width)),
            Some((begin, count)) if count == width => {
                ranges.push(begin..index);
                opening = None;
            }
            _ => {}
        }
    }
    if let Some((start, _)) = opening.filter(|(_, width)| *width >= 3) {
        ranges.push(start..body.len());
    }
    ranges
}
fn trim_url_token(token: &str) -> &str {
    let mut balance = [0i32; 3];
    for ch in token.chars() {
        match ch {
            '(' => balance[0] += 1,
            ')' => balance[0] -= 1,
            '[' => balance[1] += 1,
            ']' => balance[1] -= 1,
            '{' => balance[2] += 1,
            '}' => balance[2] -= 1,
            _ => {}
        }
    }
    let mut end = token.len();
    while let Some(ch) = token[..end].chars().next_back() {
        let closing = match ch {
            ')' => Some(0),
            ']' => Some(1),
            '}' => Some(2),
            _ => None,
        };
        if ".,;:!?".contains(ch) {
            end -= ch.len_utf8();
        } else if let Some(index) = closing.filter(|index| balance[*index] < 0) {
            balance[index] += 1;
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    &token[..end]
}
fn clickable_uri(address: &str) -> Option<String> {
    if address.encode_utf16().count() > 4096
        || address
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || "\\<>\"{}|^`".contains(ch))
    {
        return None;
    }
    let url = Url::parse(address).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
fn url_matches(body: &str, clickable: bool) -> Vec<ClickableUrl> {
    let mut found = Vec::new();
    if body.encode_utf16().count() > 65536 {
        return found;
    }
    let code = code_ranges(body);
    let mut segment_end = 0;
    let mut consumed = 0;
    for (start, ch) in body.char_indices() {
        if start < consumed || !ch.is_alphanumeric() {
            continue;
        }
        if let Some(previous) = body[..start].chars().next_back()
            && (previous.is_alphanumeric() || "@./:#_-".contains(previous))
        {
            continue;
        }
        if start >= segment_end {
            segment_end = body[start..]
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || c.is_control() || "<>\"`".contains(*c))
                .map_or(body.len(), |(offset, _)| start + offset);
        }
        let tail = &body[start..segment_end];
        let explicit = tail
            .as_bytes()
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"https://"))
            || clickable
                && tail
                    .as_bytes()
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"http://"));
        let end = if explicit {
            segment_end
        } else {
            // A host is at most 253 bytes. Stop before punctuation instead of repeatedly
            // parsing the entire remaining message for every possible label boundary.
            let boundary = tail
                .char_indices()
                .take(254)
                .find(|(_, c)| !c.is_alphanumeric() && !"._-".contains(*c));
            let host_end = boundary.map_or(tail.len(), |(at, _)| at);
            if host_end > 253 {
                continue;
            }
            let host = tail[..host_end].trim_end_matches('.');
            if !bare_host(host) || boundary.is_some_and(|(_, c)| c == '@') {
                continue;
            }
            if boundary.is_some_and(|(_, c)| "/?#:".contains(c)) {
                segment_end
            } else {
                start + host.len()
            }
        };
        consumed = end;
        let token = trim_url_token(&body[start..end]);
        let span = start..start + token.len();
        if code
            .iter()
            .any(|range| range.start < span.end && range.end > span.start)
        {
            continue;
        }
        let address = if explicit {
            token.to_owned()
        } else {
            format!("https://{token}")
        };
        let uri = if clickable {
            clickable_uri(&address)
        } else {
            safe_url(&address).ok().map(|url| url.to_string())
        };
        if let Some(uri) = uri {
            found.push(ClickableUrl { range: span, uri });
            if !clickable || found.len() >= 512 {
                break;
            }
        }
    }
    found
}
fn url_match(body: &str) -> Option<(std::ops::Range<usize>, String)> {
    url_matches(body, false)
        .into_iter()
        .next()
        .map(|link| (link.range, link.uri))
}
pub fn clickable_urls(
    body: &str,
    spoilers: Option<&crate::spoilers::Context>,
) -> Vec<ClickableUrl> {
    let context = spoilers.filter(|context| context.matches(body));
    url_matches(body, true)
        .into_iter()
        .filter(|link| {
            context.is_none_or(|context| {
                !context.intersects(
                    body[..link.range.start].chars().count(),
                    body[..link.range.end].chars().count(),
                )
            })
        })
        .collect()
}
pub fn first_url(body: &str) -> Option<String> {
    url_match(body).map(|(_, url)| url)
}
pub fn candidate(message: &Message) -> Option<String> {
    if message.id.is_empty()
        || message.retracted
        || message.unavailable
        || !message.ciphertext.is_empty()
        || message.attachment.is_some()
        || message.upload.is_some()
        || message.unsupported_attachment
        || message.action.is_some()
        || message.poll.is_some()
        || message.kind != "text"
        || crate::legacy_polls::parse(&message.text).is_some()
    {
        return None;
    }
    let context = message.preview_spoilers.as_ref()?;
    if !context.matches(&message.text) {
        return None;
    }
    let (span, url) = url_match(&message.text)?;
    let start = message.text[..span.start].chars().count();
    let end = message.text[..span.end].chars().count();
    (!context.intersects(start, end)).then_some(url)
}
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a != 0
                && a != 10
                && a != 127
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192 && (b == 0 || b == 168 || b == 88 && c == 99))
                && !(a == 198 && (b == 18 || b == 19 || b == 51 && c == 100))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let b = ip.octets();
            // Exclude IANA's complete 2001::/23 protocol-assignment block.
            b[0] & 0xe0 == 0x20
                && !(b[0] == 0x20 && b[1] == 0x01 && (b[2] < 2 || b[2] == 0x0d && b[3] == 0xb8))
                && !(b[0] == 0x20 && b[1] == 0x02)
                && !(b[0] == 0x3f && b[1] == 0xff)
        }
    }
}

struct Response {
    url: Url,
    mime: String,
    bytes: Vec<u8>,
}
#[derive(Clone, Copy)]
enum Body {
    Strict,
    HtmlPrefix,
    MediaPrefix,
}
async fn request(address: &str, limit: usize, mode: Body, permit: &Permit) -> Result<Response> {
    let mut url = safe_url(address)?;
    for redirects in 0..=4 {
        permit.check()?;
        let host = url
            .host_str()
            .ok_or("Invalid preview URL")?
            .trim_matches(['[', ']']);
        let addresses = tokio::net::lookup_host((host, 443))
            .await
            .map_err(|_| "Preview destination unavailable")?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|a| !public_address(a.ip())) {
            return Err("Unsafe preview destination");
        }
        permit.check()?;
        // Disable ambient proxies: they would bypass the DNS pin and reveal the URL elsewhere.
        let client = crate::downloads::client_builder()
            .no_proxy()
            .tls_backend_native()
            .http1_only()
            .resolve(host, SocketAddr::new(addresses[0].ip(), 443))
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(5))
            .timeout(DEADLINE)
            .build()
            .map_err(|_| "HTTPS preview unavailable")?;
        let agent = if matches!(
            host,
            "kkinstagram.com" | "www.kkinstagram.com" | "kkclip.com" | "www.kkclip.com"
        ) {
            "TelegramBot (Monogram-LinkPreview/1)"
        } else {
            "Monogram-LinkPreview/1"
        };
        let mut req = client
            .get(url.clone())
            .header(header::USER_AGENT, agent)
            .header(
                header::ACCEPT,
                "text/html, application/json, image/jpeg, image/png, image/webp",
            )
            .header(header::ACCEPT_ENCODING, "identity");
        if matches!(mode, Body::MediaPrefix) {
            req = req.header(header::RANGE, format!("bytes=0-{}", limit - 1));
        }
        let mut response = req.send().await.map_err(|_| "HTTPS preview unavailable")?;
        permit.check()?;
        let headers = response.headers();
        if headers
            .iter()
            .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
            .sum::<usize>()
            > 32768
            || headers.get_all(header::CONTENT_LENGTH).iter().count() > 1
            || headers.get_all(header::TRANSFER_ENCODING).iter().count() > 1
            || headers.contains_key(header::TRANSFER_ENCODING)
                && headers.contains_key(header::CONTENT_LENGTH)
        {
            return Err("Invalid preview response");
        }
        if [301, 302, 303, 307, 308].contains(&response.status().as_u16()) {
            if redirects == 4 {
                return Err("Preview redirect limit");
            }
            let location = headers
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("Invalid preview redirect")?;
            url = safe_url(
                url.join(location)
                    .map_err(|_| "Invalid preview redirect")?
                    .as_str(),
            )?;
            continue;
        }
        let partial = matches!(mode, Body::MediaPrefix)
            && response.status().as_u16() == 206
            && headers
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(content_range);
        if response.status().as_u16() != 200 && !partial {
            return Err("Preview unavailable");
        }
        if headers
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
            || headers
                .get(header::TRANSFER_ENCODING)
                .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"chunked"))
        {
            return Err("Unsupported preview encoding");
        }
        let mime = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let kind = mime.to_ascii_lowercase();
        if matches!(mode, Body::HtmlPrefix)
            && (kind.starts_with("image/") || kind.starts_with("video/"))
        {
            return Ok(Response {
                url,
                mime,
                bytes: vec![],
            });
        }
        let prefix = matches!(mode, Body::MediaPrefix)
            || matches!(mode, Body::HtmlPrefix)
                && (kind.starts_with("text/html") || kind.starts_with("application/xhtml+xml"));
        if !prefix && response.content_length().is_some_and(|n| n > limit as u64) {
            return Err("Preview too large");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Preview download interrupted")?
        {
            permit.check()?;
            if bytes.len() + chunk.len() > limit {
                if !prefix {
                    return Err("Preview too large");
                }
                bytes.extend_from_slice(&chunk[..limit - bytes.len()]);
                break;
            }
            bytes.extend_from_slice(&chunk);
            if prefix && bytes.len() == limit {
                break;
            }
        }
        return Ok(Response { url, mime, bytes });
    }
    Err("Preview redirect limit")
}
fn content_range(value: &str) -> bool {
    value
        .strip_prefix("bytes 0-")
        .and_then(|v| v.split_once('/'))
        .is_some_and(|(end, total)| {
            !end.is_empty()
                && end.bytes().all(|b| b.is_ascii_digit())
                && (total == "*" || !total.is_empty() && total.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn attributes(tag: &str) -> HashMap<String, String> {
    let bytes = tag.as_bytes();
    let mut at = 0;
    let mut result = HashMap::new();
    while at < bytes.len() {
        if !(bytes[at].is_ascii_alphabetic() || matches!(bytes[at], b'_' | b':')) {
            at += 1;
            continue;
        }
        let start = at;
        at += 1;
        while at < bytes.len()
            && (bytes[at].is_ascii_alphanumeric()
                || b"-_: .".contains(&bytes[at]) && bytes[at] != b' ')
        {
            at += 1;
        }
        let name = tag[start..at].to_ascii_lowercase();
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] != b'=' {
            continue;
        }
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() {
            break;
        }
        let quote = bytes[at];
        let start = if matches!(quote, b'\'' | b'"') {
            at += 1;
            at
        } else {
            at
        };
        if matches!(quote, b'\'' | b'"') {
            while at < bytes.len() && bytes[at] != quote {
                at += 1;
            }
        } else {
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>' {
                at += 1;
            }
        }
        result.insert(name, tag[start..at].into());
        if at < bytes.len() && matches!(quote, b'\'' | b'"') {
            at += 1;
        }
    }
    result
}
fn entity(value: &str) -> Option<char> {
    if let Some(number) = value
        .strip_prefix("#x")
        .or_else(|| value.strip_prefix("#X"))
    {
        return u32::from_str_radix(number, 16)
            .ok()
            .and_then(char::from_u32);
    }
    if let Some(number) = value.strip_prefix('#') {
        return number.parse::<u32>().ok().and_then(char::from_u32);
    }
    // Standard HTML4 named character references, as used by Android's HTML reader.
    Some(match value {
        "AElig" => '\u{c6}',
        "Aacute" => '\u{c1}',
        "Acirc" => '\u{c2}',
        "Agrave" => '\u{c0}',
        "Alpha" => '\u{391}',
        "Aring" => '\u{c5}',
        "Atilde" => '\u{c3}',
        "Auml" => '\u{c4}',
        "Beta" => '\u{392}',
        "Ccedil" => '\u{c7}',
        "Chi" => '\u{3a7}',
        "Dagger" => '\u{2021}',
        "Delta" => '\u{394}',
        "ETH" => '\u{d0}',
        "Eacute" => '\u{c9}',
        "Ecirc" => '\u{ca}',
        "Egrave" => '\u{c8}',
        "Epsilon" => '\u{395}',
        "Eta" => '\u{397}',
        "Euml" => '\u{cb}',
        "Gamma" => '\u{393}',
        "Iacute" => '\u{cd}',
        "Icirc" => '\u{ce}',
        "Igrave" => '\u{cc}',
        "Iota" => '\u{399}',
        "Iuml" => '\u{cf}',
        "Kappa" => '\u{39a}',
        "Lambda" => '\u{39b}',
        "Mu" => '\u{39c}',
        "Ntilde" => '\u{d1}',
        "Nu" => '\u{39d}',
        "OElig" => '\u{152}',
        "Oacute" => '\u{d3}',
        "Ocirc" => '\u{d4}',
        "Ograve" => '\u{d2}',
        "Omega" => '\u{3a9}',
        "Omicron" => '\u{39f}',
        "Oslash" => '\u{d8}',
        "Otilde" => '\u{d5}',
        "Ouml" => '\u{d6}',
        "Phi" => '\u{3a6}',
        "Pi" => '\u{3a0}',
        "Prime" => '\u{2033}',
        "Psi" => '\u{3a8}',
        "Rho" => '\u{3a1}',
        "Scaron" => '\u{160}',
        "Sigma" => '\u{3a3}',
        "THORN" => '\u{de}',
        "Tau" => '\u{3a4}',
        "Theta" => '\u{398}',
        "Uacute" => '\u{da}',
        "Ucirc" => '\u{db}',
        "Ugrave" => '\u{d9}',
        "Upsilon" => '\u{3a5}',
        "Uuml" => '\u{dc}',
        "Xi" => '\u{39e}',
        "Yacute" => '\u{dd}',
        "Yuml" => '\u{178}',
        "Zeta" => '\u{396}',
        "aacute" => '\u{e1}',
        "acirc" => '\u{e2}',
        "acute" => '\u{b4}',
        "aelig" => '\u{e6}',
        "agrave" => '\u{e0}',
        "alefsym" => '\u{2135}',
        "alpha" => '\u{3b1}',
        "amp" => '\u{26}',
        "and" => '\u{2227}',
        "ang" => '\u{2220}',
        "aring" => '\u{e5}',
        "asymp" => '\u{2248}',
        "atilde" => '\u{e3}',
        "auml" => '\u{e4}',
        "bdquo" => '\u{201e}',
        "beta" => '\u{3b2}',
        "brvbar" => '\u{a6}',
        "bull" => '\u{2022}',
        "cap" => '\u{2229}',
        "ccedil" => '\u{e7}',
        "cedil" => '\u{b8}',
        "cent" => '\u{a2}',
        "chi" => '\u{3c7}',
        "circ" => '\u{2c6}',
        "clubs" => '\u{2663}',
        "cong" => '\u{2245}',
        "copy" => '\u{a9}',
        "crarr" => '\u{21b5}',
        "cup" => '\u{222a}',
        "curren" => '\u{a4}',
        "dArr" => '\u{21d3}',
        "dagger" => '\u{2020}',
        "darr" => '\u{2193}',
        "deg" => '\u{b0}',
        "delta" => '\u{3b4}',
        "diams" => '\u{2666}',
        "divide" => '\u{f7}',
        "eacute" => '\u{e9}',
        "ecirc" => '\u{ea}',
        "egrave" => '\u{e8}',
        "empty" => '\u{2205}',
        "emsp" => '\u{2003}',
        "ensp" => '\u{2002}',
        "epsilon" => '\u{3b5}',
        "equiv" => '\u{2261}',
        "eta" => '\u{3b7}',
        "eth" => '\u{f0}',
        "euml" => '\u{eb}',
        "euro" => '\u{20ac}',
        "exist" => '\u{2203}',
        "fnof" => '\u{192}',
        "forall" => '\u{2200}',
        "frac12" => '\u{bd}',
        "frac14" => '\u{bc}',
        "frac34" => '\u{be}',
        "frasl" => '\u{2044}',
        "gamma" => '\u{3b3}',
        "ge" => '\u{2265}',
        "gt" => '\u{3e}',
        "hArr" => '\u{21d4}',
        "harr" => '\u{2194}',
        "hearts" => '\u{2665}',
        "hellip" => '\u{2026}',
        "iacute" => '\u{ed}',
        "icirc" => '\u{ee}',
        "iexcl" => '\u{a1}',
        "igrave" => '\u{ec}',
        "image" => '\u{2111}',
        "infin" => '\u{221e}',
        "int" => '\u{222b}',
        "iota" => '\u{3b9}',
        "iquest" => '\u{bf}',
        "isin" => '\u{2208}',
        "iuml" => '\u{ef}',
        "kappa" => '\u{3ba}',
        "lArr" => '\u{21d0}',
        "lambda" => '\u{3bb}',
        "lang" => '\u{2329}',
        "laquo" => '\u{ab}',
        "larr" => '\u{2190}',
        "lceil" => '\u{2308}',
        "ldquo" => '\u{201c}',
        "le" => '\u{2264}',
        "lfloor" => '\u{230a}',
        "lowast" => '\u{2217}',
        "loz" => '\u{25ca}',
        "lrm" => '\u{200e}',
        "lsaquo" => '\u{2039}',
        "lsquo" => '\u{2018}',
        "lt" => '\u{3c}',
        "macr" => '\u{af}',
        "mdash" => '\u{2014}',
        "micro" => '\u{b5}',
        "middot" => '\u{b7}',
        "minus" => '\u{2212}',
        "mu" => '\u{3bc}',
        "nabla" => '\u{2207}',
        "nbsp" => '\u{a0}',
        "ndash" => '\u{2013}',
        "ne" => '\u{2260}',
        "ni" => '\u{220b}',
        "not" => '\u{ac}',
        "notin" => '\u{2209}',
        "nsub" => '\u{2284}',
        "ntilde" => '\u{f1}',
        "nu" => '\u{3bd}',
        "oacute" => '\u{f3}',
        "ocirc" => '\u{f4}',
        "oelig" => '\u{153}',
        "ograve" => '\u{f2}',
        "oline" => '\u{203e}',
        "omega" => '\u{3c9}',
        "omicron" => '\u{3bf}',
        "oplus" => '\u{2295}',
        "or" => '\u{2228}',
        "ordf" => '\u{aa}',
        "ordm" => '\u{ba}',
        "oslash" => '\u{f8}',
        "otilde" => '\u{f5}',
        "otimes" => '\u{2297}',
        "ouml" => '\u{f6}',
        "para" => '\u{b6}',
        "part" => '\u{2202}',
        "permil" => '\u{2030}',
        "perp" => '\u{22a5}',
        "phi" => '\u{3c6}',
        "pi" => '\u{3c0}',
        "piv" => '\u{3d6}',
        "plusmn" => '\u{b1}',
        "pound" => '\u{a3}',
        "prime" => '\u{2032}',
        "prod" => '\u{220f}',
        "prop" => '\u{221d}',
        "psi" => '\u{3c8}',
        "quot" => '\u{22}',
        "rArr" => '\u{21d2}',
        "radic" => '\u{221a}',
        "rang" => '\u{232a}',
        "raquo" => '\u{bb}',
        "rarr" => '\u{2192}',
        "rceil" => '\u{2309}',
        "rdquo" => '\u{201d}',
        "real" => '\u{211c}',
        "reg" => '\u{ae}',
        "rfloor" => '\u{230b}',
        "rho" => '\u{3c1}',
        "rlm" => '\u{200f}',
        "rsaquo" => '\u{203a}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "scaron" => '\u{161}',
        "sdot" => '\u{22c5}',
        "sect" => '\u{a7}',
        "shy" => '\u{ad}',
        "sigma" => '\u{3c3}',
        "sigmaf" => '\u{3c2}',
        "sim" => '\u{223c}',
        "spades" => '\u{2660}',
        "sub" => '\u{2282}',
        "sube" => '\u{2286}',
        "sum" => '\u{2211}',
        "sup" => '\u{2283}',
        "sup1" => '\u{b9}',
        "sup2" => '\u{b2}',
        "sup3" => '\u{b3}',
        "supe" => '\u{2287}',
        "szlig" => '\u{df}',
        "tau" => '\u{3c4}',
        "there4" => '\u{2234}',
        "theta" => '\u{3b8}',
        "thetasym" => '\u{3d1}',
        "thinsp" => '\u{2009}',
        "thorn" => '\u{fe}',
        "tilde" => '\u{2dc}',
        "times" => '\u{d7}',
        "trade" => '\u{2122}',
        "uArr" => '\u{21d1}',
        "uacute" => '\u{fa}',
        "uarr" => '\u{2191}',
        "ucirc" => '\u{fb}',
        "ugrave" => '\u{f9}',
        "uml" => '\u{a8}',
        "upsih" => '\u{3d2}',
        "upsilon" => '\u{3c5}',
        "uuml" => '\u{fc}',
        "weierp" => '\u{2118}',
        "xi" => '\u{3be}',
        "yacute" => '\u{fd}',
        "yen" => '\u{a5}',
        "yuml" => '\u{ff}',
        "zeta" => '\u{3b6}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        "apos" => '\'',
        _ => return None,
    })
}
fn clean(value: &str, limit: usize) -> String {
    let mut plain = String::new();
    let mut at = 0;
    while at < value.len() {
        let rest = &value[at..];
        if rest.starts_with('<')
            && let Some(end) = rest.find('>')
        {
            at += end + 1;
            continue;
        }
        if rest.starts_with('&')
            && let Some(end) = rest.find(';').filter(|end| *end <= 32)
            && let Some(ch) = entity(&rest[1..end])
        {
            plain.push(ch);
            at += end + 1;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        plain.push(ch);
        at += ch.len_utf8();
    }
    let mut text = String::new();
    let mut pending_space = false;
    let mut length = 0;
    for ch in plain.chars() {
        let formatting = matches!(ch, '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}');
        if ch.is_whitespace() || ch.is_control() || formatting {
            pending_space = !text.is_empty();
            continue;
        }
        if length + usize::from(pending_space) + ch.len_utf16() > limit {
            break;
        }
        if pending_space {
            text.push(' ');
            length += 1;
            pending_space = false;
        }
        text.push(ch);
        length += ch.len_utf16();
    }
    text
}
pub fn metadata(html: &str) -> HashMap<String, String> {
    let lower = html.to_ascii_lowercase();
    let mut fields = HashMap::new();
    let mut at = 0;
    while let Some(offset) = lower[at..].find("<meta") {
        let start = at + offset;
        at = start + 5;
        if lower
            .as_bytes()
            .get(at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            continue;
        }
        let bound = (at + 8193).min(lower.len());
        let Some(end) = lower.as_bytes()[at..bound].iter().position(|b| *b == b'>') else {
            at = bound;
            while !lower.is_char_boundary(at) {
                at += 1;
            }
            continue;
        };
        let attrs = attributes(&html[start..at + end + 1]);
        at += end + 1;
        let name = attrs
            .get("property")
            .or_else(|| attrs.get("name"))
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(
            name.as_str(),
            "og:title"
                | "og:description"
                | "og:site_name"
                | "og:image"
                | "og:type"
                | "description"
                | "og:video"
                | "og:video:url"
                | "og:video:secure_url"
                | "og:video:type"
                | "og:video:width"
                | "og:video:height"
        ) {
            continue;
        }
        if let Some(value) = attrs.get("content") {
            let limit = if name == "og:image"
                || name == "og:video"
                || name.ends_with(":url")
                || name.ends_with(":secure_url")
            {
                4096
            } else if name.contains("description") {
                600
            } else {
                160
            };
            fields.entry(name).or_insert_with(|| clean(value, limit));
        }
    }
    if let Some((start, _)) = lower.match_indices("<title").find(|(at, _)| {
        lower
            .as_bytes()
            .get(at + 6)
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && *ch != b'_')
    }) && let Some(open) = lower[start..].find('>')
    {
        let text_start = start + open + 1;
        if let Some((end, _)) = lower[text_start..]
            .match_indices("</title")
            .find(|(at, _)| lower[text_start + at + 7..].trim_start().starts_with('>'))
        {
            fields.insert(
                "title".into(),
                clean(&html[text_start..text_start + end], 160),
            );
        }
    }
    fields
}
pub fn youtube_video_id(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let path = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy();
    let id = if host == "youtu.be" {
        path.strip_prefix('/')?.split('/').next()?.to_owned()
    } else if matches!(
        host,
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com"
    ) {
        if path == "/watch" {
            url.query_pairs()
                .find(|(name, _)| name == "v")?
                .1
                .into_owned()
        } else {
            let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
            if parts.len() != 2 || !matches!(parts[0], "shorts" | "live" | "embed") {
                return None;
            }
            parts[1].to_owned()
        }
    } else {
        return None;
    };
    (id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
    .then_some(id)
}
fn youtube_metadata(json: &[u8]) -> Result<HashMap<String, String>> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|_| "Invalid video metadata")?;
    let title = clean(
        value
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or("Missing video title")?,
        160,
    );
    if title.is_empty() {
        return Err("Missing video title");
    }
    let mut fields = HashMap::from([
        ("og:title".into(), title),
        ("og:site_name".into(), "YouTube".into()),
    ]);
    if let Some(image) = value
        .get("thumbnail_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        fields.insert("og:image".into(), image.into());
    }
    Ok(fields)
}
fn dimension(value: Option<&String>, fallback: u32) -> u32 {
    value
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=8192).contains(n))
        .unwrap_or(fallback)
}
fn direct_metadata(media: &Url, mime: &str, original: &Url) -> Result<HashMap<String, String>> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let host = original.host_str().unwrap_or_default();
    let site = if matches!(
        host,
        "kkinstagram.com" | "www.kkinstagram.com" | "kkclip.com" | "www.kkclip.com"
    ) {
        "KK Instagram"
    } else {
        host
    };
    let mut fields = HashMap::from([("og:site_name".into(), site.into())]);
    if matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp") {
        fields.insert("og:type".into(), "photo".into());
        fields.insert("og:image".into(), media.to_string());
    } else if matches!(mime.as_str(), "video/mp4" | "video/webm") {
        fields.insert("og:type".into(), "video".into());
        fields.insert("og:video".into(), media.to_string());
        fields.insert("og:video:type".into(), mime);
    } else {
        return Err("Unsupported preview media");
    }
    Ok(fields)
}
fn video_url(fields: &HashMap<String, String>, page: &Url) -> Option<Url> {
    let value = fields
        .get("og:video:secure_url")
        .or_else(|| fields.get("og:video:url"))
        .or_else(|| fields.get("og:video"))?;
    let url = safe_url(page.join(value).ok()?.as_str()).ok()?;
    let path = percent_encoding::percent_decode_str(url.path())
        .decode_utf8_lossy()
        .to_ascii_lowercase();
    (path.ends_with(".mp4") || path.ends_with(".webm")).then_some(url)
}
fn html_text(response: &Response) -> String {
    let charset = response
        .mime
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']))
        })
        .unwrap_or("UTF-8");
    if let Ok((bytes, _)) = glib::convert(&response.bytes, "UTF-8", charset) {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::from_utf8_lossy(&response.bytes).into_owned()
    }
}
struct DecodeCancel(gio::Cancellable);
impl Drop for DecodeCancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
/// Run inside a Tokio runtime. Dropping/cancelling the fetch stops network I/O and
/// cancels worker decoding; caller must also fence message identity before display.
pub async fn fetch(address: &str, permit: &Permit) -> Result<Preview> {
    permit.check()?;
    let cancel = DecodeCancel(gio::Cancellable::new());
    tokio::select! {
        biased;
        _ = permit.cancellation() => Err("Preview cancelled"),
        result = tokio::time::timeout(DEADLINE, fetch_inner(address,permit,&cancel.0)) => result.unwrap_or(Err("Preview deadline exceeded")),
    }
}
async fn fetch_inner(address: &str, permit: &Permit, cancel: &gio::Cancellable) -> Result<Preview> {
    let original = safe_url(address)?;
    let mut page_url = original.clone();
    let youtube = youtube_video_id(&original);
    let html = request(
        original.as_str(),
        if youtube.is_some() {
            1024 * 1024
        } else {
            HTML_LIMIT
        },
        Body::HtmlPrefix,
        permit,
    )
    .await;
    let mut fields = match html {
        Ok(response) => {
            let kind = response.mime.to_ascii_lowercase();
            if kind.starts_with("image/") || kind.starts_with("video/") {
                Some(direct_metadata(&response.url, &response.mime, &original)?)
            } else if kind.starts_with("text/html") || kind.starts_with("application/xhtml+xml") {
                let fields = metadata(&html_text(&response));
                if youtube.is_none() {
                    page_url = response.url;
                }
                if fields
                    .get("og:title")
                    .or_else(|| fields.get("title"))
                    .is_none_or(String::is_empty)
                    || youtube.is_some() && !fields.contains_key("og:title")
                {
                    None
                } else {
                    Some(fields)
                }
            } else {
                None
            }
        }
        Err(error) if youtube.is_none() => return Err(error),
        Err(_) => None,
    };
    if fields.is_none()
        && let Some(id) = &youtube
    {
        let endpoint = format!(
            "https://www.youtube.com/oembed?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D{id}&format=json"
        );
        let response = request(&endpoint, 64 * 1024, Body::Strict, permit).await?;
        if !response
            .mime
            .to_ascii_lowercase()
            .starts_with("application/json")
        {
            return Err("Invalid video metadata");
        }
        fields = Some(youtube_metadata(&response.bytes)?);
    }
    let fields = fields.ok_or("No preview metadata")?;
    let og_type = fields
        .get("og:type")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let kind = if youtube.is_some() || og_type == "video" || og_type.starts_with("video.") {
        Kind::Video
    } else if og_type == "photo" {
        Kind::Photo
    } else {
        Kind::Article
    };
    let direct_video = video_url(&fields, &page_url);
    let media = if let Some(id) = &youtube {
        Some(Media {
            url: format!("https://www.youtube.com/embed/{id}"),
            mime: "text/html".into(),
            width: 1280,
            height: 720,
        })
    } else {
        direct_video.as_ref().map(|url| Media {
            url: url.to_string(),
            mime: fields
                .get("og:video:type")
                .cloned()
                .unwrap_or_else(|| "video/mp4".into()),
            width: dimension(fields.get("og:video:width"), 1280),
            height: dimension(fields.get("og:video:height"), 720),
        })
    };
    let mut page = Preview {
        url: page_url.to_string(),
        display_url: page_url.host_str().unwrap_or_default().into(),
        site_name: fields
            .get("og:site_name")
            .cloned()
            .unwrap_or_else(|| page_url.host_str().unwrap_or_default().into()),
        title: fields
            .get("og:title")
            .or_else(|| fields.get("title"))
            .cloned()
            .unwrap_or_default(),
        description: fields
            .get("og:description")
            .or_else(|| fields.get("description"))
            .cloned()
            .unwrap_or_default(),
        kind: if direct_video.is_some() && youtube.is_none() {
            Kind::Video
        } else {
            kind
        },
        thumbnail: None,
        media,
    };
    let image = fields.get("og:image").filter(|s| !s.is_empty());
    if image.is_some() || direct_video.is_some() {
        // Optional image failures preserve a useful text card and its original clickable link.
        if let Ok(thumbnail) = thumbnail(
            &original,
            &page_url,
            image,
            direct_video.as_ref(),
            permit,
            cancel,
        )
        .await
        {
            if direct_video.is_some()
                && let Some(media) = page.media.as_mut()
            {
                media.width = thumbnail.width;
                media.height = thumbnail.height;
            }
            page.thumbnail = Some(thumbnail);
        }
    }
    permit.check()?;
    if page.title.is_empty() && page.description.is_empty() && page.thumbnail.is_none() {
        return Err("No preview metadata");
    }
    Ok(page)
}

pub fn image_root(owner: &str) -> Result<PathBuf> {
    let root = crate::downloads::account_root(owner)?;
    Ok(root
        .parent()
        .and_then(|p| p.parent())
        .ok_or("Preview cache unavailable")?
        .join("link-previews")
        .join(root.file_name().ok_or("Preview cache unavailable")?))
}
fn image_path(owner: &str, address: &str) -> Result<PathBuf> {
    let key = openssl::sha::sha256(address.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(image_root(owner)?.join(format!("{key}.png")))
}
fn directory(owner: &str) -> Result<PathBuf> {
    let dir = image_root(owner)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|_| "Preview cache unavailable")?;
    Ok(dir)
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "Preview cache unavailable")?;
    output
        .write_all(bytes)
        .map_err(|_| "Preview cache unavailable")
}
async fn thumbnail(
    original: &Url,
    page: &Url,
    image: Option<&String>,
    video: Option<&Url>,
    permit: &Permit,
    cancel: &gio::Cancellable,
) -> Result<Thumbnail> {
    permit.check()?;
    let is_video = image.is_none();
    let response = if let Some(image) = image {
        let url = safe_url(
            page.join(image)
                .map_err(|_| "Invalid preview image")?
                .as_str(),
        )?;
        let response = request(url.as_str(), IMAGE_LIMIT, Body::Strict, permit).await?;
        if !matches!(
            response
                .mime
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "image/jpeg" | "image/png" | "image/webp"
        ) {
            return Err("Unsupported preview image");
        }
        response
    } else {
        let response = request(
            video.ok_or("Preview image unavailable")?.as_str(),
            IMAGE_LIMIT,
            Body::MediaPrefix,
            permit,
        )
        .await?;
        if !response.mime.to_ascii_lowercase().starts_with("video/") {
            return Err("Unsupported preview video");
        }
        response
    };
    let permit = permit.clone();
    let cancel = cancel.clone();
    let address = original.to_string();
    tokio::task::spawn_blocking(move || {
        permit.check()?;
        let encoded = if is_video {
            let temporary = {
                let _guard = crate::downloads::CACHE_IO.lock().unwrap();
                permit.check()?;
                let dir = directory(permit.owner())?;
                let part = Temporary(dir.join(format!(".{}.mp4", uuid::Uuid::new_v4())));
                write_private(&part.0, &response.bytes)?;
                part
            };
            crate::image_loader::encode_video_preview(&temporary.0, &cancel, 3000)?
        } else {
            crate::image_loader::encode_link_preview(response.bytes, &cancel)?
        };
        permit.check()?;
        if cancel.is_cancelled() || encoded.bytes.len() > IMAGE_LIMIT {
            return Err("Preview cancelled");
        }
        let _guard = crate::downloads::CACHE_IO.lock().unwrap();
        permit.check()?;
        let dir = directory(permit.owner())?;
        let path = image_path(permit.owner(), &address)?;
        let temporary = Temporary(dir.join(format!(".{}.tmp", uuid::Uuid::new_v4())));
        write_private(&temporary.0, &encoded.bytes)?;
        permit.check()?;
        if cancel.is_cancelled() {
            return Err("Preview cancelled");
        }
        fs::rename(&temporary.0, &path).map_err(|_| "Preview cache unavailable")?;
        trim_images(&dir);
        permit.check()?;
        Ok(Thumbnail {
            path,
            width: encoded.width,
            height: encoded.height,
        })
    })
    .await
    .map_err(|_| "Preview image unavailable")?
}
fn trim_images(dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "png") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((
                metadata.modified().unwrap_or(UNIX_EPOCH),
                metadata.len(),
                path,
            ))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|file| std::cmp::Reverse(file.0));
    let mut bytes = 0u64;
    for (index, (_, length, path)) in files.into_iter().enumerate() {
        bytes = bytes.saturating_add(length);
        if index >= 128 || bytes > 16 * 1024 * 1024 {
            let _ = fs::remove_file(path);
        }
    }
}
/// Worker-only after cancelling this account's Privacy. Also used on account removal.
pub fn forget(owner: &str) -> Result<()> {
    let _guard = crate::downloads::CACHE_IO.lock().unwrap();
    let root = image_root(owner)?;
    if root.exists() {
        fs::remove_dir_all(root).map_err(|_| "Preview cache could not be removed")?;
    }
    Ok(())
}
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
impl Store {
    /// Expired positive entries remain visible, including when fetching is disabled.
    pub fn cached_link_preview(&self, owner: &str, address: &str) -> Option<Preview> {
        let url = safe_url(address).ok()?;
        let body: Option<String> = self
            .db
            .query_row(
                "SELECT body FROM link_previews WHERE account=? AND url=?",
                params![owner, url.as_str()],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()?;
        let mut preview: Preview = serde_json::from_str(&body?).ok()?;
        safe_url(&preview.url).ok()?;
        if preview.thumbnail.as_ref().is_some_and(|thumb| {
            image_path(owner, url.as_str()).ok().as_ref() != Some(&thumb.path)
                || !thumb.path.is_file()
                || thumb.width == 0
                || thumb.height == 0
                || thumb.width > 640
                || thumb.height > 640
        }) {
            preview.thumbnail = None;
        }
        if let Some(media) = preview.media.as_ref() {
            let valid = safe_url(&media.url).is_ok()
                && if media.mime == "text/html" {
                    youtube_video_id(&url).is_some_and(|id| {
                        media.url == format!("https://www.youtube.com/embed/{id}")
                    })
                } else {
                    safe_url(&media.url).is_ok_and(|url| {
                        let path = percent_encoding::percent_decode_str(url.path())
                            .decode_utf8_lossy()
                            .to_ascii_lowercase();
                        path.ends_with(".mp4") || path.ends_with(".webm")
                    })
                };
            if !valid {
                preview.media = None;
            }
        }
        Some(preview)
    }
    pub fn link_preview_expiry(&self, owner: &str, address: &str) -> Option<i64> {
        let url = safe_url(address).ok()?;
        self.db
            .query_row(
                "SELECT expires FROM link_previews WHERE account=? AND url=?",
                params![owner, url.as_str()],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }
    pub fn link_preview_fresh(&self, owner: &str, address: &str, now: i64) -> bool {
        let Ok(url) = safe_url(address) else {
            return false;
        };
        self.db
            .query_row(
                "SELECT expires>? FROM link_previews WHERE account=? AND url=?",
                params![now, owner, url.as_str()],
                |row| row.get(0),
            )
            .unwrap_or(false)
    }
    /// Commit on the account owner executor, after checking the message's current URL.
    pub fn save_link_preview(
        &self,
        owner: &str,
        address: &str,
        preview: Option<&Preview>,
        now: i64,
        permit: &Permit,
    ) -> rusqlite::Result<bool> {
        if permit.owner() != owner || !permit.allowed() {
            return Ok(false);
        }
        let Ok(url) = safe_url(address) else {
            return Ok(false);
        };
        let body = preview
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let expires = now.saturating_add(if preview.is_some() {
            POSITIVE_MS
        } else {
            NEGATIVE_MS
        });
        let tx = self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO link_previews VALUES(?,?,?,?) ON CONFLICT(account,url) DO UPDATE SET expires=excluded.expires,body=excluded.body",params![owner,url.as_str(),expires,body])?;
        tx.execute("DELETE FROM link_previews WHERE account=?1 AND url NOT IN(SELECT url FROM link_previews WHERE account=?1 ORDER BY expires DESC,url LIMIT 256)",[owner])?;
        if !permit.allowed() {
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }
    /// Cancel the account Privacy first; purge its private images with forget() off GTK.
    pub fn clear_link_previews(&self, owner: &str) -> rusqlite::Result<()> {
        self.db
            .execute("DELETE FROM link_previews WHERE account=?", [owner])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clickable_spans_preserve_unicode_urls_and_exclude_code_and_spoilers() {
        let body = "😀 (https://example.org/a(b)). http://localhost:3000/#part example.org?q=a&b=2 `https://hidden.org/` me@example.org javascript:alert(1)";
        let links = clickable_urls(body, None);
        assert_eq!(
            links
                .iter()
                .map(|link| &body[link.range.clone()])
                .collect::<Vec<_>>(),
            [
                "https://example.org/a(b)",
                "http://localhost:3000/#part",
                "example.org?q=a&b=2"
            ]
        );
        assert_eq!(links[1].uri, "http://localhost:3000/#part");
        assert_eq!(links[2].uri, "https://example.org/?q=a&b=2");
        let hidden = &links[1].range;
        let spoilers = crate::spoilers::Context {
            whole: false,
            ranges: vec![crate::spoilers::Range {
                start: body[..hidden.start].chars().count(),
                end: body[..hidden.end].chars().count(),
            }],
            text_hash: Some(openssl::sha::sha256(body.as_bytes())),
        };
        assert_eq!(clickable_urls(body, Some(&spoilers)).len(), 2);
        assert_eq!(
            clickable_urls("http://[::1]:3000/", None)[0].uri,
            "http://[::1]:3000/"
        );
        assert!(
            clickable_urls("http://user:pass@example.org/ ftp://example.org/", None).is_empty()
        );
        assert!(clickable_urls("```\nhttps://hidden.org/", None).is_empty());
        assert_eq!(
            first_url("`https://hidden.org/` https://example.org/"),
            Some("https://example.org/".into())
        );
        assert!(clickable_urls(&"a(".repeat(32769), None).is_empty());
    }
    #[test]
    fn bounded_metadata_destination_cache_and_cancellation_check() {
        assert_eq!(
            first_url(
                "mail me@example.org; aesgcm://files.example.org/x#key https://example.org/watch?q=one#secret https://later.org"
            ),
            Some("https://example.org/watch?q=one".into())
        );
        assert!(first_url(&"a(".repeat(32768)).is_none());
        assert_eq!(
            first_url("Read (example.org), then later.org"),
            Some("https://example.org/".into())
        );
        assert_eq!(
            first_url("Read example.org/a."),
            Some("https://example.org/a".into())
        );
        for bad in [
            "http://example.org",
            "https://localhost/",
            "https://example.org:444/",
            "https://user:pass@example.org/",
            "https://@example.org/",
            "https:////example.org/",
            "https://example.org/%zz",
            "https://example.org./",
            "https://example.org\\@evil.org/",
        ] {
            assert!(safe_url(bad).is_err(), "{bad}");
        }
        for bad in [
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.2.1",
            "172.31.255.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "::ffff:8.8.8.8",
            "2001:db8::1",
            "2001:100::1",
            "2002::1",
            "3fff::1",
        ] {
            assert!(!public_address(bad.parse().unwrap()), "{bad}");
        }
        for good in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_address(good.parse().unwrap()), "{good}");
        }
        let fields = metadata(
            "<META CONTENT='First &amp; &#x1f600;' PROPERTY=og:title><meta property='og:title' content='Second'><meta name=description content='A &quot;bounded&quot; description'><meta property=og:image content='/image.png?x=1&amp;y=2'><title>Fallback</title>",
        );
        assert_eq!(fields["og:title"], "First & 😀");
        assert_eq!(fields["description"], "A \"bounded\" description");
        assert_eq!(fields["og:image"], "/image.png?x=1&y=2");
        let youtube = safe_url("https://youtu.be/dQw4w9WgXcQ#secret").unwrap();
        assert_eq!(youtube_video_id(&youtube).as_deref(), Some("dQw4w9WgXcQ"));
        let fallback = youtube_metadata(br#"{"title":"Video", "author_name":"Never a description", "thumbnail_url":"https://example.org/thumb.jpg"}"#).unwrap();
        assert!(!fallback.contains_key("og:description"));
        assert!(
            video_url(
                &HashMap::from([("og:video".into(), "https://evil.org/player.html".into())]),
                &youtube
            )
            .is_none()
        );
        assert_eq!(
            direct_metadata(
                &safe_url("https://example.org/movie.mp4").unwrap(),
                "video/mp4",
                &youtube
            )
            .unwrap()["og:type"],
            "video"
        );
        let owner = "owner@example.invalid";
        let other = "other@example.invalid";
        let privacy = Privacy::new(owner, false, false);
        assert!(privacy.ticket(false).is_none());
        privacy.set_permissions(true, false);
        assert!(privacy.ticket(true).is_none());
        let stale = privacy.ticket(false).unwrap();
        privacy.set_permissions(true, true);
        assert!(!stale.allowed());
        let permit = privacy.ticket(true).unwrap();
        let store = Store::open(true).unwrap();
        store.init_link_previews().unwrap();
        for account in [owner, other] {
            store
                .save_account(&crate::model::Account {
                    jid: account.into(),
                    host: String::new(),
                    port: 5222,
                    direct_tls: false,
                })
                .unwrap();
        }
        let page = Preview {
            url: "https://example.org/".into(),
            display_url: "example.org".into(),
            site_name: "Example".into(),
            title: "Title".into(),
            description: "Description".into(),
            kind: Kind::Article,
            thumbnail: None,
            media: None,
        };
        assert!(
            store
                .save_link_preview(owner, &page.url, Some(&page), 1000, &permit)
                .unwrap()
        );
        assert!(store.link_preview_fresh(owner, &page.url, 1000 + POSITIVE_MS - 1));
        assert!(!store.link_preview_fresh(owner, &page.url, 1000 + POSITIVE_MS));
        assert!(store.cached_link_preview(other, &page.url).is_none());
        assert!(
            !store
                .save_link_preview(other, &page.url, Some(&page), 1000, &permit)
                .unwrap()
        );
        assert!(
            store
                .save_link_preview(owner, "https://failure.org/", None, 1000, &permit)
                .unwrap()
        );
        assert!(store.link_preview_fresh(owner, "https://failure.org/", 1000 + NEGATIVE_MS - 1));
        assert!(!store.link_preview_fresh(owner, "https://failure.org/", 1000 + NEGATIVE_MS));
        for index in 0..260 {
            store
                .save_link_preview(
                    owner,
                    &format!("https://example.org/{index}"),
                    None,
                    1000 + index,
                    &permit,
                )
                .unwrap();
        }
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT count(*) FROM link_previews WHERE account=?",
                    [owner],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            256
        );
        privacy.set_permissions(false, false);
        assert!(
            !store
                .save_link_preview(owner, "https://cancelled.org/", None, 1000, &permit)
                .unwrap()
        );
        assert!(store.cached_link_preview(owner, &page.url).is_some());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(
                tokio::time::timeout(Duration::from_millis(100), permit.cancellation())
                    .await
                    .is_ok()
            );
            assert_eq!(
                fetch("https://example.org/", &permit).await.unwrap_err(),
                "Preview cancelled"
            );
        });
        store.clear_link_previews(owner).unwrap();
        assert!(store.cached_link_preview(owner, &page.url).is_none());
        let native = Message {
            attachment: Some(
                crate::file_share::Attachment::bare("https://example.org/image.png").unwrap(),
            ),
            text: "https://example.org".into(),
            ..Default::default()
        };
        assert!(candidate(&native).is_none());
        let text = "😀 unrelated https://first.example.org hidden https://second.example.org";
        let begin = text[..text.find("https://").unwrap()].chars().count();
        let mut hidden = Message {
            text: text.into(),
            preview_spoilers: Some(crate::spoilers::Context {
                whole: false,
                ranges: vec![crate::spoilers::Range {
                    start: begin,
                    end: begin + "https://first.example.org".len(),
                }],
                text_hash: Some(openssl::sha::sha256(text.as_bytes())),
            }),
            ..Default::default()
        };
        assert!(
            candidate(&hidden).is_none(),
            "never fall through to a later URL"
        );
        hidden.preview_spoilers.as_mut().unwrap().ranges =
            vec![crate::spoilers::Range { start: 0, end: 1 }];
        assert_eq!(
            candidate(&hidden).as_deref(),
            Some("https://first.example.org/")
        );
        hidden.text.push_str(" edited");
        assert!(
            candidate(&hidden).is_none(),
            "stale context cannot authorize fetching"
        );
        hidden.preview_spoilers = None;
        assert!(
            candidate(&hidden).is_none(),
            "unknown historical markup remains unknown"
        );
    }
}
