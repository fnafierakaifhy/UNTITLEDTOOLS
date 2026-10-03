# [UNTITLED] TOOLS

Native Rust app (window + optional local server) that saves the audio and minimal
metadata from an untitled.stream project link.

## Run
* `launch.bat` - builds on first run (via `build.ps1`), then opens the app.
* `powershell -ExecutionPolicy Bypass -File build.ps1` - build only (add `-NoRun`).
* `dist\untitled-tools.exe --serve` - browser UI on http://127.0.0.1:8787 (this PC only).
* `dist\untitled-tools.exe --cli <link> --out <folder> [--mode mp3|original|both] [--preview]`
  plus `--show-browser`, `--no-browser`, `--remember-login`, `--cookies ask|always|never`

## Use
1. Paste a link like `https://untitled.stream/library/project/<id>` (one per line for several).
2. Choose the output folder.
3. **Preview** shows exactly what would be saved. **Download** saves it.
4. The app opens the page in a **background browser** (Edge/Chrome/Brave already on your PC,
   in a brand-new temporary profile; nothing extra to download). It passes the site's bot
   check, reads the track list and downloads.
5. If the site wants a login or a human check, the app says so and offers
   **Open the browser window and retry**. Log in or tick the box in that window; the app
   continues by itself. **Open browser window now** lets you look at it any time to check
   it is working. **Remember my login between runs** keeps the login in the app's own
   profile folder (off by default; otherwise everything is deleted when the run ends).
6. If the server refuses a download, the app asks once whether it may use the browser's
   login session for this run. It is kept in memory only. Your real browser's cookies and
   passwords are never read.
7. Fallback: save the page (Ctrl+S) and use **Open saved page...**.

## Library
The **Library** tab shows everything you downloaded in a layout like untitled.stream's own
project page: cover, title, artist, track count, total length, track list, and a player bar.
Everything is read from the audio files themselves (tags, length, bitrate, sample rate,
embedded cover), so it works even if you switched the .json files off. `...` next to a song
opens **Properties** with everything found in the file.

## What to save
Grabber tab, **What to save**: audio, tags written into the MP3, cover images, artist name,
a .json per song, `_project.json`, `_tracks.csv`, `_playlist.m3u8`. Each can be switched off.
The account username, e-mail and profile picture are never saved.

## MSI installer
Put your artwork in `src\assets` (see `src\assets\README.txt`: icon.png, banner.png,
dialog.png, license.txt) and run `build-installer.bat` (or `build-installer.ps1`).
It builds the app, prepares the artwork, installs the .NET SDK and WiX tool if missing
(asks first), and writes `dist\UNTITLED-TOOLS-<version>-x64.msi`. Per-user install, no admin prompt.

## What is saved
`NN - Title.mp3`, `NN - Title.json` (title, length, BPM, key, format info, created date,
download flag), `_project.json`, `_tracks.csv`, `_playlist.m3u8`.

## What is NOT saved
Usernames, artist names, e-mails, profile pictures, cover art, account IDs, session
tokens, links. The page is read in memory; only an allow-list of fields is copied out
(see `struct Track` in `src/grab.rs`).

## Safety rules built in
* Talks only to untitled.stream hosts; media links on any other host are refused.
* Password-protected and paid projects are refused.
* Tracks whose owner disabled downloads are skipped unless you tick the permission box.
* Server binds 127.0.0.1 only, checks Host/Origin, and needs a random per-launch token.
* Filenames are sanitised (no path tricks, no reserved Windows names); files are written
  as `.part` and renamed when complete.
* Any access token you paste stays in memory: never logged, never written.

Only use this on music you made or have permission to save.
