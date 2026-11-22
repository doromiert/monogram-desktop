//! Account-scoped live stories; network and persistence stay outside GTK callbacks.
use crate::{model::Story, protocol::Command, ui::Ui};
use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, atomic::Ordering};
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    draft: crate::model::UploadDraft,
    caption: String,
    entry: Option<crate::stories::Entry>,
}
fn preview(
    path: &std::path::Path,
    cancel: &gio::Cancellable,
) -> Result<gtk::gdk::Texture, &'static str> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "webm"))
    {
        let frame = crate::image_loader::encode_video_preview(path, cancel, 5000)?;
        return gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(frame.bytes))
            .map_err(|_| "Video preview unavailable");
    }
    let file = std::fs::File::open(path).map_err(|_| "Story file unavailable")?;
    if file.metadata().map_err(|_| "Story file unavailable")?.len() > crate::file_share::MAX {
        return Err("Story file is too large");
    }
    let mut decoder =
        crate::image_loader::Decoder::open(crate::downloads::VerifiedFile(file), cancel)?;
    Ok(decoder.frame(640)?.texture)
}

#[cfg(test)]
mod native_tests {
    use super::*;
    fn wait(mut ready: impl FnMut() -> bool) {
        let context = glib::MainContext::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < end {
            for _ in 0..50 {
                if !context.pending() {
                    break;
                }
                context.iteration(false);
            }
            if ready() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(ready(), "story UI did not settle");
    }
    fn button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = root.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = root.first_child();
        while let Some(w) = child {
            child = w.next_sibling();
            if let Some(found) = button(&w, label) {
                return Some(found);
            }
        }
        None
    }
    #[test]
    #[ignore = "requires private GTK display and D-Bus"]
    fn prepared_story_posts_without_external_io_and_close_cancels_pending_publication() {
        let app = adw::Application::builder()
            .application_id("com.negzero.MonogramStoriesFixture")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui = Ui::new(&app, true);
        let owner = "alice@example.invalid";
        ui.store
            .save_account(&crate::model::Account {
                jid: owner.into(),
                host: String::new(),
                port: 5222,
                direct_tls: false,
            })
            .unwrap();
        ui.activate_state(crate::model::State::live(owner, vec![]));
        ui.online.set(true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        ui.session
            .replace(Some(crate::protocol::Session::fixture_channel(tx)));
        ui.window.present();
        let make = |id: &str| {
            let entry = crate::stories::Entry {
                id: id.into(),
                owner: owner.into(),
                url: "https://example.invalid/story.png".into(),
                mime: "image/png".into(),
                caption: "Native story".into(),
                published: chrono::Utc::now().timestamp_millis(),
            };
            Pending {
                id: id.into(),
                draft: crate::model::UploadDraft {
                    uri: "file:///missing-story-fixture.png".into(),
                    name: "story.png".into(),
                    error: None,
                    sticker: false,
                    voice: false,
                    local_sticker: None,
                },
                caption: entry.caption.clone(),
                entry: Some(entry),
            }
        };
        ui.story_editor(make("published"));
        let dialog = ui.dialogs.borrow().last().unwrap().clone();
        wait(|| button(&dialog.clone().upcast(), "Retry Posting").is_some());
        let post = button(&dialog.clone().upcast(), "Retry Posting").unwrap();
        let start = std::time::Instant::now();
        post.emit_clicked();
        assert!(!post.is_sensitive());
        assert!(start.elapsed() < std::time::Duration::from_millis(400));
        let mut published = None;
        wait(|| {
            while let Ok(command) = rx.try_recv() {
                if let Command::StoryPublish { entry, reply } = command {
                    published = Some(entry.clone());
                    reply.send(Ok(entry)).unwrap();
                    return true;
                }
            }
            false
        });
        wait(|| {
            ui.store
                .stories_cache(owner, chrono::Utc::now().timestamp_millis())
                .unwrap()
                .len()
                == 1
        });
        assert_eq!(published.unwrap().caption, "Native story");
        wait(|| ui.dialogs.borrow().is_empty());
        ui.story_editor(make("cancelled"));
        let dialog = ui.dialogs.borrow().last().unwrap().clone();
        let post = button(&dialog.clone().upcast(), "Retry Posting").unwrap();
        post.emit_clicked();
        let mut pending = None;
        wait(|| {
            while let Ok(command) = rx.try_recv() {
                if let Command::StoryPublish { reply, .. } = command {
                    pending = Some(reply);
                    return true;
                }
            }
            false
        });
        dialog.close();
        wait(|| pending.as_ref().unwrap().is_closed());
        assert_eq!(
            ui.store
                .stories_cache(owner, chrono::Utc::now().timestamp_millis())
                .unwrap()
                .len(),
            1
        );
        assert!(
            !ui.store
                .pref(&format!("story-pending:{owner}"), "")
                .is_empty(),
            "cancelled draft stays recoverable"
        );
        ui.window.close();
    }
}
impl Ui {
    pub(crate) fn edit_story_caption(self: &Rc<Self>, story: Story) {
        if !story.own || story.id.is_empty() {
            return;
        }
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        let caption = adw::EntryRow::builder()
            .title("Caption")
            .text(&story.caption)
            .build();
        group.add(&caption);
        page.add(&group);
        let (dialog, header) = self.dialog("Edit Story", &page, 480, 240);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        let weak = Rc::downgrade(self);
        save.connect_clicked(move|button|{let Some(ui)=weak.upgrade().filter(|u|u.generation.get()==generation&&u.state.borrow().account==owner)else{return;};if caption.text().encode_utf16().count()>32768{ui.notice("Use a caption of at most 32768 characters.");return;}
            let entry=crate::stories::Entry{id:story.id.clone(),owner:story.jid.clone(),url:story.url.clone(),mime:story.mime.clone(),published:story.published,caption:caption.text().into()};let (reply,result)=tokio::sync::oneshot::channel();if !ui.command(Command::StoryPublish{entry,reply}){ui.notice("Reconnect before editing this story.");return;}button.set_sensitive(false);button.set_label("Saving…");let save=button.clone();let close=dialog.clone();let weak=Rc::downgrade(&ui);let owner=owner.clone();
            glib::spawn_future_local(async move{let answer=result.await;let Some(ui)=weak.upgrade().filter(|u|u.generation.get()==generation&&u.state.borrow().account==owner)else{return;};match answer{Ok(Ok(entry))=>{let account=owner.clone();let saved=ui.store.write_snapshot(move|store|{let now=chrono::Utc::now().timestamp_millis();let mut entries=store.stories_cache(&account,now)?.into_iter().filter(|e|e.owner==account&&e.id!=entry.id).collect::<Vec<_>>();entries.push(entry);entries.sort_by_key(|e|e.published);if entries.len()>100{entries.drain(..entries.len()-100);}store.stories_replace(&account,&account,&entries,now)}).await;if saved.is_ok(){close.close();ui.load_cached_stories().await;}else{ui.notice("Caption saved on the server; refresh stories to update this device.");}},Ok(Err(error))=>ui.notice(&error.to_string()),Err(_)=>ui.notice("Editing was interrupted. Reconnect and retry.")};save.set_label("Save");save.set_sensitive(true);});
        });
    }
    pub(crate) fn delete_story(self: &Rc<Self>, story: Story) {
        if !story.own || story.id.is_empty() {
            return;
        }
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let confirm = adw::AlertDialog::builder()
            .heading("Delete Story?")
            .body("Remove this story from your server and this device's story history.")
            .build();
        confirm.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        let weak = Rc::downgrade(self);
        confirm.connect_response(Some("delete"),move|_,_|{let Some(ui)=weak.upgrade().filter(|u|u.generation.get()==generation&&u.state.borrow().account==owner)else{return;};let (reply,result)=tokio::sync::oneshot::channel();if !ui.command(Command::StoryRetract{id:story.id.clone(),reply}){ui.notice("Reconnect before deleting this story.");return;}ui.notice("Deleting story…");let owner=owner.clone();let id=story.id.clone();let weak=Rc::downgrade(&ui);glib::spawn_future_local(async move{let answer=result.await;let Some(ui)=weak.upgrade().filter(|u|u.generation.get()==generation&&u.state.borrow().account==owner)else{return;};match answer{Ok(Ok(()))=>{let account=owner.clone();if ui.store.write_snapshot(move|store|store.stories_remove(&account,&account,&id)).await.is_ok(){ui.load_cached_stories().await;ui.notice("Story deleted");}else{ui.notice("Story deleted on the server; refresh stories to update local history.");}},Ok(Err(error))=>ui.notice(&error.to_string()),Err(_)=>ui.notice("Deletion was interrupted. Reconnect and retry.")}});});
        self.track_dialog(&confirm);
        confirm.present(Some(&self.window));
    }
    pub(crate) fn show_story_history(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            self.notice("Sign in to view your story history.");
            return;
        }
        let page = adw::PreferencesPage::new();
        let group=adw::PreferencesGroup::builder().title("Your Story History").description("Own stories retained on this device, including expired stories. Remote media still depends on its upload host.").build();
        page.add(&group);
        let loading = adw::ActionRow::builder()
            .title("Loading story history…")
            .build();
        let spinner = gtk::Spinner::builder().spinning(true).build();
        loading.add_suffix(&spinner);
        group.add(&loading);
        let (dialog, _) = self.dialog("Story History", &page, 520, 560);
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let account = owner.clone();
            let entries = ui
                .store
                .read_snapshot(move |store| {
                    store
                        .stories_history(&account)?
                        .into_iter()
                        .map(|entry| {
                            let asset = crate::uploads::snapshot_path(&account, &entry.id)
                                .ok()
                                .filter(|p| p.is_file())
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            Ok((entry, asset))
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .await;
            if ui.generation.get() != generation || ui.state.borrow().account != owner {
                return;
            }
            group.remove(&loading);
            match entries {
                Ok(entries) if entries.is_empty() => group.add(
                    &adw::ActionRow::builder()
                        .title("No story history on this device")
                        .build(),
                ),
                Ok(entries) => {
                    for (index, (entry, asset)) in entries.into_iter().enumerate() {
                        let stamp = chrono::DateTime::from_timestamp_millis(entry.published)
                            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        let row = adw::ActionRow::builder()
                            .title(if entry.caption.is_empty() {
                                "Story"
                            } else {
                                &entry.caption
                            })
                            .subtitle(&stamp)
                            .activatable(true)
                            .build();
                        let open = crate::widgets::icon_button(
                            "media-playback-start-symbolic",
                            "View story",
                        );
                        row.add_suffix(&open);
                        row.set_activatable_widget(Some(&open));
                        let story = Story {
                            owner: "You".into(),
                            id: entry.id,
                            jid: entry.owner,
                            url: entry.url,
                            mime: entry.mime,
                            published: entry.published,
                            caption: entry.caption,
                            asset,
                            own: true,
                            seen: true,
                            ..Default::default()
                        };
                        let weak = Rc::downgrade(&ui);
                        let close = dialog.clone();
                        open.connect_clicked(move |_| {
                            if let Some(ui) = weak.upgrade() {
                                close.close();
                                ui.open_story_item(story.clone());
                            }
                        });
                        group.add(&row);
                        if index % 8 == 7 {
                            glib::timeout_future(std::time::Duration::from_millis(1)).await;
                            if !dialog.is_mapped() {
                                return;
                            }
                        }
                    }
                }
                Err(_) => group.add(
                    &adw::ActionRow::builder()
                        .title("Story history could not be loaded. Close and reopen to retry.")
                        .build(),
                ),
            }
        });
    }
    pub(crate) fn pending_story(self: &Rc<Self>) -> bool {
        let owner = self.state.borrow().account.clone();
        let value = self.store.pref(&format!("story-pending:{owner}"), "");
        if let Ok(pending) = serde_json::from_str::<Pending>(&value) {
            self.story_editor(pending);
            true
        } else {
            false
        }
    }
    pub(crate) fn new_live_story(self: &Rc<Self>, file: gio::File) {
        let name = file
            .basename()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "story".into());
        self.story_editor(Pending {
            id: crate::model::new_id(),
            draft: crate::model::UploadDraft {
                uri: file.uri().into(),
                name,
                error: None,
                sticker: false,
                voice: false,
                local_sticker: None,
            },
            caption: String::new(),
            entry: None,
        });
    }
    fn story_editor(self: &Rc<Self>, pending: Pending) {
        let owner = self.state.borrow().account.clone();
        let generation = self.generation.get();
        let page = adw::PreferencesPage::new();
        let group=adw::PreferencesGroup::builder().title("Story").description("Visible to subscribed contacts for 24 hours. Story media uses HTTPS and is not end-to-end encrypted.").build();
        let media = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .height_request(200)
            .build();
        group.add(&media);
        let caption = adw::EntryRow::builder()
            .title("Caption")
            .text(&pending.caption)
            .build();
        group.add(&caption);
        let status = adw::ActionRow::builder()
            .title("Ready to post")
            .subtitle(&pending.draft.name)
            .build();
        let progress = gtk::ProgressBar::new();
        progress.set_width_request(100);
        progress.set_visible(false);
        status.add_suffix(&progress);
        group.add(&status);
        page.add(&group);
        let (dialog, header) = self.dialog("New Story", &page, 480, 560);
        let post = gtk::Button::with_label(if pending.entry.is_some() {
            "Retry Posting"
        } else {
            "Post Story"
        });
        post.add_css_class("suggested-action");
        header.pack_end(&post);
        let discard = gtk::Button::with_label("Discard");
        header.pack_start(&discard);
        let job = Arc::new(crate::uploads::Job::default());
        let cancel = job.clone();
        dialog.connect_closed(move |_| cancel.stop());
        let publish_cancel: Rc<RefCell<Option<tokio::sync::oneshot::Sender<()>>>> =
            Rc::new(RefCell::new(None));
        let stop = publish_cancel.clone();
        dialog.connect_closed(move |_| {
            if let Some(stop) = stop.borrow_mut().take() {
                let _ = stop.send(());
            }
        });
        let weak = Rc::downgrade(self);
        let current = Rc::new(RefCell::new(pending));
        let saved = current.clone();
        let close = dialog.clone();
        discard.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let key = format!("story-pending:{}", ui.state.borrow().account);
                let _ = ui.store.set_pref(&key, "");
                close.close();
            }
            let _ = &saved;
        });
        let source = gio::File::for_uri(&current.borrow().draft.uri);
        if let Some(path) = source.path() {
            let weak_picture = media.downgrade();
            glib::spawn_future_local(async move {
                let cancel = gio::Cancellable::new();
                let result = gio::spawn_blocking(move || preview(&path, &cancel)).await;
                if let (Some(picture), Ok(Ok(preview))) = (weak_picture.upgrade(), result) {
                    picture.set_paintable(Some(&preview));
                }
            });
        }
        let weak = Rc::downgrade(self);
        let close = dialog.clone();
        let cancel = job.clone();
        post.connect_clicked(move |button|{
            let Some(ui)=weak.upgrade().filter(|ui|ui.generation.get()==generation&&ui.state.borrow().account==owner)else{return;};
            if !ui.online.get(){ui.notice("Reconnect before posting. Your story draft is kept.");return;}
            if caption.text().encode_utf16().count()>32768{ui.notice("Use a caption of at most 32768 characters.");return;}
            current.borrow_mut().caption=caption.text().into();button.set_sensitive(false);discard.set_sensitive(false);caption.set_sensitive(false);status.set_title("Uploading story…");progress.set_visible(true);progress.pulse();
            let pending=current.borrow().clone();let state=current.clone();let owner=owner.clone();let key=format!("story-pending:{owner}");let job=cancel.clone();let weak=Rc::downgrade(&ui);let status=status.clone();let progress=progress.clone();let post=button.clone();let caption=caption.clone();let discard=discard.clone();let close=close.clone();let publish_cancel=publish_cancel.clone();
            let timer_job=job.clone();let timer_progress=progress.downgrade();let timer_status=status.downgrade();let timer=glib::timeout_add_local(std::time::Duration::from_millis(80),move||{
                let (Some(progress),Some(status))=(timer_progress.upgrade(),timer_status.upgrade())else{return glib::ControlFlow::Break;};
                match &*timer_job.phase.lock().unwrap(){crate::uploads::Phase::Uploading{bytes,total}=>{progress.set_fraction(*bytes as f64/(*total).max(1) as f64);status.set_subtitle(&format!("{} of {}",glib::format_size(*bytes),glib::format_size(*total)));},crate::uploads::Phase::Preparing=>{status.set_title("Preparing story…");progress.pulse();},crate::uploads::Phase::Reconnecting=>{status.set_title("Reconnecting to upload server…");progress.pulse();},_=>progress.pulse()};glib::ControlFlow::Continue
            });
            glib::spawn_future_local(async move{
                let result=async{
                    let ui=weak.upgrade().ok_or("The story window was closed")?;let record=pending.clone();let record_key=key.clone();ui.store.write_snapshot(move|store|store.set_pref(&record_key,&serde_json::to_string(&record).unwrap())).await.map_err(|_|"Could not save the story draft")?;
                    let mut pending=pending;
                    if pending.entry.is_none(){
                        let tx=ui.session.borrow().as_ref().map(|s|s.tx.clone()).ok_or("Reconnect before posting")?;let iq=crate::iq::Client::new(tx);let account=owner.clone();let id=pending.id.clone();let draft=pending.draft.clone();let task=job.clone();
                        let uploaded=gio::spawn_blocking(move||tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_|"Could not start story upload")?.block_on(crate::uploads::run(&iq,&account,&id,&draft,crate::encryption_policy::Mode::Plain,task))).await.map_err(|_|"Story upload was interrupted")??;
                        let entry=crate::stories::Entry{id:pending.id.clone(),owner:owner.clone(),url:uploaded.attachment.wire_url,mime:uploaded.attachment.mime,caption:pending.caption.clone(),published:chrono::Utc::now().timestamp_millis()};entry.validate(entry.published).map_err(|_|"Choose a JPEG, PNG, WebP, GIF, MP4 or WebM file")?;pending.entry=Some(entry);
                    }
                    if job.cancel.load(Ordering::Acquire)||ui.generation.get()!=generation||ui.state.borrow().account!=owner{return Err("Story posting cancelled");}
                    let mut entry=pending.entry.clone().unwrap();entry.caption=pending.caption.clone();pending.entry=Some(entry.clone());*state.borrow_mut()=pending.clone();let saved=pending;let record_key=key.clone();ui.store.write_snapshot(move|store|store.set_pref(&record_key,&serde_json::to_string(&saved).unwrap())).await.map_err(|_|"Could not save the uploaded story draft")?;
                    status.set_title("Posting story…");let (reply,received)=tokio::sync::oneshot::channel();if !ui.command(Command::StoryPublish{entry,reply}){return Err("Reconnect before posting");}
                    let (stop,cancelled)=tokio::sync::oneshot::channel();publish_cancel.replace(Some(stop));
                    let published=match futures_util::future::select(Box::pin(received),Box::pin(cancelled)).await{futures_util::future::Either::Left((result,_))=>result.map_err(|_|"Story posting was interrupted")?.map_err(|_|"The story could not be posted. Check server PEP support and retry.")?,futures_util::future::Either::Right(_)=>return Err("Story posting cancelled")};publish_cancel.borrow_mut().take();
                    if ui.generation.get()!=generation||ui.state.borrow().account!=owner{return Err("The account changed");}
                    let account=owner.clone();ui.store.write_snapshot(move|store|{let now=chrono::Utc::now().timestamp_millis();let mut entries=store.stories_cache(&account,now)?.into_iter().filter(|e|e.owner==account&&e.id!=published.id).collect::<Vec<_>>();entries.push(published);entries.sort_by_key(|e|e.published);if entries.len()>100{entries.drain(..entries.len()-100);}store.stories_replace(&account,&account,&entries,now)?;store.set_pref(&key,"")}).await.map_err(|_|"Story posted, but the local cache could not be updated")?;
                    Ok::<_,&str>(())
                }.await;
                timer.remove();
                if job.cancel.load(Ordering::Acquire){return;}
                if let Some(ui)=weak.upgrade().filter(|ui|ui.generation.get()==generation&&ui.state.borrow().account==owner){match result{Ok(())=>{close.close();ui.load_cached_stories().await;ui.notice("Story posted");},Err(error)=>{status.set_title(error);post.set_label("Retry Posting");post.set_sensitive(true);discard.set_sensitive(true);caption.set_sensitive(true);progress.set_visible(false);}}}
            });
        });
    }
    pub(crate) fn refresh_live_stories(self: &Rc<Self>, peer: Option<String>) {
        let state = self.state.borrow();
        let owner = state.account.clone();
        if owner.is_empty() {
            return;
        }
        let peers = peer.map(|p| vec![p]).unwrap_or_else(|| {
            std::iter::once(owner.clone())
                .chain(
                    state
                        .chats
                        .iter()
                        .filter(|c| c.contact && !c.group)
                        .take(50)
                        .map(|c| c.jid.clone()),
                )
                .collect()
        });
        drop(state);
        let tx = self.session.borrow().as_ref().map(|s| s.tx.clone());
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::spawn_future_local(async move {
            if let Some(ui) = weak.upgrade() {
                ui.load_cached_stories().await;
            }
            let Some(tx) = tx else {
                return;
            };
            for peer in peers {
                let Some(ui) = weak.upgrade().filter(|ui| {
                    ui.generation.get() == generation && ui.state.borrow().account == owner
                }) else {
                    return;
                };
                let (reply, result) = tokio::sync::oneshot::channel();
                if tx
                    .send(Command::StoriesFetch {
                        peer: peer.clone(),
                        reply,
                    })
                    .is_err()
                {
                    return;
                }
                let Ok(Ok(entries)) = result.await else {
                    continue;
                };
                if ui.generation.get() != generation || ui.state.borrow().account != owner {
                    return;
                }
                let account = owner.clone();
                let now = chrono::Utc::now().timestamp_millis();
                if ui
                    .store
                    .write_snapshot(move |store| {
                        store.stories_replace(&account, &peer, &entries, now)
                    })
                    .await
                    .is_ok()
                {
                    ui.load_cached_stories().await;
                }
            }
        });
    }
    pub(crate) async fn load_cached_stories(self: &Rc<Self>) {
        let owner = self.state.borrow().account.clone();
        if owner.is_empty() {
            return;
        }
        let account = owner.clone();
        let generation = self.generation.get();
        let data = self
            .store
            .read_snapshot(move |store| {
                let entries =
                    store.stories_cache(&account, chrono::Utc::now().timestamp_millis())?;
                entries
                    .into_iter()
                    .map(|e| {
                        let asset = if e.owner == account {
                            crate::uploads::snapshot_path(&account, &e.id)
                                .ok()
                                .filter(|p| p.is_file())
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        store
                            .stories_seen(&account, &e.owner, &e.id)
                            .map(|seen| (e, seen, asset))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await;
        if self.generation.get() != generation || self.state.borrow().account != owner {
            return;
        }
        if let Ok(data) = data {
            let stories = data
                .into_iter()
                .map(|(entry, seen, asset)| {
                    let own = entry.owner == owner;
                    let name = if own {
                        "You".into()
                    } else {
                        self.state
                            .borrow()
                            .chats
                            .iter()
                            .find(|c| c.jid == entry.owner)
                            .map(|c| c.display_name().to_owned())
                            .unwrap_or_else(|| entry.owner.clone())
                    };
                    Story {
                        owner: name,
                        id: entry.id,
                        jid: entry.owner,
                        url: entry.url,
                        mime: entry.mime,
                        published: entry.published,
                        caption: entry.caption,
                        seen,
                        own,
                        asset,
                        ..Default::default()
                    }
                })
                .collect();
            self.state.borrow_mut().stories = stories;
            self.refresh_stories();
        }
    }
}
