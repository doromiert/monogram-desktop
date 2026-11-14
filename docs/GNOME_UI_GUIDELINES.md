# GNOME UI guidelines (gtk4-rs / libadwaita)

A working checklist for any change to widget code (`src/*_ui.rs` and similar).
[DESIGN.md](DESIGN.md) describes the app's structure and why; this file is the
rulebook for building and reviewing individual screens. Both defer to the
[GNOME HIG](https://developer.gnome.org/hig/).

The project targets `gtk4` 0.11 and `libadwaita` 0.9 (`v1_9`), so prefer current
APIs and avoid deprecated ones.

## Process

1. Decide the pattern first: container, navigation, controls, feedback.
2. Look at what the codebase already does for the same job (`grep -r 'adw::' src`)
   and match it.
3. Use a native libadwaita widget before writing custom CSS or a custom widget.
4. Run the [checklist](#checklist) before calling the change done.

## Pick the native widget

| Need | Use | Avoid |
|------|-----|-------|
| Main window | `adw::ApplicationWindow` + `adw::ToolbarView` + `adw::HeaderBar` | custom title bars |
| Settings | `adw::PreferencesDialog` > `PreferencesPage` > `PreferencesGroup` | `adw::PreferencesWindow` (deprecated) |
| Secondary task or form | `adw::Dialog` with a `HeaderBar` in a `ToolbarView` | separate `gtk::Window`s |
| Confirmation or blocking error | `adw::AlertDialog` | `gtk::MessageDialog` |
| Sidebar plus content | `adw::OverlaySplitView` / `NavigationSplitView` with `adw::Breakpoint` | fixed-width columns |
| Drill-down | `adw::NavigationView` | hand-rolled stacks |
| On/off setting | `adw::SwitchRow` | checkboxes for settings |
| Choose one | `adw::ComboRow` | radio buttons outside dialogs |
| Text input | `adw::EntryRow`, `adw::PasswordEntryRow` | bare `gtk::Entry` in forms |
| Number | `adw::SpinRow` | text entry for numbers |
| Exclusive view modes | `adw::ToggleGroup` | several `ToggleButton`s |
| Loading | `adw::Spinner`; for long jobs a progress bar with text | frozen UI |
| Wrapping tags or chips | `adw::WrapBox` | manual line breaking |
| Keyboard shortcuts | `adw::ShortcutsDialog` | `gtk::ShortcutsWindow` (deprecated) |
| Large or dynamic lists | `gtk::ListView` / `GridView` (virtualised) | `ListBox` with thousands of rows |
| Short or navigation lists | `gtk::ListBox` (`boxed-list`, `navigation-sidebar`) | |
| Empty list | `adw::StatusPage` (icon, title, one action) | a blank area |

## Feedback

- Finished action or recoverable error: `adw::Toast`. Keep it short. For
  destructive actions offer Undo instead of asking for confirmation first.
- Persistent state (offline, degraded, sign-in needed): `adw::Banner`.
- A decision is needed, or the problem blocks the user: `adw::AlertDialog`.
- Escalate toast, then banner, then dialog. Do not show more than one for the
  same event.
- Dialog buttons: Cancel first, action last. Use specific verbs ("Delete",
  "Save"), never "OK", "Yes" or "Submit". Use `destructive-action` for
  destructive responses and `suggested-action` for the primary one, at most one
  of each per view.

## Details

- **Icons:** symbolic only, named `action-object-symbolic`, taken from the GNOME
  Icon Library. Icon-only buttons need a tooltip and an accessible label
  (`update_property(&[gtk::accessible::Property::Label(..)])`).
- **Typography:** use style classes (`title-1` to `title-4`, `heading`, `body`,
  `caption`, `monospace`). Use `.dimmed` for secondary text in new code; existing
  `.dim-label` uses do not need a drive-by migration.
- **Spacing and colour:** keep libadwaita's default spacing; do not add ad-hoc
  margins to fix a layout. Follow the user's accent colour, fonts and colour
  scheme through `adw::StyleManager`, and use the CSS variables
  (`@accent_bg_color`, `@window_bg_color`, ...) rather than hard-coded colours.
- **Text:** Header Case for labels and titles, sentence case for descriptions.
- **Imagery:** no text over images or textured backgrounds.
- **Threading:** never block the GTK main loop. Do I/O, crypto and network work
  off-thread and update widgets back on the main context.

## Checklist

1. A native widget is used wherever one exists; no custom styling where libadwaita
   already has a pattern.
2. It works at about 800x600 and adapts when narrower (breakpoints, see
   [DESIGN.md](DESIGN.md)).
3. It is fully keyboard operable (Tab, Enter, Space; Escape closes dialogs) and
   focus lands somewhere sensible when it opens.
4. Every control has an accessible name. Check with `GTK_THEME=Adwaita:hc` and at
   200% text scale.
5. Empty, loading, error and very long text states are handled.
6. Light and dark colour schemes both look right.
7. Mobile parity is checked, and [FEATURE_PARITY.md](FEATURE_PARITY.md) is updated
   if behaviour changed. A fixture or mock-up is not an implemented feature.
