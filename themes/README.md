# Glimt themes

Each `.theme` file here is a theme for [Glimt](../README.md). The ones in this folder are built
in; to add another, save its file in `~/.config/glimt/themes/` and choose it in
Preferences → Theme. Edit → Preferences → Save as Theme… writes the look shown now as a file
to start from.

A theme is `key=value` lines; `#` starts a comment. Every key may be left out: the light or
dark style and the accent colour then follow the desktop, the rest Glimt's defaults.

| Key | Value | What |
|---|---|---|
| `dark` | `true` or `false` | Dark or light style |
| `accent` | `#rrggbb` | Selection, links, toggled buttons |
| `panel` | `#rrggbb` | The file list, toolbar and panels |
| `window` | `#rrggbb` | Dialogs and menus |
| `field` | `#rrggbb` | Text fields |
| `faint` | `#rrggbb` | Striped rows |
| `hover` | `#rrggbb` | A button under the pointer |
| `active` | `#rrggbb` | A button being pressed |
| `border` | `#rrggbb` | Edges of windows; when set, of fields and buttons too |
| `text` | `#rrggbb` | All text |
| `radius` | `0`–`20` | Roundness of corners |
| `text_size` | `10`–`24` | Body text in points; the rest in proportion |

High Contrast (white on black) and Contrast (black on white) are meant for low vision.
Themes are welcome as pull requests.
