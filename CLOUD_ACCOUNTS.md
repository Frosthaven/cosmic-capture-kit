# Setting up Cloud Accounts (Google Drive, OneDrive, Dropbox, YouTube)

**Do you need this guide?**

- **Google Drive, OneDrive and Dropbox, official build**: no. They work out of
  the box. Open Settings, go to Cloud Accounts, and press Connect.
- **YouTube, any build**: yes. YouTube needs your own registration even in the
  official app, for a quota reason explained in its section below.
- **Any provider, a build you made yourself from source**: yes, for each
  provider you want to use.

**Why the app might not list a provider at all.** The Add cloud account dialog
offers a provider only once this build has a registration for it. So a build
from source with nothing set up shows a short page pointing at this guide
instead of a list, and each provider appears in the list as soon as its
environment variable is set and the app is restarted. Nothing is hidden
permanently, and there is no setting to toggle: a provider is listed exactly
when it can actually be connected.

## Why this extra step exists

Cloud services like Google Drive will not let just any program sign someone in.
Before an app can show the "sign in with Google" screen, it has to be registered
with Google as an app first. That registration produces an id (a short string of
letters and numbers) that tells Google which app is asking.

The official, downloadable build of this app carries that registration for
Google Drive, OneDrive and Dropbox, so those three just work. A copy you build
yourself from source carries none of them, because those registrations belong to
one project owner, not to everyone who builds the code. So if you are building
from source and want to connect a cloud account, you need to register your own,
which is free and takes a few minutes per provider.

YouTube is the one provider nobody gets for free, official build included. Its
daily upload allowance belongs to the registration rather than to the person
using it, so one shared registration would run out within a handful of uploads
a day across everybody. Registering your own gives you the whole allowance to
yourself. The YouTube section below has the steps.

This id is not a secret you need to protect closely. It is closer to a name tag
than a password. You still register your own instead of using someone else's,
the same way you would not want to run a website under someone else's name.

## The general idea

For each provider you want to use, you will:

1. Go to that provider's developer site and create a small, private "app"
   registration under your own account.
2. Copy the id it gives you.
3. Tell Cosmic Capture Kit that id, using an environment variable (a small
   piece of text your computer can hand to a program when it starts).
4. Restart the app and try Connect again.

Below are the exact steps for each provider. Proton Drive and iCloud are not
supported yet (neither offers an official way for outside apps to connect), so
they are not in this list, and the app does not offer them either.

## One folder, on every provider

Whichever provider you connect, this app can only see and write to a single
folder called "Cosmic Capture Kit". Everything it uploads lands there, or in a
folder you make inside it, and nothing else in your drive is reachable at all.
Each provider enforces that a different way, which is why the steps below differ:

- **Google Drive**: the `drive.file` permission only ever grants access to files
  this app itself created. The app finds or creates the folder for you the first
  time you connect, in your My Drive.
- **OneDrive**: the `Files.ReadWrite.AppFolder` permission grants access to one
  app folder and nothing else. Microsoft creates it for you, under `Apps/`, and
  names it after your app registration.
- **Dropbox**: the app is registered with "App folder" access, so the folder
  under `Apps/` IS the whole of what its sign-in can reach.
- **YouTube**: no folders exist there, so there is nothing to sandbox. It has
  its own scope, covered in its section.

Because two of the three name that folder after your app REGISTRATION, name
your registrations "Cosmic Capture Kit" as you create them if you want the
folder to match what the app calls it on screen. Any name works; only the
folder's own name changes.

### Organising inside it

You do not have to leave your drive to arrange that folder. The step that
appears after you connect an account, and the gear button on the account's row
afterwards, is a small folder manager for it:

- **Where you are is where your captures go.** The path above the list names the
  folder you are looking inside, and that is where this account uploads. There
  is nothing separate to select.
- The list shows what is inside that folder. Click a folder to go into it, which
  also makes it the destination, and click any part of the path to come back out,
  which moves the destination back out with you.
