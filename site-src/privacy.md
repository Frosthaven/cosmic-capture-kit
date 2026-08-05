---
title: Privacy
---

# Privacy

**Effective 5 August 2026.**

Cosmic Capture Kit is a screen capture tool that runs on your computer. It has
no accounts, no sign-up, and no servers of ours behind it. This page says
plainly what the app does with your data, and what leaves your machine.

## Your captures stay on your machine

Screenshots and recordings are written to the folders you picked in Settings.
Whatever the scanner reads stays in the app, on your machine. None of it is sent
anywhere on its own.

There is exactly one way a capture leaves your computer: you connect a cloud
account yourself, then press Upload on a capture. Nothing uploads
automatically, and there is no background sync.

## Cloud accounts, if you connect one

Connecting an account is optional and always starts with you, in Settings.
When you do connect one:

- The upload goes straight from your computer to that provider. It does not
  pass through anything of ours, because we run nothing in the middle.
- It lands in your own storage, in your own account. On Google Drive, OneDrive
  and Dropbox it goes into a single folder named "Cosmic Capture Kit", and the
  app asks each of them for the narrowest permission that reaches only that
  folder: on Google Drive it can touch only the files it created itself, and on
  OneDrive and Dropbox it is confined to one app folder. Nothing else in your
  drive is reachable.
- YouTube is the exception, and you should know why before you connect it.
  YouTube has no folders, and the permission it offers covers managing your
  YouTube videos, including deleting them. It is that wide because YouTube's
  narrower upload-only permission cannot delete a video at all, and the app has
  to be able to take one back off your channel in two moments: when you cancel
  an upload that has already gone through, and when you press Undo just after
  one finishes. That is the whole of what the delete power is used for. Nothing
  is ever deleted on a schedule, and nothing is touched that you did not just
  upload.
- We never ask for your name, your email address, or your profile. The sign-in
  permissions that would reveal who you are are deliberately left out.
- The app remembers a random identifier for each connected account. It is
  generated locally and says nothing about you.

You can disconnect an account at any time in Settings, which deletes its stored
sign-in from your computer.

## Where sign-in tokens are kept

When you connect a cloud account, the provider gives the app a token. That
token is stored in your operating system's own secret store: Secret Service on
Linux, the Keychain on macOS, and Credential Manager on Windows.

If that store is unavailable, for example on a machine with no desktop keyring
running, the token falls back to a file in the app's configuration folder that
only your user account can read. Tokens are never passed on a command line,
where other programs on the machine could see them.

## The debug log

The app writes a small debug log, for when something goes wrong and you want
to send us something useful. It is **on by default** and you can turn it off
any time in Settings, under Health. It stays small on purpose: the log rolls
over at a few megabytes and never grows past that.

It records what the app did, never what you captured. It does
not write file paths, file names, window titles, clipboard contents, scanned or
recognized text, audio, pixels, user names, host names, or network names. Where
a path matters for diagnosis, the log records its shape instead of the path: the
file extension, how long the name was, how deep the folder was, and whether the
folder existed and was writable. That is enough to tell "the folder is gone"
apart from "it was read-only", with none of your filesystem in it.

The log is a plain file on your computer. Nothing uploads it. It reaches us only
if you decide to send it.

## The update check

The app checks whether a newer version exists. It fetches one small file from
the project's public GitHub release page:

```
https://github.com/Frosthaven/cosmic-capture-kit/releases/latest/download/update.json
```

That request is a plain download with nothing attached: no identifier, no
account, no machine fingerprint, and not even the version you are running. The
app downloads the file and does the comparison locally. GitHub, as the host, can
see the IP address making the request, the same as any web page you open.

The check runs when you open the Settings window. If you keep the tray or menu
bar helper running, it also checks when that starts up, and turning off "Notify
me when an update is available" in Settings stops that one. An update is only
downloaded when you press Install.

## What the app never does

- No analytics, no telemetry, no usage statistics, no crash reporting.
- No advertising, and no data sold or shared with anyone.
- No accounts of ours, no registration, and no license check. There is no
  licensing machinery in the app at all, on any platform.
- No servers of ours at all. There is nothing to send data to.

## Scanning and other local features

QR codes and barcodes are decoded inside the app. Text recognition runs through
`tesseract`, a program installed on your own machine. Neither one contacts an
online service, so what you scan is never seen by anyone else.

Joining a Wi-Fi network from a scanned QR code hands the details to your own
system's network manager, on your machine.

Your settings, your capture folders, and your list of connected accounts are
ordinary files in your user profile. They stay there.

## Children

Cosmic Capture Kit is a general purpose tool and is not directed at children. It
collects nothing from anyone, of any age.

## Changes to this policy

If the app's behavior changes in a way that affects this page, the page changes
with it, and the effective date at the top moves. Meaningful changes are called
out in the release notes.

## About this website

This documentation site is static and has no comment system. It is hosted on
GitHub Pages, so GitHub can see the IP address of visitors in its own server
logs. The page fonts are loaded from Google Fonts, which means your browser
fetches them from Google when the page opens.

The site uses Google Analytics to count visits and see which pages help
people, and only if you say yes. A consent banner asks on your first visit;
analytics stays off until you accept, and rejecting changes nothing about how
the site works. If you accept, Google Analytics sets cookies and collects the
pages you view, the rough region you are visiting from, and basic browser and
device information. Google processes that data as the service's provider. You
can change your answer any time by clearing the site's cookies, which makes
the banner ask again.

## Contact

Questions, or something on this page that does not match what you observe? Open
an issue:

[github.com/Frosthaven/cosmic-capture-kit/issues](https://github.com/Frosthaven/cosmic-capture-kit/issues)
