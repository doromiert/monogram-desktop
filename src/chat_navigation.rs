//! Keyboard navigation follows the same visible order as the folder and chat lists.
use crate::ui::Ui;
use adw::prelude::*;
use std::rc::Rc;

pub(crate) fn install(ui:&Rc<Ui>) {
    for (name, key, folders, forward) in [
        ("previous-folder","<Alt>Left",true,false),
        ("next-folder","<Alt>Right",true,true),
        ("previous-chat","<Alt>Up",false,false),
        ("next-chat","<Alt>Down",false,true),
    ] {
        ui.action(name,move |ui|navigate(ui,folders,forward));
        if let Some(app)=ui.window.application() {app.set_accels_for_action(&format!("win.{name}"),&[key]);}
    }
}
fn neighbour(items:&[String], current:&str, forward:bool)->Option<String> {
    let index=match items.iter().position(|item|item==current) {
        Some(index) if forward=>index.checked_add(1)?,
        Some(index)=>index.checked_sub(1)?,
        None if forward=>0,
        None=>items.len().checked_sub(1)?,
    };
    items.get(index).cloned()
}
fn navigate(ui:&Rc<Ui>, folders:bool, forward:bool) {
    if ui.window.visible_dialog().is_some() || ui.active_media.borrow().is_some() || ui.active_story.borrow().is_some() {return;}
    let target={
        let state=ui.state.borrow();
        if folders {neighbour(&state.visible_folder_names(),&state.folder,forward)}
        else {neighbour(&state.filtered(ui.search.text().as_str()).into_iter().map(|chat|chat.id).collect::<Vec<_>>(),&state.active,forward)}
    };
    if let Some(target)=target {
        if let Some(popup)=ui.active_profile.borrow().as_ref() {popup.popdown();}
        if folders {ui.set_folder(&target);} else {ui.select_chat(&target);}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::{gio,glib};
    use std::time::{Duration,Instant};
    #[test]
    fn edges_and_empty_lists_do_not_wrap() {
        let items=vec!["one".into(),"two".into()];
        assert_eq!(neighbour(&items,"one",false),None);assert_eq!(neighbour(&items,"two",true),None);
        assert_eq!(neighbour(&[],"one",true),None);assert_eq!(neighbour(&items,"outside",true),Some("one".into()));
        assert_eq!(neighbour(&items,"outside",false),Some("two".into()));
    }
    #[test]
    #[ignore="requires private GTK display, xdotool and D-Bus"]
    fn alt_arrows_follow_visible_folder_and_chat_order_from_the_composer() {
        let app=adw::Application::builder().application_id("com.negzero.NavigationFixture").flags(gio::ApplicationFlags::NON_UNIQUE).build();app.register(None::<&gio::Cancellable>).unwrap();
        let ui=Ui::new(&app,true);let mut state=ui.state.borrow().clone();state.chats.truncate(3);
        let (first,second)=(state.chats[1].id.clone(),state.chats[0].id.clone());state.active=first.clone();
        state.folder_order=vec!["All".into(),"Few".into(),"Empty".into()];state.all_folder_hidden=true;state.folder="Few".into();state.folders.clear();
        state.folders.insert("Few".into(),crate::model::Folder{include:[first.clone(),second.clone()].into(),pinned:vec![first.clone(),second.clone()],..Default::default()});
        state.folders.insert("Empty".into(),Default::default());ui.activate_state(state);ui.window.set_default_size(1100,700);ui.window.present();ui.composer.grab_focus();
        let pump=||{let until=Instant::now()+Duration::from_millis(180);while Instant::now()<until{glib::MainContext::default().iteration(false);std::thread::sleep(Duration::from_millis(2));}};pump();
        let ids=std::process::Command::new("xdotool").args(["search","--name","^Monogram$"]).output().unwrap();let id=String::from_utf8(ids.stdout).unwrap().lines().last().unwrap().to_owned();assert!(std::process::Command::new("xdotool").args(["windowfocus","--sync",&id]).status().unwrap().success());pump();
        let key=|key:&str|{
            assert!(std::process::Command::new("xdotool").args(["key","--clearmodifiers",key]).status().unwrap().success());pump();
            let mut child=ui.folder_bar.first_child();let mut active=Vec::new();
            while let Some(widget)=child {child=widget.next_sibling();if let Some(button)=widget.downcast_ref::<gtk::ToggleButton>().filter(|button|button.is_active()){active.push(button.widget_name().to_string());}}
            assert_eq!(active,vec![ui.state.borrow().folder.clone()],"visible selected tab must follow keyboard folder navigation");
        };
        ui.composer.buffer().set_text("kept draft");key("alt+Down");assert_eq!(ui.state.borrow().active,second);assert_eq!(ui.state.borrow().chats.iter().find(|chat|chat.id==first).unwrap().draft,"kept draft");
        key("alt+Down");assert_eq!(ui.state.borrow().active,second);key("alt+Up");assert_eq!(ui.state.borrow().active,first);
        key("alt+Left");assert_eq!(ui.state.borrow().folder,"Few","hidden All is skipped");key("alt+Right");assert_eq!(ui.state.borrow().folder,"Empty");
        key("alt+Down");assert_eq!(ui.state.borrow().active,first,"empty folder leaves the conversation alone");key("alt+Right");assert_eq!(ui.state.borrow().folder,"Empty");key("alt+Left");assert_eq!(ui.state.borrow().folder,"Few");
        ui.shortcuts();pump();key("alt+Right");assert_eq!(ui.state.borrow().folder,"Few","modal dialogs isolate navigation");ui.window.visible_dialog().unwrap().close();ui.window.close();
    }
}
