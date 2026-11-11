use crate::ui::Ui;
use std::rc::Rc;

impl Ui {
    pub fn delete_custom_folder(self: &Rc<Self>, name: &str) -> bool {
        let mut state = self.state.borrow_mut();
        if name == "All" || !state.folders.contains_key(name) {
            return false;
        }
        let folders = state.folders.clone();
        let order = state.folder_order.clone();
        let selected = state.folder.clone();
        state.folders.remove(name);
        state.folder_order.retain(|folder| folder != name);
        if state.folder == name {
            state.folder = "All".into();
        }
        drop(state);
        if !self.persist() {
            let mut state = self.state.borrow_mut();
            state.folders = folders;
            state.folder_order = order;
            state.folder = selected;
            return false;
        }
        self.rebuild_folders();
        self.refresh_chats(None);
        true
    }
}
