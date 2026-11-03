# Icons

`com.rinflow.linux.svg` and `com.rinflow.linux-symbolic.svg` are the
application's own icons.

`scalable/` holds the symbolic icons the window uses, copied unchanged from
the GNOME Adwaita icon theme (https://gitlab.gnome.org/GNOME/adwaita-icon-theme,
Creative Commons Attribution-ShareAlike 3.0 / LGPL 3), so the window has a
complete set on desktops whose theme does not ship them (adwaita-icon-theme
dropped most device icons after GNOME 46). They are bundled in the
GResource and registered with `IconTheme::add_resource_path`; the desktop's
own theme still wins where it provides an icon.
