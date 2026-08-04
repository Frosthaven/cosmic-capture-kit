---
title: Saving, copying and uploading
---

# Getting the capture out

There are four ways out of the editor, and one rule that covers all of them:
**none of them closes the editor**. Save your capture, copy it, upload it, then
carry on editing if you want. You leave when you press Close.

| Button | Key | What it does |
|---|---|---|
| Save | primary + S | Opens your system's save dialog |
| Copy to clipboard | primary + C | Puts the edited capture on the clipboard |
| Share | none | Opens your system's share sheet. Windows and macOS only |
| Upload | primary + U | Uploads to a cloud account you connected |
| Close | Esc | Closes the editor. It never deletes anything |

The primary key is Ctrl on Linux and Windows, and Cmd on macOS.

## Saving

There is one Save, and it always asks you where. That is deliberate: there is no
separate Save As to remember, because Save already is one. The dialog opens
pre-filled with the name and folder a plain save would have used, so pressing
Enter is the quick path and changing the destination is always available.

What it suggests, in order of preference:

1. Wherever you already saved this capture during this session.
2. Your configured screenshots or recordings folder, plus the capture's own
   name.
3. For a file you opened yourself, that file's own location.

Screenshots are always PNG, so the suggested name is corrected to match. A name
ending in another image extension is changed, and a name ending in something
else has `.png` added. Recordings keep the format they were recorded in.

The app does not invent unique names or add "edited" to anything. If you pick a
name that already exists, your system's own save dialog asks you about it, the
way it does for every other program.

## Copying

Copy puts the **current edited state** on the clipboard, annotations and all.

Remember that your capture was already copied for you the moment the editor
opened. Copy is for after you have marked it up.

A very large still image, over about a gigabyte, is not copied automatically at
open time, and you will see a note saying so. Copy still works if you ask for
it.

## Sharing

The Share button hands the edited capture to your operating system's own share
sheet, so you can send it wherever that offers. It exists on Windows and macOS.
Linux has no Share button.

## Uploading to a cloud account

Upload sends the capture to a drive you connected yourself, and can hand you a
link to it.

If you have not connected anything, the button is greyed out and its tooltip
says "Upload (no cloud accounts yet)". Pressing Ctrl+U in that state tells you
where to go: "No cloud accounts yet. Connect one in Settings." Connecting is
covered on the [Cloud accounts](../cloud-accounts.md) page.

### The upload popover

Pressing Upload opens a small panel:

1. **Which account.** A chip at the top shows the account it will use, with the
   provider's logo. Click it to see all your connected accounts and choose a
   different one.
2. **Visibility**, offering **Public**, **Unlisted** or **Private**. This only
   appears for YouTube, which is the only provider that has the idea. It starts
   on **Unlisted**, never Public, and it is remembered per account.
3. **Automatically Share & Copy URL**, a checkbox. **On by default.** It asks
   the provider for a share link and puts that link on your clipboard, so you
   can paste it straight into a message.
4. The **Upload** button, which starts the transfer.

Pressing Enter does not start an upload. Only the button does. An upload is not
the sort of thing that should happen because you leaned on the keyboard.

### The account it remembers

The app preselects the account you used last, as long as it is still connected
and can take the kind of file you have. YouTube, for example, only takes videos,
so a screenshot will not preselect it. When there is no valid last choice, you
get the first account in the list.

It remembers your choice the moment you pick it in the list, not when the
upload finishes, so choosing and then changing your mind still teaches it.

### While it uploads

**The editor stays open, and you can close it.** The transfer is handed to a
separate background process, so closing the editor, or even taking another
capture, does not interrupt it.

A meter appears in the editor's header: the provider's logo, a stop button, and
a bar. It starts as a spinner and turns into a real progress bar as soon as the
provider tells it how far along it is. There is no number printed on it, but
hovering shows one, along with the account name.

You also get short messages: "Uploading to *account*" when it starts, and
"Uploaded to *account*" when it lands, plus "Copied to clipboard" if a link was
copied. A desktop notification arrives too, and clicking it opens the link.

If you are using the tray or menu bar helper, the upload shows up there as well,
with a percentage, so you can watch it after closing the editor.

### Cancelling

Press the stop button in the meter. Its tooltip is "Cancel upload". The bar
turns red immediately, holds for a moment so you can see it registered, and then
goes away. You will see "Upload canceled".

One honest caveat: cancelling stops the transfer, but a partly uploaded file may
be left behind at the provider. The app does not tidy that up for you.

### Undoing an upload right after it finishes

This is worth knowing before you need it.

When an upload succeeds, the meter **stays on screen for four seconds**, and
during those four seconds the button that was the stop button becomes a **bin**,
tooltipped "Remove from *account*". Press it and the app deletes the file it
just uploaded, from the provider.

After four seconds the meter disappears and the upload is permanent as far as
the app is concerned. You can still delete the file yourself in the provider's
own website or app.

Two things the bin does **not** do: it cannot recall the desktop notification
that already went out, and it does not take the link back off your clipboard. If
you already pasted that link somewhere, deleting the file will break it rather
than unsend it.

## Closing with unsaved changes

Press Close, or Esc, and if you have edits that have never been written to a
file, you get a card headed **Unsaved changes**:

> Your edits haven't been written to a file yet. Save them, keep working, or
> close and let them go.

Three choices:

- **Save**, which saves and then closes.
- **Continue editing**, which puts you back where you were. This is the
  suggested one.
- **Close without saving**, which throws the edits away.

From the keyboard, **Enter** saves and closes, and **Esc** goes back to editing.
Esc is deliberately not the discard option, so a habit of pressing Esc can never
lose your work.

If saving then fails, the same card turns into the explanation, tells you what
went wrong, and offers **Exit anyway** so you are never trapped.

Whether you are asked at all is a setting, separately for screenshots and for
recordings, under
[Preview Editor](../settings/preview-editor.md).
