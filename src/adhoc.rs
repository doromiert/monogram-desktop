//! Ad-hoc commands (XEP-0050) with data forms (XEP-0004), and the invite link a server can create
//! through them (XEP-0401). This module is the protocol half: building requests and parsing
//! answers, with hard bounds on everything a server can send. `adhoc_ui.rs` shows the result.
//!
//! Only commands the account's own server offers are listed: a request is addressed to the server
//! domain, so an item that names another address is ignored rather than followed.
use std::collections::BTreeMap;
use tokio_xmpp::minidom::Element;

pub const NS: &str = "http://jabber.org/protocol/commands";
pub const ITEMS: &str = "http://jabber.org/protocol/disco#items";
pub const DATA: &str = "jabber:x:data";
/// XEP-0401: the command that creates an invitation to register and add the inviter.
pub const INVITE_NODE: &str = "urn:xmpp:invite#invite";

const MAX_COMMANDS: usize = 128;
const MAX_FIELDS: usize = 64;
const MAX_OPTIONS: usize = 128;
const MAX_VALUES: usize = 64;
const MAX_NOTES: usize = 16;
const MAX_NODE: usize = 256;
const MAX_SESSION: usize = 256;
const MAX_LABEL: usize = 200;
const MAX_TEXT: usize = 4096;
const MAX_URI: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub node: String,
    pub name: String,
}

/// `disco#items` of the commands node, asked of the server.
pub fn list_request() -> Element {
    Element::builder("query", ITEMS)
        .attr("node".try_into().unwrap(), NS)
        .build()
}

/// The commands `server` offers. Items for any other address, without a node, or past the bound
/// are dropped; a missing name falls back to the node.
pub fn parse_commands(query: &Element, server: &str) -> Vec<Command> {
    if !query.is("query", ITEMS) {
        return Vec::new();
    }
    let mut seen = std::collections::BTreeSet::new();
    query
        .children()
        .filter(|item| item.is("item", ITEMS))
        .filter(|item| item.attr("jid") == Some(server))
        .filter_map(|item| {
            let node = item.attr("node")?;
            if node.is_empty() || node.len() > MAX_NODE || has_control(node) || !seen.insert(node) {
                return None;
            }
            let name = item.attr("name").map(clean).filter(|name| !name.is_empty());
            Some(Command {
                node: node.into(),
                name: name.unwrap_or_else(|| clean(node)),
            })
        })
        .take(MAX_COMMANDS)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Execute,
    Next,
    Prev,
    Complete,
    Cancel,
}
impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "execute" => Self::Execute,
            "next" => Self::Next,
            "prev" => Self::Prev,
            "complete" => Self::Complete,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
}

/// The first request of a command.
pub fn execute(node: &str) -> Element {
    command(node, None, Action::Execute, None)
}

/// A later request: answer the current step, move on, or give up. `session` is the one the server
/// chose; `form` is a [`submit`] answer.
pub fn step(node: &str, session: &str, action: Action, form: Option<Element>) -> Element {
    command(node, Some(session), action, form)
}

