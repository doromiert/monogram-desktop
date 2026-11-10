# Desktop prototype review pass

User's screenshots and 21-item review, plus removal of footer Preferences, are
the acceptance reference for this pass.

Implemented: compact native headers; left-aligned conversation identity; sidebar
button only at collapsed width; readable clickable reply targets; compact composer
with external paper-plane button; full-height right ToolbarView/sidebar; native
Adw.ToggleGroup; centered request actions and small request badge; search-left /
menu-right sidebar header without edit button; animated in-window media overlay
with edge navigation and thumbnail fly-in; animated bundled GIF previews; Monero
tab and direct wallet access; no “Express yourself” heading; higher-contrast
interactive reactions, image caption spacing and encryption by sender name;
separate breakpoints for the two sidebars; inline bubble editing; folder editing
and chat context menus; native Adw.ShortcutsDialog; Preferences in the main menu.

Wallet: the original user-edited exported Monero mesh is reused, with projected
card geometry, depth, pointer parallax and directional wallet transitions. The
current Cairo material is **not an exact port of Telegram's GLSL renderer**.
Exact shader lighting, engraving, texture/material behavior and the integrated
precision control are still outstanding. Do not mark that review item complete.

The media viewer follows Galeria's in-window overlay/header/edge-controls/slide
stack pattern, adapted to the bundled prototype media. It does not implement
Galeria's full edit/video/export/backend features. GIF content is generated local
artwork, not a remote GIF search service.

Verification: Nix package checks exercise local models and native GTK creation,
compact header bounds, dialogs, the actual ShortcutsDialog, rendered mesh and
animated viewer under Xvfb with fatal GTK criticals. Isolated mock-data captures
check light/dark and windowed layouts. No user's desktop or phone was captured.
All state remains fictional and in memory.
