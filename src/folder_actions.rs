use crate::ui::Ui;
use std::rc::Rc;

impl Ui {
    pub fn reorder_visible_folder(self: &Rc<Self>, name: &str, index: usize) -> bool {
        let order = self.state.borrow().folder_order.clone();
        if !self.state.borrow_mut().reorder_visible(name, index) {
            return false;
        }
        if !self.persist() {
            self.state.borrow_mut().folder_order = order;
            return false;
        }
        self.rebuild_folders();
        true
    }
    pub fn set_all_folder_hidden(self: &Rc<Self>, hidden: bool) -> bool {
        let mut state = self.state.borrow_mut();
        if state.all_folder_hidden == hidden {
            return true;
        }
        if hidden && state.visible_folder_names().len() <= 1 {
            drop(state);
            self.notice("Keep at least one folder visible");
            return false;
        }
        let previous = state.all_folder_hidden;
        let selected = state.folder.clone();
        state.all_folder_hidden = hidden;
        if !state.folder_visible(&state.folder) {
            state.folder = state.visible_folder_names()[0].clone();
        }
        drop(state);
        if !self.persist() {
            let mut state = self.state.borrow_mut();
            state.all_folder_hidden = previous;
            state.folder = selected;
            return false;
        }
        self.rebuild_folders();
        self.refresh_chats(None);
        true
    }
    pub fn delete_custom_folder(self: &Rc<Self>, name: &str) -> bool {
        let mut state = self.state.borrow_mut();
        if name == "All" || !state.folders.contains_key(name) {
            return false;
        }
        let folders = state.folders.clone();
        let order = state.folder_order.clone();
        let selected = state.folder.clone();
        let hidden = state.all_folder_hidden;
        state.folders.remove(name);
        state.folder_order.retain(|folder| folder != name);
        if state.visible_folder_names().is_empty() {
            state.all_folder_hidden = false;
        }
        if !state.folder_visible(&state.folder) {
            state.folder = state.visible_folder_names()[0].clone();
        }
        drop(state);
        if !self.persist() {
            let mut state = self.state.borrow_mut();
            state.folders = folders;
            state.folder_order = order;
            state.folder = selected;
            state.all_folder_hidden = hidden;
            return false;
        }
        self.rebuild_folders();
        self.refresh_chats(None);
        true
    }
}
