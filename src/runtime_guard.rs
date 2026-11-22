//! Tests that keep Tokio-only calls out of the code that runs on the GTK main loop.
//!
//! The GTK main loop has no Tokio runtime. Anything that needs one panics there with
//! "there is no reactor running, must be called from the context of a Tokio 1.x runtime":
//! `iq::Client::request` (it uses a Tokio timer), `tokio::time::sleep` and `timeout`, `tokio::spawn`
//! and the like. On the main loop use `Client::request_on_main_loop`, `glib::timeout_future`
//! and `glib::spawn_future_local`.
//!
//! A panic like that only shows when the code runs against a real session, so it is easy to miss
//! in review and in tests that stub the network. These tests read the presentation files and fail
//! on the calls themselves.
use std::path::Path;

/// Calls that need a Tokio runtime. `request_on_main_loop` and `spawn_blocking` do not match.
const NEEDS_TOKIO: [&str; 8] = [
    ".request(",
    "tokio::time::",
    "tokio::spawn(",
    "tokio::task::spawn(",
    "Handle::current(",
    "tokio::net::",
    "tokio::fs::",
    "tokio::process::",
];

/// Source files whose code runs on the GTK main loop and nothing else: windows, dialogs and
/// widgets. Files that mix presentation with network work (`protocol.rs`, `keepassxc.rs`,
/// `link_preview_logic.rs`, ...) are not listed: they use Tokio on purpose, on Tokio tasks.
pub fn presentation_files(src: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(src)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name.ends_with("_ui.rs")
                || ["ui.rs", "dialogs.rs", "media.rs", "messages.rs", "widgets.rs", "components.rs"]
                    .contains(&name.as_str())
        })
        .collect();
    files.sort();
    files
}

/// `(line number, line)` of each call that needs Tokio, ignoring comments.
pub fn offences(source: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let code = line.split("//").next().unwrap_or("");
            NEEDS_TOKIO.iter().any(|call| code.contains(call)).then(|| (index + 1, line.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_flags_tokio_only_calls_and_nothing_else() {
        for bad in [
            "let reply = iq.request(&peer, Kind::Get, q).await;",
            "    .request(\n",
            "tokio::time::sleep(Duration::from_millis(5)).await;",
            "let r = tokio::time::timeout(d, fut).await;",
            "tokio::spawn(async move {});",
            "tokio::task::spawn(work);",
            "let h = tokio::runtime::Handle::current();",
            "let s = tokio::net::TcpStream::connect(a);",
            "tokio::fs::read(p).await",
            "tokio::process::Command::new(\"x\")",
            "x.request(\"a\"); // a comment after the call does not hide it",
        ] {
            assert_eq!(offences(bad).len(), 1, "{bad:?}");
        }
        for fine in [
            "iq.request_on_main_loop(&peer, Kind::Get, q).await",
            "tokio::task::spawn_blocking(move || work())",
            "glib::timeout_future(Duration::from_millis(5)).await;",
            "glib::spawn_future_local(async move {});",
            "// iq.request( is what panics here",
            "    // tokio::time::sleep is for Tokio tasks",
            "let requested = 1; let request = 2; fn request(a: u8) {}",
            "",
        ] {
            assert!(offences(fine).is_empty(), "{fine:?}");
        }
        let many = "ok();\nlet a = iq.request(x);\nok();\ntokio::spawn(b);\n";
        assert_eq!(offences(many).iter().map(|(line, _)| *line).collect::<Vec<_>>(), [2, 4]);
    }

    #[test]
    fn the_presentation_files_are_found_and_include_the_ones_that_talk_to_the_server() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let files = presentation_files(&src);
        assert!(files.len() >= 30, "a broken file filter would make the guard check nothing: {}", files.len());
        for must in ["last_activity_ui.rs", "ui.rs", "dialogs.rs", "room_ui.rs", "adhoc_ui.rs", "messages.rs", "components.rs"] {
            assert!(files.iter().any(|f| f == must), "{must} is covered");
        }
        for never in ["protocol.rs", "iq.rs", "keepassxc.rs", "link_preview_logic.rs", "runtime_guard.rs"] {
            assert!(!files.iter().any(|f| f == never), "{never} runs on Tokio and is not covered");
        }
    }

    /// The regression test for "there is no reactor running ... iq.rs": no presentation file may
    /// call `Client::request` or anything else that needs a Tokio runtime.
    #[test]
    fn nothing_that_runs_on_the_gtk_main_loop_needs_a_tokio_runtime() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        for name in presentation_files(&src) {
            let source = std::fs::read_to_string(src.join(&name)).unwrap();
            for (line, text) in offences(&source) {
                found.push(format!("src/{name}:{line}: {text}"));
            }
        }
        assert!(
            found.is_empty(),
            "these calls need a Tokio runtime and panic on the GTK main loop (\"there is no reactor \
             running\"); use `request_on_main_loop`, `glib::timeout_future` or `glib::spawn_future_local`:\n{}",
            found.join("\n")
        );
    }
}
