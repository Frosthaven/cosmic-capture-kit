---
title: Cloud Accounts
---

# Settings: Cloud Accounts

This page is where you connect a cloud drive, so the editor can upload a capture
straight to it and hand you a link.

!!! tip "Setting up a provider"
    The step by step instructions for each provider live on the
    [Cloud accounts](../cloud-accounts.md) page. This page only describes the
    controls in the settings window.

Nothing here is on by default. The app has no account of its own and no server
of ours behind it. Until you connect a drive yourself, nothing can leave your
machine.

## Connecting a drive

The **Connect a cloud drive** row explains itself: *Upload a capture to a drive
you have connected, and copy the link to it.* Press **Add cloud account**.

A dialog asks you to choose a provider, then sends you to your browser to sign
in. Google Drive, OneDrive, Dropbox and YouTube are supported. The dialog shows
a link you can copy, in case you would rather sign in on another device, and you
can **Cancel** at any point.

After signing in, the app asks **Where should uploads go?** Here you give the
account a name of your own and pick the folder uploads land in. The folder
browser lets you walk into folders, and it can also make a new one with **New
folder here** or remove one with **Delete this folder**. Press **Done** when the
destination is right.

## Managing connected accounts

Once connected, each account gets a row showing the provider's icon, the name
you gave it, and the folder it uploads to. Before you connect anything the
section reads "No cloud accounts are connected yet."

Each row offers:

- **Reconnect**, which appears only when the sign-in has expired and needs
  renewing.
- A gear button, tooltipped **Configure this account**, which reopens the naming
  and destination folder step.
- **Disconnect**.

Disconnecting asks first. It says the account "will be removed from this
computer, along with its sign-in", and that "Files already uploaded stay where
they are." That is worth reading twice: disconnecting is about this computer's
access, not about your files. Nothing you already uploaded is touched.

## What is not here

There is no default account picker and no share-link switch on this page. Both
of those live in the editor's upload popover, next to the capture you are about
to send. See
[Saving, copying and uploading](../editor/sharing.md#uploading-to-a-cloud-account).
