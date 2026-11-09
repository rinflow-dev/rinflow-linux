# Icons

`com.rinflow.rinflow.svg` and `com.rinflow.rinflow-symbolic.svg` are the
application's own icons, installed to `hicolor/scalable/apps` and
`hicolor/symbolic/apps`. Both are drawn from the Rinflow mark
(`docs/logo/rinflow-light.svg` for light backgrounds, `rinflow-dark.svg`
for dark ones), its round-joined strokes turned into filled outlines:

- the application icon has no light and dark versions (the icon theme
  specification has none): the warm-ink mark sits on a light tile with a
  darker edge, on GNOME's 128px grid, so it reads on light and dark shells;
- the symbolic icon is the mark alone in one colour (`#2e3436`), fills
  only, on a 16px grid: GTK recolours it to the theme's foreground, which is
  how it follows light and dark.

`scalable/` holds the symbolic icons the window uses, copied unchanged from
the GNOME Adwaita icon theme (https://gitlab.gnome.org/GNOME/adwaita-icon-theme,
Creative Commons Attribution-ShareAlike 3.0 / LGPL 3), so the window has a
complete set on desktops whose theme does not ship them (adwaita-icon-theme
dropped most device icons after GNOME 46). They are bundled in the
GResource and registered with `IconTheme::add_resource_path`; the desktop's
own theme still wins where it provides an icon.
