//! File-manager and chat attachment activation share the account-owned pack viewer.
use crate::{packs::{Job, Kind}, ui::Ui};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub(crate) fn is_pack(name: &str, mime: &str) -> bool {
    matches!(mime, "application/vnd.monogram.sticker-pack" | "application/x-monogram-pack")
        || name.rsplit_once('.').is_some_and(|(_,ext)|
            ["pack", "nzsp", "xmsp"].iter().any(|known|ext.eq_ignore_ascii_case(known)))
}
impl Ui {
    pub(crate) fn open_pack_file(self: &Rc<Self>, file: gio::File) {
        if self.pending_pack_files.borrow().len() >= 16 {
            self.notice("Finish opening these packs before choosing more.");
            return;
        }
        self.pending_pack_files.borrow_mut().push(file);
        if self.state.borrow().account.is_empty() {
            self.notice("Sign in to open this pack. It will open after sign-in.");
        } else if self.pack_library.borrow().is_none() {
            self.notice("Loading your pack library…");
        }
        self.open_pending_pack_files();
    }
    pub(crate) fn open_pending_pack_files(self: &Rc<Self>) {
        let Some(library)=self.pack_library.borrow().clone() else {return;};
        let files=std::mem::take(&mut *self.pending_pack_files.borrow_mut());
        for file in files {
            let Some(path)=file.path() else {
                self.notice("Save this pack to your computer, then open it again.");continue;
            };
            let job=Job::new();
            let dialog=adw::Dialog::builder().title("Opening Pack").content_width(360).build();
            let body=gtk::Box::new(gtk::Orientation::Vertical,18);
            crate::widgets::margins(&body,24);
            let spinner=gtk::Spinner::builder().spinning(true).halign(gtk::Align::Center).build();
            body.append(&spinner);
            body.append(&gtk::Label::new(Some("Checking the pack and its artwork…")));
            let cancel=gtk::Button::with_label("Cancel");
            let weak=dialog.downgrade();cancel.connect_clicked(move |_|{if let Some(dialog)=weak.upgrade(){dialog.close();}});
            body.append(&cancel);dialog.set_child(Some(&body));
            let cancelled=job.clone();dialog.connect_closed(move |_|cancelled.cancel());
            self.track_dialog(&dialog);dialog.present(Some(&self.window));
            let ui=Rc::downgrade(self);let generation=self.generation.get();let library=library.clone();
            glib::spawn_future_local(async move {
                let result=library.preview_archive(path,job).await;
                let was_open=dialog.is_visible();dialog.close();
                let Some(ui)=ui.upgrade().filter(|ui|ui.generation.get()==generation && ui.state.borrow().account==library.account()) else {return;};
                if !was_open {return;}
                match result {
                    Ok(pack)=>{
                        let index=if pack.kind==Kind::Stickers{0}else{1};
                        let page=ui.pack_pages.borrow().get(index).cloned();
                        if let Some(page)=page {page.refresh();crate::pack_editor::open(&ui,&page,library,pack.id);}
                    }
                    Err(error)=>ui.notice(&format!("Could not open this pack: {error}")),
                }
            });
        }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn wait(mut ready:impl FnMut()->bool) {
        let started=std::time::Instant::now();let context=glib::MainContext::default();
        while started.elapsed()<std::time::Duration::from_secs(8) {
            for _ in 0..100 {if !context.pending(){break;}context.iteration(false);}
            if ready(){return;}std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(ready(),"pack activation did not settle");
    }
    fn find_button(widget:&gtk::Widget,label:&str)->Option<gtk::Button> {
        if let Some(button)=widget.downcast_ref::<gtk::Button>().filter(|b|b.label().as_deref()==Some(label)){return Some(button.clone());}
        let mut child=widget.first_child();while let Some(current)=child {child=current.next_sibling();if let Some(button)=find_button(&current,label){return Some(button);}}
        None
    }
    #[gtk::test]
    #[ignore = "requires a disposable GTK display and keyring"]
    fn file_activation_previews_then_installs_the_pack_without_blocking_gtk(){
        assert_eq!(std::env::var("MONOGRAM_KEYRING_FIXTURE").as_deref(),Ok("1"));
        crate::widgets::install_icons();
        let app=adw::Application::builder().application_id("com.negzero.MonogramPackFilesTest").flags(gio::ApplicationFlags::NON_UNIQUE).build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ui=Ui::new(&app,true);ui.window.present();
        ui.state.borrow_mut().account="pack-open@fixture.invalid".into();ui.rebuild_pack_library();
        let source=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/device-sync/mobile.xmsp");
        let path=std::env::temp_dir().join(format!("{}.pack",uuid::Uuid::new_v4()));std::fs::copy(source,&path).unwrap();
        let now=std::time::Instant::now();ui.open_pack_file(gio::File::for_path(&path));
        assert!(now.elapsed().as_millis()<400);assert!(!ui.pending_pack_files.borrow().is_empty() || ui.window.visible_dialog().is_some());
        wait(||ui.pack_library.borrow().is_some());
        let library=ui.pack_library.borrow().clone().unwrap();
        wait(||ui.window.visible_dialog().is_some_and(|d|find_button(d.upcast_ref(),"Add Pack").is_some()));
        let snapshot=glib::MainContext::default().block_on(library.snapshot()).unwrap();
        let pack=snapshot.packs.last().unwrap();assert!(!pack.installed);let id=pack.id;
        let button=find_button(ui.window.visible_dialog().unwrap().upcast_ref(),"Add Pack").unwrap();button.emit_clicked();
        wait(||glib::MainContext::default().block_on(library.snapshot()).unwrap().packs.iter().any(|p|p.id==id&&p.installed));
        ui.window.visible_dialog().unwrap().close();
        let before=glib::MainContext::default().block_on(library.snapshot()).unwrap().packs.len();
        std::fs::write(&path,b"not a pack").unwrap();ui.open_pack_file(gio::File::for_path(&path));
        wait(||ui.window.visible_dialog().is_none());
        assert_eq!(glib::MainContext::default().block_on(library.snapshot()).unwrap().packs.len(),before);
        ui.window.close();let _=std::fs::remove_file(path);
    }
    #[test] fn recognizes_pack_aliases_without_claiming_other_documents(){
        for name in ["friends.pack","cats.NZSP","custom.XmSp"]{assert!(is_pack(name,"application/octet-stream"));}
        assert!(is_pack("opaque.bin","application/vnd.monogram.sticker-pack"));
        for name in ["pack","x.pack.exe","photo.png",""]{assert!(!is_pack(name,"application/zip"));}
    }
}