fn command(node: &str, session: Option<&str>, action: Action, form: Option<Element>) -> Element {
    let mut command = Element::builder("command", NS)
        .attr("node".try_into().unwrap(), node)
        .attr("action".try_into().unwrap(), action.name());
    if let Some(session) = session {
        command = command.attr("sessionid".try_into().unwrap(), session);
    }
    if let Some(form) = form {
        command = command.append(form);
    }
    command.build()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Executing,
    Completed,
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// `info`, `warn` or `error`.
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Private,
    Multi,
    Boolean,
    ListSingle,
    ListMulti,
    Jid,
    JidMulti,
    Fixed,
    Hidden,
}
impl Kind {
    fn parse(value: Option<&str>) -> Option<Self> {
        Some(match value.unwrap_or("text-single") {
            "text-single" => Self::Text,
            "text-private" => Self::Private,
            "text-multi" => Self::Multi,
            "boolean" => Self::Boolean,
            "list-single" => Self::ListSingle,
            "list-multi" => Self::ListMulti,
            "jid-single" => Self::Jid,
            "jid-multi" => Self::JidMulti,
            "fixed" => Self::Fixed,
            "hidden" => Self::Hidden,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Self::Text => "text-single",
            Self::Private => "text-private",
            Self::Multi => "text-multi",
            Self::Boolean => "boolean",
            Self::ListSingle => "list-single",
            Self::ListMulti => "list-multi",
            Self::Jid => "jid-single",
            Self::JidMulti => "jid-multi",
            Self::Fixed => "fixed",
            Self::Hidden => "hidden",
        }
    }
    /// Whether the user can change the field.
    pub fn editable(self) -> bool {
        !matches!(self, Self::Fixed | Self::Hidden)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub kind: Kind,
    /// Empty for a `fixed` field, which is only a label.
    pub var: String,
    pub label: String,
    pub values: Vec<String>,
    /// `(value, label)` for list fields.
    pub options: Vec<(String, String)>,
    pub required: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    pub instructions: Vec<String>,
    pub fields: Vec<Field>,
    /// A `result` form is shown, never filled in.
    pub result: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub status: Status,
    pub session: Option<String>,
    /// What the server allows next, besides cancelling.
    pub actions: Vec<Action>,
    pub default: Option<Action>,
    pub notes: Vec<Note>,
    pub form: Option<Form>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not a `command` element for the node that was asked.
    WrongCommand,
    /// An unknown status, a missing or oversized session, or a form with a field this client
    /// cannot send back correctly.
    Invalid,
}

/// Read the server's answer to a command request for `node`.
pub fn parse_step(command: &Element, node: &str) -> Result<Step, ParseError> {
    if !command.is("command", NS) || command.attr("node") != Some(node) {
        return Err(ParseError::WrongCommand);
    }
    let status = match command.attr("status") {
        Some("executing") => Status::Executing,
        Some("completed") => Status::Completed,
        Some("canceled") => Status::Canceled,
        _ => return Err(ParseError::Invalid),
    };
    let session = command
        .attr("sessionid")
        .filter(|id| !id.is_empty() && id.len() <= MAX_SESSION && !has_control(id))
        .map(str::to_owned);
    if status == Status::Executing && session.is_none() {
        return Err(ParseError::Invalid);
    }
    let mut actions = Vec::new();
    let mut default = None;
    if let Some(list) = command.children().find(|c| c.is("actions", NS)) {
        default = list.attr("execute").and_then(Action::parse);
        for child in list.children().filter(|c| c.ns() == NS) {
            if let Some(action) = Action::parse(child.name()).filter(|a| {
                matches!(a, Action::Next | Action::Prev | Action::Complete)
            }) {
                if !actions.contains(&action) {
                    actions.push(action);
                }
            }
        }
    }
    let notes = command
        .children()
        .filter(|c| c.is("note", NS))
        .take(MAX_NOTES)
        .map(|note| Note {
            kind: match note.attr("type") {
                Some(kind @ ("warn" | "error")) => kind.into(),
                _ => "info".into(),
            },
            text: clean_text(&note.text()),
        })
        .filter(|note| !note.text.is_empty())
        .collect();
    let form = match command.children().find(|c| c.is("x", DATA)) {
        Some(x) => Some(parse_form(x)?),
        None => None,
    };
    Ok(Step {
        status,
        session,
        actions,
        default,
        notes,
        form,
    })
}

fn parse_form(x: &Element) -> Result<Form, ParseError> {
    let result = match x.attr("type") {
        Some("form") => false,
        Some("result") => true,
        _ => return Err(ParseError::Invalid),
    };
    let mut form = Form {
        title: x
            .children()
            .find(|c| c.is("title", DATA))
            .map(|c| clean(&c.text()))
            .unwrap_or_default(),
        instructions: x
            .children()
            .filter(|c| c.is("instructions", DATA))
            .take(MAX_NOTES)
            .map(|c| clean_text(&c.text()))
            .filter(|text| !text.is_empty())
            .collect(),
        result,
        ..Default::default()
    };
    for field in x.children().filter(|c| c.is("field", DATA)) {
        if form.fields.len() >= MAX_FIELDS {
            return Err(ParseError::Invalid);
        }
        let Some(kind) = Kind::parse(field.attr("type")) else {
            return Err(ParseError::Invalid);
        };
        let var = field.attr("var").unwrap_or("");
        if (kind != Kind::Fixed && var.is_empty()) || var.len() > MAX_NODE || has_control(var) {
            return Err(ParseError::Invalid);
        }
        let values: Vec<String> = field
            .children()
            .filter(|c| c.is("value", DATA))
            .take(MAX_VALUES)
            .map(|c| clean_text(&c.text()))
            .collect();
        let options = field
            .children()
            .filter(|c| c.is("option", DATA))
            .take(MAX_OPTIONS)
            .filter_map(|option| {
                let value = option.children().find(|c| c.is("value", DATA))?.text();
                let label = option.attr("label").map(clean).filter(|l| !l.is_empty());
                Some((clean_text(&value), label.unwrap_or_else(|| clean(&value))))
            })
            .collect();
        form.fields.push(Field {
            kind,
            var: var.into(),
            label: field
                .attr("label")
                .map(clean)
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| clean(var)),
            values,
            options,
            required: field.children().any(|c| c.is("required", DATA)),
        });
    }
    Ok(form)
}

/// What to tell the user when a command request failed.
pub fn error_text(error: &crate::iq::Error) -> String {
    use crate::iq::Error;
    match error {
        Error::Rejected { condition, .. } => match condition.as_str() {
            "item-not-found" | "service-unavailable" | "feature-not-implemented" => {
                "This server does not offer that command."
            }
            "forbidden" | "not-allowed" | "not-authorized" => {
                "The server does not let this account run that command."
            }
            "bad-request" | "jid-malformed" => "The server did not accept that request.",
            _ => "The server could not run that command.",
        },
        Error::Timeout => "The server did not answer in time. Try again.",
        Error::Closed => "The connection closed. Connect and try again.",
        Error::Busy => "Too many requests are waiting. Try again in a moment.",
        Error::Invalid => "The server's answer could not be used.",
    }
    .into()
}

/// Whether a boolean field's value reads as true (`1` or `true`).
pub fn truthy(value: &str) -> bool {
    matches!(value.trim(), "1" | "true")
}

/// The titles of required fields the user left empty.
pub fn missing_required(form: &Form, values: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    form.fields
        .iter()
        .filter(|field| field.required && field.kind.editable() && field.kind != Kind::Boolean)
        .filter(|field| {
            values
                .get(&field.var)
                .is_none_or(|v| v.iter().all(|value| value.trim().is_empty()))
        })
        .map(|field| field.label.clone())
        .collect()
}

/// The answer to a `form`: every field the user can edit with what they entered, and hidden fields
/// as the server sent them (they carry its state). Fixed fields are only labels and are not sent.
/// A field the user did not touch keeps the server's default.
pub fn submit(form: &Form, values: &BTreeMap<String, Vec<String>>) -> Element {
    let mut x = Element::builder("x", DATA).attr("type".try_into().unwrap(), "submit");
    for field in form.fields.iter().filter(|field| field.kind != Kind::Fixed) {
        let entered = if field.kind == Kind::Hidden {
            &field.values
        } else {
            values.get(&field.var).unwrap_or(&field.values)
        };
        let mut element = Element::builder("field", DATA)
            .attr("var".try_into().unwrap(), &field.var)
            .attr("type".try_into().unwrap(), field.kind.name());
        for value in entered.iter().take(MAX_VALUES) {
            let value: String = value.chars().take(MAX_TEXT).collect();
            element = element.append(Element::builder("value", DATA).append(value).build());
        }
        x = x.append(element.build());
    }
    x.build()
}

/// The invitation a completed `urn:xmpp:invite#invite` command returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// `xmpp:…?register;preauth=…`, for a client that handles invitations.
    pub uri: Option<String>,
    /// A web page that explains the invitation to someone without a client.
    pub landing: Option<String>,
    pub expires: Option<String>,
}

