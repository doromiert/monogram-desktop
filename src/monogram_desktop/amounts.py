"""Decimal amount editing shared by the prototype's payment forms."""
import re
from decimal import Decimal


def valid_edit(text):
    return re.fullmatch(r"[0-9]*(?:[.,][0-9]{0,12})?",text) is not None


def positive(text):
    return bool(valid_edit(text) and any(c in "123456789" for c in text) and Decimal(text.replace(",","."))>0)


def bind(entry,button):
    accepted=entry.get_text()
    def changed(widget):
        nonlocal accepted
        text=widget.get_text()
        if not valid_edit(text):widget.set_text(accepted);widget.set_position(-1);return
        accepted=text;button.set_sensitive(positive(text))
    # GtkEditable's in/out insert-text position is not marshalable in this GI version.
    entry.connect("changed",changed);button.set_sensitive(positive(accepted))
