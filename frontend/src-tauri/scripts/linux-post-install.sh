#!/bin/sh
# Refresh the GTK icon theme cache and desktop-entry database so the
# installed app icon shows up immediately (taskbar, app grid, alt-tab)
# instead of falling back to a generic icon until the next unrelated
# cache rebuild. Both tools are optional depending on the desktop
# environment, so failures here must never fail the package install.
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi

exit 0