/// The invitation in a completed step. Only an `xmpp:` URI and an `https://` page are accepted,
/// with no whitespace or control characters, so nothing else can be passed off as a link.
pub fn invite(step: &Step) -> Option<Invite> {
    if step.status != Status::Completed {
        return None;
    }
    let form = step.form.as_ref()?;
    let value = |var: &str| {
        form.fields
            .iter()
            .find(|field| field.var == var)
            .and_then(|field| field.values.first())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let uri = value("uri").filter(|uri| safe_link(uri, "xmpp:"));
    let landing = value("landing-url").filter(|url| safe_link(url, "https://"));
    if uri.is_none() && landing.is_none() {
        return None;
    }
    Some(Invite {
        uri,
        landing,
        expires: value("expire").map(|value| clean(&value)),
    })
}

impl Invite {
    /// When the invitation stops working, if the server said and the time is readable.
    pub fn expires_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let value = self.expires.as_deref()?.trim();
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|time| time.with_timezone(&chrono::Utc))
    }
}

/// How long until `expires`, in words: "Expires in 6 days", "Expires in 12 minutes", "Expired 3
/// hours ago". It changes with `now`, so show it from a timer rather than once.
pub fn expiry_text(expires: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let delta = expires - now;
    // Judged on the exact instant: a fraction of a second left is still "not expired".
    let (amount, past) = (delta.num_seconds().abs(), delta <= chrono::Duration::zero());
    let span = if amount < 60 {
        if past {
            return "Expired just now".into();
        }
        return "Expires in less than a minute".into();
    } else if amount < 3600 {
        plural(amount / 60, "minute")
    } else if amount < 2 * 86_400 {
        plural(amount / 3600, "hour")
    } else {
        plural(amount / 86_400, "day")
    };
    if past {
        format!("Expired {span} ago")
    } else {
        format!("Expires in {span}")
    }
}

