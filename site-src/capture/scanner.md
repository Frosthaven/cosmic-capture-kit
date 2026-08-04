---
title: The scanner
---

# The scanner

The scanner reads what is on your screen: QR codes, barcodes, and ordinary text.
It is the leftmost button in the kind group, and its tooltip reads "Scan for
codes and text".

Everything it does runs on your own machine. Nothing is sent anywhere to be
read.

## Scanning

Choosing Scanner puts you in region mode and hides the other mode buttons,
because a scan is always a scan of a region. Draw a rectangle over whatever you
want read.

Where the mode buttons were, there is now a single **Scan again** button. Press
it to read the region again. While a scan is running its icon spins and the
button reads "Scanning".

Each scan takes a fresh picture of the region at that moment. It does not use a
frozen screen, so if the thing you are scanning has changed since you drew the
rectangle, scan again and it will read the new state.

## What you see

**Codes** get an outline that follows the code's own orientation, so a tilted or
angled code gets a tilted outline rather than a rectangle around it. Hovering
one pops up a bubble telling you what it says and what clicking it will do.

**Words** are washed very faintly all the time, so you can see at a glance what
is selectable. The wash strengthens when you hover a word, and strengthens again
when it is selected.

## Clicking a code

Left-clicking a code does the sensible thing for that kind of code, and then the
app closes. The hover bubble always tells you which of these you are about to
get:

| What the code holds | What clicking it does |
|---|---|
| A web address | Opens it in your browser |
| A location | Opens it as a map |
| A phone number | Starts a call |
| An email address | Opens a new message, with the subject and body filled in if the code carried them |
| A text message | Opens a new message to that number |
| Wi-Fi details | Joins the network. See below |
| A contact card | Saves it and opens it, so your address book can take it |
| A calendar event | Saves it and opens it, so your calendar can take it |
| Anything else | Copies it to your clipboard |

If a code holds plain text that begins with something that looks like a web
address, the app will still offer to open it.

**Right-clicking a code** is the alternative. It opens a small menu with a
single item, "Copy QR Contents" for a QR code and "Copy contents" for anything
else. That copies the code's full raw contents, and unlike a left-click, it
leaves the app open so you can carry on scanning. Click anywhere else to dismiss
the menu.

### Joining a Wi-Fi network

Wi-Fi QR codes are the ones printed on a router, or handed out by a café. The
hover bubble reads "Join Wi-Fi" followed by the network name. Click it and the
app asks your system to join, using your operating system's own networking tool.
You do not have to type the password.

If your system does not have that tool available, the app does the next best
thing: it copies the password to your clipboard, so you can open your normal
Wi-Fi menu and paste it. If the network has no password, it copies the network
name instead.

## Selecting text

Text recognition needs one extra piece of software, `tesseract`, plus at least
one language pack. macOS and Windows official builds and Linux installs differ
here, and if it is missing, the text recognition switch in Settings is greyed
out with a note explaining why. The rest of the scanner still works.

Selecting recognised words works the way text selection works everywhere else:

| Gesture | What it does |
|---|---|
| Click and drag | Selects from where you started to where you are, picking up whole rows as you drag up or down |
| Double-click | Selects that line |
| Triple-click | Selects everything |
| Ctrl+click | Adds or removes a single word |
| Shift+click | Extends the selection to there |
| Ctrl+Shift+click and drag | Adds that range to what is already selected |

Right-clicking opens a menu with **Copy**, **Select all** and **Select none**.
Right-clicking a word that is not currently selected selects just that word
first.

| Keys | What it does |
|---|---|
| primary + A | Select all text |
| primary + C | Copy the selected text |
| primary + D | Clear the selection |

The primary key is Ctrl on Linux and Windows, and Cmd on macOS.

**Copying text does not close the app.** That is deliberate, and it is the
opposite of clicking a code. You can copy one paragraph, then another, then scan
somewhere else, all in one session. Close it with Esc when you are done.

## Turning each half on or off

Both halves of the scanner have their own switch in
[Settings, under Capture Modes](../settings/capture-modes.md), and both are on
by default:

- **Enable QR/barcode recognition**: recognised codes become clickable.
- **Enable text recognition (OCR)**: recognised words become selectable. There
  is a **Text matching strictness** slider alongside it.