- Press Done to save it. The account's row on the settings page then shows the
  full path. Cancel leaves the account's folder exactly as it was.
- Opening this step again later starts you inside the folder the account is
  already using, so you can see where you are before changing anything.
- The plus button beside the path makes a new folder at the level you are
  looking at. Type its name and press Enter.
- The bin button on a folder's row deletes it. It asks first, because the
  folder's contents go with it. The folder this account uploads to cannot be
  deleted, nor can one that contains it; move somewhere else and press Done
  first.

Everything here stays inside the one folder above. There is no way to reach the
rest of your drive from this app, and there is no way to point an account
outside it.

## Google Drive

1. Go to [console.cloud.google.com](https://console.cloud.google.com) and sign
   in with the Google account you want to upload to.
2. If you do not already have a project, create one. Any name is fine, this is
   just a container for the next steps.
3. In the left menu, go to **APIs & Services > Library**. Search for
   **Google Drive API** and click **Enable**.
4. Go to **APIs & Services > OAuth consent screen**. Google calls this whole
   area "Google Auth Platform", with its own tabs on the left (Overview,
   Branding, Audience, Clients, Data Access, Verification Center).
   - Choose **External**.
   - Fill in an app name (anything, like "My Screenshot Uploader"), your email
     as the support email, and your email again under developer contact.
   - Save. This takes you to an Overview page. You do not need to publish the
     app or ask Google to review it.
5. Click the **Audience** tab on the left. Scroll to **Test users**, click
   **+ Add users**, and add your own Google account's email address. Save.
6. Click the **Data Access** tab on the left, then **Add or remove scopes**.
   In the filter box, search for `drive.file`, and check the box for
   `.../auth/drive.file` ("See, edit, create, and delete only the specific
   Google Drive files you use with this app"). Click **Update**, then **Save**.
   (If it does not show up in the list, go back to step 3: this scope only
   appears once the Drive API is enabled for the project.)
   This step is easy to miss and matters: without it, signing in can succeed
   and still leave the app unable to actually use Drive, failing the first
   time it tries to list your folders with an error like "the cloud service
   refused this account's permission to list folders". If you connected
   before adding this scope, disconnect the account in the app and connect it
   again afterward.
7. Click the **Clients** tab on the left (or the **Create OAuth Client**
   button on the Overview page).
   - Application type: **Desktop app**. (Not "TVs and Limited Input devices".
     That type needs something this app cannot provide.)
   - Give it any name and click **Create**.
8. You will be shown a **Client ID** and a **Client Secret**. Copy both. Despite the
   name, Google's own documentation notes that this "secret" is not treated as
   sensitive here: it ships inside the app the same way the client id does, since
   PKCE (not the secret) is what actually protects the sign-in. Google's "Desktop
   app" client type is the one that issues a secret at all; Microsoft's and
   Dropbox's setups below do not need one.
9. Set the environment variable `CCK_GDRIVE_CLIENT_ID` to the Client ID, and
   `CCK_GDRIVE_CLIENT_SECRET` to the Client Secret (see "Setting the environment
   variable" below), then restart the app. Google Drive then appears in the Add
   cloud account list; press Connect on it.

There is nothing to set up on the Drive side for the folder. The first time you
connect (and the first time you open the gear on an account connected before
this app worked this way), it looks for a folder called "Cosmic Capture Kit" in
your My Drive and creates one if there is none. It looks before it creates, so
reconnecting the same account never leaves you with two of them. The folder is
an ordinary Drive folder: you can open it, move it, or move things out of it,
and the app carries on using the one it made.

## Microsoft OneDrive

Microsoft no longer lets a plain personal account create an app registration
on its own. It needs a "directory" (Microsoft's name for an organization)
behind it first. This sounds bigger than it is, and the steps below create
one for free. You will still sign in and upload with your normal personal
Microsoft/OneDrive account afterward. The directory is only a place to park
the app registration.

You might be tempted to try the "Microsoft 365 Developer Program" for this,
since it also offers a free sandbox directory. As of this writing it often
refuses personal accounts with a message like "you don't currently qualify",
a real restriction on Microsoft's side, not something you did wrong. Skip
straight to the Azure signup below instead, which uses a different,
more reliable path to the same thing.

1. Go to [azure.microsoft.com/free](https://azure.microsoft.com/free) and
   sign up for a free account, signing in with your Microsoft account.
   - It will ask for a **credit card**, for identity verification only. As of
     this writing, it does not charge anything unless you later create a
     billable resource (a virtual machine, a storage account, and so on),
     which nothing in these steps does.
   - Finishing signup automatically creates a Microsoft Entra ID directory
     for you. There is no separate "create a directory" step.
2. Go to [entra.microsoft.com](https://entra.microsoft.com) and sign in with
   the same account. You should land in (or be able to switch into) the new
   directory.
3. On the directory's Overview page, open the **"+ Add"** dropdown near the
   top and choose **App registration**. (This is not under a separate
   "Applications" menu; it lives in this dropdown.)
   - Name it anything.
   - Under "Supported account types", it starts on **"Single tenant only -
     Default Directory"**. Click it to open the full picker, and choose the
     option whose description mentions **personal Microsoft accounts**
     (wording varies, look for that phrase specifically). This is the
     setting that lets your normal, everyday Microsoft account sign in
     later, even though the app itself lives in this new directory.
   - Under Redirect URI, click **"Select a platform"**, choose **Mobile and
     desktop applications**, and enter `http://localhost` as the URI.
   - Click **Register**.
4. On the app's overview page:
   - Copy the **Application (client) ID**.
   - You should see **"Redirect URIs: 0 web, 0 spa, 1 public client"** ,
     confirming the redirect URI registered correctly.
   - Ignore **"Application ID URI"** and leave it blank. That is for an app
     exposing its own API for others to call, which does not apply here.
5. Open the app's **API permissions** page, click **Add a permission**, choose
   **Microsoft Graph**, then **Delegated permissions**, and add
   `Files.ReadWrite.AppFolder`. (Search the list for "AppFolder": the entry
   reads "Have full access to the application's folder".) You do not need to
   add `offline_access` by hand, and you do not need an administrator to grant
   anything: the app asks for both by name when you sign in, and you consent to
   them there.

   This is the permission that limits the app to one folder. Its wider sibling,
   `Files.ReadWrite`, would give read and write access to everything in your
   OneDrive, and this app deliberately does not ask for it. Microsoft creates
   the folder itself, under `Apps/`, named after this app registration.
6. Set the environment variable `CCK_ONEDRIVE_CLIENT_ID` to the client id you
   copied, then restart the app. OneDrive then appears in the Add cloud account
   list; press Connect on it. Sign in with
   your normal Microsoft/OneDrive account (not the Azure account, unless they
   happen to be the same one).

One caveat worth knowing before you sign in with a WORK or SCHOOL account:
Microsoft's own documentation disagrees with itself about whether app folders
work there. One page says the permission is for personal accounts only, another
says app folders work on OneDrive for work or school as well. Personal accounts
are the tested path. If a work account signs in and then cannot list folders,
that is this, not something you configured wrongly.

## Dropbox

1. Go to [dropbox.com/developers/apps](https://www.dropbox.com/developers/apps)
   and sign in.
2. Click **Create app**.
   - Choose **Scoped access**.
   - Choose **App folder**, not Full Dropbox. This limits the app to its own
     dedicated folder (under `Apps/` in your Dropbox) instead of your whole
     account, the same least-privilege idea as Google Drive's setup above,
     which only ever grants access to files this app itself creates. Choose
     carefully: Dropbox fixes the access type when the app is created, and
     changing it later means deleting the app and making a new one, with a new
     App key to set.
   - Give it any name. It becomes the folder's name under `Apps/`.
3. Open the new app's **Settings** tab.
   - Under **OAuth 2**, check **"Allow public clients (Implicit Grant &
     PKCE)"**. This one is easy to miss and required: without it, sign-in
     fails, since this app has no client secret to authenticate with here
     (Dropbox's public-client setting, and Microsoft's app registration above,
     both work this way; Google is the one exception, its Desktop app client
     type issues a secret too, which the Google Drive steps above cover).
   - Under **OAuth 2 > Redirect URIs**, add all four of these, exactly as
     written, one per line:
     - `http://localhost:47821/`
     - `http://localhost:47822/`
     - `http://localhost:47823/`
     - `http://localhost:47824/`
   - Dropbox checks these exactly, including the ending slash, so copy them
     as they are.
4. Under the **Permissions** tab, turn on: `files.content.write`,
   `files.metadata.read`, `sharing.write`, and `sharing.read`. Click
   **Submit** to save.
5. Back on the Settings tab, copy the **App key**. That is your client id.
   Ignore **App secret**; it is not needed.
6. Set the environment variable `CCK_DROPBOX_CLIENT_ID` to that value, then
   restart the app. Dropbox then appears in the Add cloud account list; press
   Connect on it.

## YouTube

YouTube is a video destination, not a file-storage drive: it only appears in the
app's Upload list when you are editing a recording, never a screenshot, and it
lets you choose a visibility (Public, Unlisted or Private) for what you upload.

**This one needs your own registration even in the official app**, unlike the
three above. YouTube's upload API works on a daily allowance that belongs to the
app registration and is shared by everyone using it: a project gets 10,000 units
a day and a single upload costs about 1,600 of them. A registration shipped with
the app would therefore give every user of that build about six uploads a day
between them, and the seventh person to record something would be turned away
with nothing they could do about it. Your own registration gives you the whole
allowance. That is why YouTube is missing from the Add cloud account list until
you set the variables below, in every build.

It uses the same Google Cloud project as Google Drive, just with one more API
enabled and one more scope requested. If you already did the Google Drive
setup above, skip straight to step 3. If you are on the official build and have
never done any of this, you will be creating a project of your own for the first
time here, so start at step 1.

1. Go to [console.cloud.google.com](https://console.cloud.google.com) and sign
   in with the Google account you want to upload to. If you do not already
   have a project, create one first (any name is fine).
2. In the left menu, go to **APIs & Services > Library**. Search for
   **YouTube Data API v3** and click **Enable**.
3. Go to **APIs & Services > OAuth consent screen** ("Google Auth Platform").
   If you have not set this up yet (you would have for Google Drive already if
   you did that section first), choose **External**, fill in an app name and
   your email as the support/developer contact, and save. You do not need to
   publish the app or ask Google to review it for this step to work.
4. Click the **Audience** tab, scroll to **Test users**, click **+ Add
   users**, and add your own Google account's email address, if it is not
   there already. Save.
5. Click the **Data Access** tab, then **Add or remove scopes**. Search for
   `youtube.force-ssl`, and check the box for the scope that mentions
   managing your YouTube account (viewing, editing and permanently deleting
   your videos). Click **Update**, then **Save**. This is the ONE scope this
   app asks for; it is broader than "upload only" because the app has to be
   able to take a video back off your channel when you cancel an upload that
   has already gone through, or press Undo just after one finishes, and
   YouTube's narrower upload-only scope cannot delete a video at all.
6. Click the **Clients** tab. If you already created a Desktop app client for
   Google Drive, you can reuse the SAME Client ID and Client Secret for
   YouTube; this scope is just added to the same client's consent. Otherwise,
   create one the same way the Google Drive steps above do (Application type
   **Desktop app**), and copy the Client ID and Client Secret it gives you.
7. Set the environment variable `CCK_YOUTUBE_CLIENT_ID` to the Client ID, and
   `CCK_YOUTUBE_CLIENT_SECRET` to the Client Secret (see "Setting the
   environment variable" below: these are separate variables from Drive's,
   even if you reused the same underlying client), then restart the app. YouTube
   then appears in the Add cloud account list; press Connect on it.

**A real Google-side limitation to know about before you test this**: a
project Google has not audited yet forces every video uploaded through this
API to Private, no matter which visibility you pick in the app. This applies
to any Google Cloud project created after 2020-07-28 that has not completed
Google's review process for this API (search for "YouTube API Services Audit
and Quota Extension Form" in Google's own documentation). A fresh project you
just created for this falls into that bucket. This is not a bug in the app;
it is Google's own restriction on unverified projects, and community reports
put the review turnaround at several weeks. Until your project is reviewed,
test with Private/Unlisted expectations in mind, or ask a teammate with an
already-reviewed project to set up the client for you.

## Setting the environment variable

An environment variable is just a named piece of text your computer keeps
around and hands to a program when it starts. This section uses YouTube's pair
as the worked example, since YouTube is the one provider you set up yourself
whichever build you are on:

```
CCK_YOUTUBE_CLIENT_ID       = the Client ID from your Google Cloud project
CCK_YOUTUBE_CLIENT_SECRET   = the Client Secret from the same client
```

Every other variable in this guide goes in exactly the same place, so once you
have done one you can add the rest beside it. Drive uses
`CCK_GDRIVE_CLIENT_ID` and `CCK_GDRIVE_CLIENT_SECRET`, while
`CCK_ONEDRIVE_CLIENT_ID` and `CCK_DROPBOX_CLIENT_ID` need no matching secret at
all (the Google-based providers are the ones that do). Drive's and YouTube's
values can be identical if you reused one Google Cloud project for both; they
are still separate variables.

Set them somewhere PERMANENT rather than typing them before a command. This app
is normally started from a keyboard shortcut, a launcher or the tray icon, and
none of those go through a terminal, so a variable that only exists in one
terminal window is a variable this app will usually never see.

**Restart the app after any change.** Variables are read once at startup. If
the app is already running in the background (the tray or menu bar icon), quit
it first, then start it again.

### Linux

For launches from a terminal, add `export` lines to your shell's startup file,
`~/.bashrc` for bash or `~/.zshrc` for zsh, or the equivalent in your shell,
then open a new terminal:

```sh
export CCK_YOUTUBE_CLIENT_ID="123456789-abcdefg.apps.googleusercontent.com"
export CCK_YOUTUBE_CLIENT_SECRET="GOCSPX-abcdefghijklmnopqrstuvwxyz"
```

That covers terminals and nothing else. **For the keyboard shortcut, the app
launcher and the tray icon, the variables have to be in the SESSION
environment**, which on a systemd-based system means a small `.conf` file in
`~/.config/environment.d/`. Create the folder if it is not there, and put plain
`KEY=value` lines in it, with no `export` and no quotes:

```sh
mkdir -p ~/.config/environment.d
cat > ~/.config/environment.d/cosmic-capture-kit.conf <<'EOF'
CCK_YOUTUBE_CLIENT_ID=123456789-abcdefg.apps.googleusercontent.com
CCK_YOUTUBE_CLIENT_SECRET=GOCSPX-abcdefghijklmnopqrstuvwxyz
EOF
```

Log out and back in for the session to pick it up. On a system without
systemd, your login profile (`~/.profile`) is the equivalent place, read the
same once-per-session way.

### macOS

Shell startup files (`~/.zshrc` and friends) work exactly as on Linux, and they
work for terminal launches only. **An app started from the Dock, from Spotlight
or from the menu bar icon never reads them**, which covers almost every way you
will actually start this app.

The practical route is a per-user LaunchAgent that runs `launchctl setenv` when
you log in. Create
`~/Library/LaunchAgents/dev.frosthaven.cck-env.plist` with this content,
replacing the values with your own:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.frosthaven.cck-env</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>launchctl setenv CCK_YOUTUBE_CLIENT_ID 123456789-abcdefg.apps.googleusercontent.com; launchctl setenv CCK_YOUTUBE_CLIENT_SECRET GOCSPX-abcdefghijklmnopqrstuvwxyz</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
```

Then load it once, so you do not have to log out to try it:

```sh
launchctl load ~/Library/LaunchAgents/dev.frosthaven.cck-env.plist
```

It runs at every login from then on. Quit and restart the app afterwards.

### Windows

The easiest of the three, and the GUI route covers every way the app can be
started, shortcuts included. Open Settings, go to **System > About > Advanced
system settings > Environment Variables**, and under **User variables** click
**New**. Add one variable per line of the list above, name on the left and
value on the right.

If you prefer a terminal, `setx` writes the same permanent user variable:

```powershell
setx CCK_YOUTUBE_CLIENT_ID "123456789-abcdefg.apps.googleusercontent.com"
setx CCK_YOUTUBE_CLIENT_SECRET "GOCSPX-abcdefghijklmnopqrstuvwxyz"
```

Either way, close and reopen any terminal you had open, and quit and restart
the app, before the new value is visible.

## Troubleshooting

- **A provider is missing from the Add cloud account list**: this build has no
  registration for it, which for YouTube is normal and expected on every build.
  Set its variable somewhere permanent (see "Setting the environment variable"
  above), restart the app, and it appears in the list.
- **"No cloud drives are set up" instead of a list**: none of the variables are
  visible to the app, which is the usual state of a build made from source
  before any setup. The dialog links to this guide. If you HAVE set them, they
  are almost certainly in a shell startup file while the app is being launched
  from a shortcut, launcher, Dock or tray icon: those never read a shell startup
  file, so move the variables to the per-platform location above.
- **"needs an app registration"**: the environment variable is not set, or is
  not being seen by the app. Check it is in the permanent location for your
  platform rather than one terminal window, and restart the app afterwards.
- **"redirect_uri_mismatch" or similar, right after signing in**: the redirect
  URI you registered does not match what is expected. Go back and check it
  is typed exactly as shown above, including `http://` and any trailing
  slashes.
- **The sign-in page does not open on its own**: Connect no longer opens a
  browser automatically. Instead it shows a clickable link; click it to
  continue.
- **The browser says you signed in, but the app then says the account
  "could not be connected"**: for Google Drive or YouTube specifically, check
  that you set the matching `_CLIENT_SECRET` as well as the `_CLIENT_ID`.
  Google's Desktop app client type issues both, and the sign-in fails right
  after the browser step if only the id is set.
- **"Reconnect needed: ... refused this account's permission to list
  folders"** (Google Drive): see step 6 above (the Data Access scope). If you
  connected before adding it, disconnect and reconnect the account afterward.
- **"Reconnect needed" on a OneDrive account that worked yesterday**: this app
  now asks for `Files.ReadWrite.AppFolder` instead of access to your whole
  OneDrive. A sign-in from before that change keeps working as a sign-in, it
  just does not carry the new permission, so the first thing the app tries
  comes back refused. Press **Reconnect** on the account's row and sign in
  again; you keep the account, its name and its settings. Add the permission to
  your app registration first (OneDrive step 5) if you registered your own.
  Google Drive and Dropbox accounts are unaffected and need no action.
- **Every YouTube upload lands Private, whatever visibility you picked**:
  see the "real Google-side limitation" note in the YouTube section above.
  This is Google forcing every upload to Private on an unaudited Cloud
  project, not something this app's Unlisted/Public options are failing to
  do.

If the app is already running quietly in the background (the tray icon or menu
bar icon), quit it first and start it again after any change. It reads these
variables once, when it starts, so a copy that was already running is still
using the old values.