fn plural(count: i64, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

fn safe_link(value: &str, scheme: &str) -> bool {
    value.len() <= MAX_URI
        && value.len() > scheme.len()
        && value.starts_with(scheme)
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// One line of display text: controls removed, bounded.
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Display text that may span lines: tabs and newlines kept, other controls removed, bounded.
fn clean_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(MAX_TEXT)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Element {
        xml.parse().unwrap()
    }
    fn items(xml: &str) -> Element {
        parse(&format!("<query xmlns='{ITEMS}' node='{NS}'>{xml}</query>"))
    }
    fn answer(status: &str, inner: &str) -> Element {
        parse(&format!(
            "<command xmlns='{NS}' node='urn:x#n' sessionid='s1' status='{status}'>{inner}</command>"
        ))
    }
    fn form(fields: &str) -> String {
        format!("<x xmlns='{DATA}' type='form'><title>T</title><instructions>Do it</instructions>{fields}</x>")
    }
    fn step_of(status: &str, inner: &str) -> Step {
        parse_step(&answer(status, inner), "urn:x#n").unwrap()
    }

    #[test]
    fn the_command_list_is_asked_of_the_commands_node() {
        let request = list_request();
        assert!(request.is("query", ITEMS));
        assert_eq!(request.attr("node"), Some(NS));
    }

    #[test]
    fn only_the_servers_own_commands_are_listed() {
        let query = items(&format!(
            "<item xmlns='{ITEMS}' jid='example.com' node='urn:xmpp:invite#invite' name='Invite a friend'/>\
             <item xmlns='{ITEMS}' jid='example.com' node='announce'/>\
             <item xmlns='{ITEMS}' jid='evil.example' node='steal' name='Other host'/>\
             <item xmlns='{ITEMS}' jid='example.com' name='No node'/>\
             <item xmlns='{ITEMS}' jid='example.com' node='announce' name='Duplicate'/>"
        ));
        assert_eq!(
            parse_commands(&query, "example.com"),
            vec![
                Command { node: INVITE_NODE.into(), name: "Invite a friend".into() },
                Command { node: "announce".into(), name: "announce".into() },
            ]
        );
    }

    #[test]
    fn a_hostile_command_list_is_bounded_and_cleaned() {
        let mut xml = String::new();
        for n in 0..500 {
            xml.push_str(&format!("<item xmlns='{ITEMS}' jid='example.com' node='n{n}'/>"));
        }
        xml.push_str(&format!(
            "<item xmlns='{ITEMS}' jid='example.com' node='{}'/>",
            "a".repeat(300)
        ));
        let mut query = items(&xml);
        query.append_child(
            Element::builder("item", ITEMS)
                .attr("jid".try_into().unwrap(), "example.com")
                .attr("node".try_into().unwrap(), "ctl\u{7}x")
                .build(),
        );
        let commands = parse_commands(&query, "example.com");
        assert_eq!(commands.len(), MAX_COMMANDS);
        assert!(commands.iter().all(|c| !has_control(&c.node) && c.node.len() <= MAX_NODE));
        let wrong_root = parse("<query xmlns='urn:other'/>");
        assert!(parse_commands(&wrong_root, "example.com").is_empty());
        let long = items(&format!(
            "<item xmlns='{ITEMS}' jid='example.com' node='a' name='{}'/>",
            "x".repeat(1000)
        ));
        assert_eq!(parse_commands(&long, "example.com")[0].name.len(), MAX_LABEL);
    }

    #[test]
    fn requests_carry_the_node_action_session_and_form() {
        let first = execute("urn:x#n");
        assert!(first.is("command", NS));
        assert_eq!(first.attr("node"), Some("urn:x#n"));
        assert_eq!(first.attr("action"), Some("execute"));
        assert_eq!(first.attr("sessionid"), None, "the server chooses the session");

        let answer = Element::builder("x", DATA).build();
        let next = step("urn:x#n", "s1", Action::Next, Some(answer));
        assert_eq!(next.attr("action"), Some("next"));
        assert_eq!(next.attr("sessionid"), Some("s1"));
        assert!(next.children().next().unwrap().is("x", DATA));

        let cancel = step("urn:x#n", "s1", Action::Cancel, None);
        assert_eq!(cancel.attr("action"), Some("cancel"));
        assert_eq!(cancel.children().count(), 0);
    }

    #[test]
    fn a_step_is_read_with_its_actions_notes_and_form() {
        let s = step_of(
            "executing",
            &format!(
                "<actions xmlns='{NS}' execute='next'><next/><prev/><complete/></actions>\
                 <note xmlns='{NS}' type='warn'>Careful</note>\
                 <note xmlns='{NS}'>  </note>{}",
                form("<field xmlns='jabber:x:data' var='who' type='jid-single' label='Who'><required/><value>a@b</value></field>")
            ),
        );
        assert_eq!(s.status, Status::Executing);
        assert_eq!(s.session.as_deref(), Some("s1"));
        assert_eq!(s.actions, vec![Action::Next, Action::Prev, Action::Complete]);
        assert_eq!(s.default, Some(Action::Next));
        assert_eq!(s.notes, vec![Note { kind: "warn".into(), text: "Careful".into() }]);
        let form = s.form.unwrap();
        assert_eq!((form.title.as_str(), form.instructions.clone()), ("T", vec!["Do it".to_string()]));
        assert!(!form.result);
        assert_eq!(form.fields.len(), 1);
        let field = &form.fields[0];
        assert_eq!((field.kind, field.var.as_str(), field.label.as_str()), (Kind::Jid, "who", "Who"));
        assert!(field.required);
        assert_eq!(field.values, vec!["a@b"]);
    }

    #[test]
    fn list_fields_keep_their_options() {
        let s = step_of(
            "executing",
            &form("<field xmlns='jabber:x:data' var='role' type='list-single'>\
                   <option label='Admin'><value>admin</value></option>\
                   <option><value>user</value></option></field>"),
        );
        let field = &s.form.unwrap().fields[0];
        assert_eq!(
            field.options,
            vec![("admin".into(), "Admin".into()), ("user".into(), "user".into())]
        );
    }

    #[test]
    fn a_missing_field_type_means_text_and_an_unknown_one_is_refused() {
        let s = step_of("executing", &form("<field xmlns='jabber:x:data' var='a'/>"));
        assert_eq!(s.form.unwrap().fields[0].kind, Kind::Text);
        let bad = answer("executing", &form("<field xmlns='jabber:x:data' var='a' type='spooky'/>"));
        assert_eq!(parse_step(&bad, "urn:x#n"), Err(ParseError::Invalid));
        let unnamed = answer("executing", &form("<field xmlns='jabber:x:data' type='text-single'/>"));
        assert_eq!(parse_step(&unnamed, "urn:x#n"), Err(ParseError::Invalid));
        let fixed = step_of("executing", &form("<field xmlns='jabber:x:data' type='fixed'><value>Hi</value></field>"));
        assert_eq!(fixed.form.unwrap().fields[0].kind, Kind::Fixed);
    }

    #[test]
    fn an_answer_for_another_command_or_with_a_bad_status_is_refused() {
        assert_eq!(parse_step(&answer("completed", ""), "other"), Err(ParseError::WrongCommand));
        assert_eq!(parse_step(&parse("<query xmlns='urn:x'/>"), "urn:x#n"), Err(ParseError::WrongCommand));
        assert_eq!(parse_step(&answer("running", ""), "urn:x#n"), Err(ParseError::Invalid));
        let no_status = parse(&format!("<command xmlns='{NS}' node='urn:x#n'/>"));
        assert_eq!(parse_step(&no_status, "urn:x#n"), Err(ParseError::Invalid));
        let no_session = parse(&format!("<command xmlns='{NS}' node='urn:x#n' status='executing'/>"));
        assert_eq!(parse_step(&no_session, "urn:x#n"), Err(ParseError::Invalid));
        let done = parse(&format!("<command xmlns='{NS}' node='urn:x#n' status='completed'/>"));
        assert_eq!(parse_step(&done, "urn:x#n").unwrap().session, None);
        let huge = parse(&format!(
            "<command xmlns='{NS}' node='urn:x#n' sessionid='{}' status='executing'/>",
            "s".repeat(1000)
        ));
        assert_eq!(parse_step(&huge, "urn:x#n"), Err(ParseError::Invalid));
    }

    #[test]
    fn a_hostile_form_is_bounded() {
        let mut fields = String::new();
        for n in 0..(MAX_FIELDS + 1) {
            fields.push_str(&format!("<field xmlns='jabber:x:data' var='f{n}'/>"));
        }
        assert_eq!(
            parse_step(&answer("executing", &form(&fields)), "urn:x#n"),
            Err(ParseError::Invalid)
        );
        let big = format!(
            "<field xmlns='jabber:x:data' var='v'><value>{}</value></field>",
            "z".repeat(10_000)
        );
        let s = step_of("executing", &form(&big));
        assert_eq!(s.form.unwrap().fields[0].values[0].len(), MAX_TEXT);
    }

    #[test]
    fn display_text_loses_control_characters_but_keeps_lines() {
        assert_eq!(clean("a\u{7}b\nc\u{1b}[0m"), "abc[0m");
        assert_eq!(clean_text("a\u{7}b\nc\td\u{1b}"), "ab\nc\td");
    }

    #[test]
    fn only_info_warn_and_error_notes_exist() {
        let s = step_of(
            "completed",
            &format!("<note xmlns='{NS}' type='error'>Bad</note><note xmlns='{NS}' type='x'>Odd</note>"),
        );
        assert_eq!(s.notes[0].kind, "error");
        assert_eq!(s.notes[1].kind, "info");
    }

    #[test]
    fn the_answer_sends_what_was_typed_and_keeps_hidden_state() {
        let s = step_of(
            "executing",
            &form(
                "<field xmlns='jabber:x:data' type='fixed'><value>Header</value></field>\
                 <field xmlns='jabber:x:data' var='FORM_TYPE' type='hidden'><value>urn:x</value></field>\
                 <field xmlns='jabber:x:data' var='name' type='text-single'><value>old</value></field>\
                 <field xmlns='jabber:x:data' var='keep' type='text-single'><value>default</value></field>\
                 <field xmlns='jabber:x:data' var='all' type='list-multi'/>",
            ),
        );
        let form = s.form.unwrap();
        let mut values = BTreeMap::new();
        values.insert("name".to_string(), vec!["new".to_string()]);
        values.insert("all".to_string(), vec!["a".to_string(), "b".to_string()]);
        // A hostile page cannot change hidden state by naming it.
        values.insert("FORM_TYPE".to_string(), vec!["urn:evil".to_string()]);
        let x = submit(&form, &values);
        assert_eq!(x.attr("type"), Some("submit"));
        let sent: Vec<(String, Vec<String>)> = x
            .children()
            .map(|f| (f.attr("var").unwrap().to_string(), f.children().map(|v| v.text()).collect()))
            .collect();
        assert_eq!(
            sent,
            vec![
                ("FORM_TYPE".to_string(), vec!["urn:x".to_string()]),
                ("name".to_string(), vec!["new".to_string()]),
                ("keep".to_string(), vec!["default".to_string()]),
                ("all".to_string(), vec!["a".to_string(), "b".to_string()]),
            ],
            "fixed fields are labels and are not sent"
        );
    }

    #[test]
    fn typed_values_are_bounded_when_sent() {
        let s = step_of("executing", &form("<field xmlns='jabber:x:data' var='a'/>"));
        let mut values = BTreeMap::new();
        values.insert("a".to_string(), vec!["x".repeat(MAX_TEXT * 2)]);
        let x = submit(&s.form.unwrap(), &values);
        assert_eq!(x.children().next().unwrap().children().next().unwrap().text().len(), MAX_TEXT);
    }

    #[test]
    fn required_fields_must_not_be_blank() {
        let s = step_of(
            "executing",
            &form(
                "<field xmlns='jabber:x:data' var='a' type='text-single' label='Name'><required/></field>\
                 <field xmlns='jabber:x:data' var='b' type='text-single'><required/></field>\
                 <field xmlns='jabber:x:data' var='c' type='boolean' label='Sure'><required/></field>\
                 <field xmlns='jabber:x:data' var='d' type='text-single' label='Optional'/>",
            ),
        );
        let form = s.form.unwrap();
        let mut values = BTreeMap::new();
        assert_eq!(missing_required(&form, &values), vec!["Name", "b"]);
        values.insert("a".to_string(), vec!["   ".to_string()]);
        values.insert("b".to_string(), vec!["x".to_string()]);
        assert_eq!(missing_required(&form, &values), vec!["Name"], "blank is missing; a switch never is");
        values.insert("a".to_string(), vec!["ok".to_string()]);
        assert!(missing_required(&form, &values).is_empty());
    }

    #[test]
    fn booleans_read_as_the_protocol_writes_them() {
        for yes in ["1", "true", " true "] {
            assert!(truthy(yes), "{yes:?}");
        }
        for no in ["0", "false", "", "yes", "TRUE"] {
            assert!(!truthy(no), "{no:?}");
        }
    }

    fn invite_step(uri: &str, landing: &str) -> Step {
        step_of(
            "completed",
            &format!(
                "<x xmlns='{DATA}' type='result'>\
                 <field var='uri'><value>{uri}</value></field>\
                 <field var='landing-url'><value>{landing}</value></field>\
                 <field var='expire'><value>2026-12-01T00:00:00Z</value></field></x>"
            ),
        )
    }

    #[test]
    fn a_completed_invite_yields_its_links() {
        let invite = invite(&invite_step(
            "xmpp:example.com?register;preauth=abc",
            "https://example.com/invite/abc",
        ))
        .unwrap();
        assert_eq!(invite.uri.as_deref(), Some("xmpp:example.com?register;preauth=abc"));
        assert_eq!(invite.landing.as_deref(), Some("https://example.com/invite/abc"));
        assert_eq!(invite.expires.as_deref(), Some("2026-12-01T00:00:00Z"));
    }

    #[test]
    fn an_invite_with_only_one_link_still_works() {
        let only_uri = invite(&invite_step("xmpp:example.com?register;preauth=abc", "")).unwrap();
        assert!(only_uri.landing.is_none());
        let only_page = invite(&invite_step("", "https://example.com/i/abc")).unwrap();
        assert!(only_page.uri.is_none());
    }

    #[test]
    fn unsafe_or_missing_links_are_not_an_invite() {
        for (uri, landing) in [
            ("", ""),
            ("javascript:alert(1)", "http://example.com/plain"),
            ("file:///etc/passwd", "ftp://example.com"),
            ("xmpp:", "https://"),
            ("xmpp:a b", "https://example.com/a b"),
            ("https://example.com", "xmpp:example.com"),
        ] {
            assert_eq!(invite(&invite_step(uri, landing)), None, "{uri} {landing}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URI));
        assert_eq!(invite(&invite_step("", &long)), None);
    }

    #[test]
    fn an_unfinished_or_formless_command_is_not_an_invite() {
        let mut step = invite_step("xmpp:example.com?register;preauth=abc", "");
        step.status = Status::Executing;
        assert_eq!(invite(&step), None);
        step.status = Status::Completed;
        step.form = None;
        assert_eq!(invite(&step), None);
    }

    #[test]
    fn the_invite_node_is_the_xep_0401_one() {
        assert_eq!(INVITE_NODE, "urn:xmpp:invite#invite");
    }

    #[test]
    fn every_failure_has_a_plain_explanation() {
        use crate::iq::Error;
        let rejected = |c: &str| Error::Rejected { condition: c.into(), pubsub: None };
        assert!(error_text(&rejected("item-not-found")).contains("does not offer"));
        assert!(error_text(&rejected("service-unavailable")).contains("does not offer"));
        assert!(error_text(&rejected("forbidden")).contains("does not let"));
        assert!(error_text(&rejected("not-allowed")).contains("does not let"));
        assert!(error_text(&rejected("bad-request")).contains("did not accept"));
        assert!(error_text(&rejected("internal-server-error")).contains("could not run"));
        for error in [Error::Timeout, Error::Closed, Error::Busy, Error::Invalid] {
            assert!(!error_text(&error).is_empty());
        }
        assert!(!error_text(&rejected("<script>")).contains('<'), "a condition is never echoed");
    }

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&chrono::Utc)
    }
    #[test]
    fn expiry_reads_in_the_largest_whole_unit() {
        let now = at("2026-11-21T12:00:00Z");
        let cases = [
            ("2026-11-21T12:00:30Z", "Expires in less than a minute"),
            ("2026-11-21T12:01:00Z", "Expires in 1 minute"),
            ("2026-11-21T12:12:59Z", "Expires in 12 minutes"),
            ("2026-11-21T13:00:00Z", "Expires in 1 hour"),
            ("2026-11-22T11:00:00Z", "Expires in 23 hours"),
            ("2026-11-22T12:00:00Z", "Expires in 24 hours"),
            ("2026-11-23T12:00:00Z", "Expires in 2 days"),
            ("2026-11-27T18:00:00Z", "Expires in 6 days"),
            ("2027-11-21T12:00:00Z", "Expires in 365 days"),
        ];
        for (when, text) in cases {
            assert_eq!(expiry_text(at(when), now), text, "{when}");
        }
    }
    #[test]
    fn an_expired_invite_says_so() {
        let now = at("2026-11-21T12:00:00Z");
        assert_eq!(expiry_text(now, now), "Expired just now");
        assert_eq!(expiry_text(at("2026-11-21T11:59:30Z"), now), "Expired just now");
        assert_eq!(expiry_text(at("2026-11-21T11:58:00Z"), now), "Expired 2 minutes ago");
        assert_eq!(expiry_text(at("2026-11-21T11:00:00Z"), now), "Expired 1 hour ago");
        assert_eq!(expiry_text(at("2026-11-18T12:00:00Z"), now), "Expired 3 days ago");
    }
    #[test]
    fn the_text_follows_the_clock() {
        let expires = at("2026-11-21T12:10:00Z");
        assert_eq!(expiry_text(expires, at("2026-11-21T12:00:00Z")), "Expires in 10 minutes");
        assert_eq!(expiry_text(expires, at("2026-11-21T12:09:00Z")), "Expires in 1 minute");
        assert_eq!(expiry_text(expires, at("2026-11-21T12:09:45Z")), "Expires in less than a minute");
        assert_eq!(expiry_text(expires, at("2026-11-21T12:10:00Z")), "Expired just now");
        let almost = at("2026-11-21T12:09:59.600Z");
        assert_eq!(expiry_text(expires, almost), "Expires in less than a minute", "0.4 s left is not expired");
        assert_eq!(expiry_text(expires, at("2026-11-21T12:20:00Z")), "Expired 10 minutes ago");
    }
    #[test]
    fn the_servers_expiry_is_read_with_any_offset() {
        let invite = |value: &str| Invite { uri: None, landing: None, expires: Some(value.into()) };
        assert_eq!(invite("2026-12-01T00:00:00Z").expires_at(), Some(at("2026-12-01T00:00:00Z")));
        assert_eq!(invite("2026-12-01T02:00:00+02:00").expires_at(), Some(at("2026-12-01T00:00:00Z")));
        assert_eq!(invite("2026-12-01T00:00:00.250Z").expires_at().is_some(), true);
        for bad in ["", "soon", "2026-12-01", "9999999999999999999"] {
            assert_eq!(invite(bad).expires_at(), None, "{bad:?}");
        }
        assert_eq!(Invite { uri: None, landing: None, expires: None }.expires_at(), None);
    }
}
