"""Slot contributions (section 4)."""

SLOTS = (
    "sidebar.section", "sidebar.entry", "toolbar.item", "workspace.view", "inspector.panel",
    "preferences.page", "modal", "statusbar.item", "asset.action", "library.source",
    "search.provider", "drop.target", "command",
)


class AppliesTo(dict):
    """applies_to: AppliesTo(kinds=["image"], min=1, max=None, ext=["mkv"])."""

    def __init__(self, kinds=(), min=1, max=None, ext=()):
        super().__init__(kinds=list(kinds), min=min, max=max, ext=list(ext))


class Contribution(dict):
    """A slot contribution: a dict with slot, id and props. Use the slot helpers."""

    def __init__(self, slot, id, **props):
        if slot not in SLOTS:
            raise ValueError(f"unknown slot {slot}")
        super().__init__(slot=slot, id=id, **props)

    def prop(self, key, value):
        self[key] = value
        return self

    @staticmethod
    def sidebar_section(id, title, icon, order=0):
        return Contribution("sidebar.section", id, title=title, icon=icon, order=order)

    @staticmethod
    def sidebar_entry(id, title, icon, opens, order=0):
        return Contribution("sidebar.entry", id, title=title, icon=icon, opens=opens, order=order)

    @staticmethod
    def toolbar_item(id, group, order=0):
        return Contribution("toolbar.item", id, group=group, order=order)

    @staticmethod
    def workspace_view(id, title, icon):
        return Contribution("workspace.view", id, title=title, icon=icon)

    @staticmethod
    def inspector_panel(id, title, icon, applies_to, order=0):
        return Contribution("inspector.panel", id, title=title, icon=icon, order=order, applies_to=applies_to)

    @staticmethod
    def preferences_page(id, title, icon, order=0):
        return Contribution("preferences.page", id, title=title, icon=icon, order=order)

    @staticmethod
    def modal(id, title, size="medium", modal=True, resizable=True):
        return Contribution("modal", id, title=title, size=size, modal=modal, resizable=resizable)

    @staticmethod
    def statusbar_item(id, align="right", order=0):
        return Contribution("statusbar.item", id, align=align, order=order)

    @staticmethod
    def asset_action(id, title, icon, applies_to, selection="one"):
        return Contribution("asset.action", id, title=title, icon=icon, applies_to=applies_to, selection=selection)

    @staticmethod
    def command(id, title, shortcut=None):
        c = Contribution("command", id, title=title)
        if shortcut:
            c["shortcut"] = shortcut
        return c

    @staticmethod
    def drop_target(id, accepts, where):
        return Contribution("drop.target", id, accepts=list(accepts), where=where)

    @staticmethod
    def library_source(id, title, icon, capabilities):
        return Contribution("library.source", id, title=title, icon=icon, capabilities=list(capabilities))

    @staticmethod
    def search_provider(id, title, icon, mode_id):
        return Contribution("search.provider", id, title=title, icon=icon, mode_id=mode_id)
