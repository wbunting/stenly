+++
title = "Privacy policy"
description = "Privacy policy for Stenly: what stays on your Mac, which optional services receive data only when you turn them on, and how Stenly uses Google Calendar data."
template = "legal.html"

[extra]
contact_intro = "Questions about privacy, or a request about data that you believe we hold"
+++

# Privacy policy

<p class="updated">Last updated 3 November 2026</p>

Stenly is a local-first meeting recorder and dictation app. This page says what stays on your Mac, what can leave it, and who gets it when it does. It covers the Stenly desktop app, its command-line tools, this website (stenly.ai) and the default share service at share.stenly.ai.

## The short version

- Recordings, transcripts, voiceprints and notes stay on your Mac. Speech is turned into text on your device with the Parakeet model.
- Nothing in this list happens until you turn it on or choose it: AI summaries from an outside provider, Google Calendar sign-in, encrypted share links, Notion, Slack and webhook destinations. The one exception is the update check in official release builds, which is on by default and can be turned off (see "Update checks" below).
- We (the people who make Stenly) do not receive your recordings, transcripts, notes, calendar data or voiceprints. There is no Stenly account and no Stenly server that holds them.
- Stenly has no analytics and no crash reporting. This website has no analytics, no tracking scripts and no cookies of its own.

## What stays on your Mac

Stenly stores these in folders on your Mac. By default that is `~/Documents/stenly`, and you can change it.

- Audio recordings of your microphone and of the audio playing on your Mac.
- Transcripts, with who said what.
- Voiceprints: the small voice fingerprints Stenly keeps so it can put names on speakers. They never leave your Mac.
- Notes and summaries, as Markdown files, including any Obsidian vault you point Stenly at.
- Your settings, your word list, and calendar details attached to a meeting you recorded (see below).

Secrets such as API keys, sign-in tokens and your licence key are kept in the macOS Keychain, not in plain settings files.

You are in charge of this data. Delete a meeting's folder and it is gone from Stenly. We cannot delete it for you, because we never had it.

## Optional services, and exactly what each one receives

Each service below sends data only if you set it up or switch it on. Stenly does not choose one for you.

### AI summaries

Stenly writes meeting notes with a summary provider you choose in Settings. The default is Ollama running on your own Mac, which sends nothing anywhere. The other choices send text to a company:

- **Sign in with ChatGPT.** Summary requests go to OpenAI, using your ChatGPT plan. Stenly keeps the sign-in in your Keychain.
- **The Codex or Claude Code command-line tool you already have installed.** Stenly runs the tool on your Mac, and the tool sends the text to OpenAI (Codex) or Anthropic (Claude Code) under your own login.
- **An API key** for OpenAI or any other OpenAI-compatible service you name, such as Anthropic, Groq or OpenRouter. The text goes to that service.
- **Ollama** (or another endpoint) running on your own Mac or network. Text goes only where you point it.

What the provider receives is the **transcript text**. Stenly never sends audio. The request also carries details that help the model write a better note: the meeting title, the names of the other people in the meeting (with the domain of their email address, such as a company's website name, to tell colleagues from guests), your own name if you set it, spellings from your word list, and the invite's agenda text if the meeting came from a calendar event.

Two related features are off by default and send the same kind of text to your chosen provider when you turn them on:

- **Meeting context** looks up background on an upcoming meeting. It can send the invite's title, attendee names and emails and agenda to your chosen model, and it can query your past meetings, your Obsidian vault, Notion, and any MCP servers you add.
- **AI clean-up for dictation** sends the words you dictated (and any text you had selected) to your chosen provider.

What those companies do with the text is covered by their own terms and privacy policies and by your account with them. Please read them before you pick a provider.

### Google Calendar sign-in

**Not available in released builds yet.** Sign in with Google is built, but the current macOS release does not include it. This section describes how it will behave once a release enables it.

If you choose **Sign in with Google**, Stenly asks Google for one permission: **read-only access to your calendars** (`calendar.readonly`). It cannot create, change or delete anything.

- **What Stenly reads:** the list of your calendars, and events from the calendars you choose to watch: title, time, attendees, organiser, meeting link and description. It also reads your account's email address.
- **What it is used for:** showing your upcoming meetings, offering to record them, linking a recording to its event, and naming the people on the call.
- **Where it is kept:** Stenly fetches events straight from Google to your Mac. Your sign-in tokens are stored in the macOS Keychain. Stenly fetches events from Google as it needs them and does not upload them anywhere. When you record a meeting from an event, its title, time, attendees, link and description are saved in that meeting's folder on your Mac, and names from it can appear in your notes.
- **Who else can see it:** we cannot, because the data never passes through any Stenly server. It is not used for advertising and not sold. If you use AI summaries or meeting context with an outside provider, the event details listed in the AI summaries section above are sent to that provider, because they help name people and spell their names correctly. Choose Ollama and none of it leaves your Mac. If you connect an AI tool over MCP, or send notes to a destination, those tools can see attendee names as described below.
- **Signing out:** Settings → Calendar → sign out removes the account's tokens from your Keychain and asks Google to revoke Stenly's access. You can also remove Stenly at [myaccount.google.com/permissions](https://myaccount.google.com/permissions).

Stenly can also read calendars from Calendar.app on your Mac, from an ICS link you paste in, or through a Kody server you connect. Those are separate choices. An ICS link goes only to the address you gave, and Kody is a service you set up with its operator.

**Google API Services User Data Policy.** Stenly's use and transfer to any other app of information received from Google APIs will adhere to the [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy), including the Limited Use requirements. In plain words: Stenly uses Google Calendar data only to provide the visible features described here; it does not transfer the data to others except to provide those features at your direction (for example, sending event details to the summary provider you chose), to follow the law, or as part of a sale or merger with notice to you; it does not use the data for advertising; and no person at Stenly reads it, because it never reaches us.

### Encrypted share links

If you turn on a **Stenly share link** destination, Stenly encrypts a note on your Mac and uploads only the encrypted version to the share server. The default is share.stenly.ai. You can point it at a server you run yourself. The key that unlocks the note sits in the part of the link after the `#`, which browsers do not send to servers. So the server cannot read the note, its title, its date, who was there or the transcript.

The server does store, for each share:

- the encrypted note and its size;
- when it was created, updated and when it expires, when it was revoked if it was, and how many times it was opened;
- a one-way hash of a secret that lets you update or revoke the share;
- **a plaintext title, only if you opt in** (it is off by default). That title is stored unencrypted so link previews can show it.

It also sees ordinary request details such as your IP address. The server keeps IP addresses in memory to rate-limit abuse and does not write them to its database. A hosting or network layer in front of it may keep ordinary access logs.

By default a share expires after 90 days (the server's default maximum is 365). When you revoke a share, the encrypted note is deleted at once. When one expires, it is deleted within about ten minutes. A small record that the link existed is kept for another 30 days, so the link can answer "expired" rather than "not found", and then it is forgotten. Anyone who has the full link can read the note, so treat links like passwords. A server you run yourself is yours to look after, and this policy does not cover it.

### Destinations: Notion, Slack, webhooks and Obsidian

- **Obsidian and Markdown folders** are files on your Mac. Nothing is sent anywhere.
- **Notion** receives the note, title, date, attendees, tags and meeting link, and the transcript if you choose that, in the Notion workspace you connect.
- **Slack** receives the note, with the title, date and attendee names, in the channel of the incoming webhook you add.
- **A webhook** receives a JSON copy of the meeting at the address you enter: title, times, attendees with their emails, meeting link, summary and note, and the transcript if you choose that.

Stenly sends to these only after you set one up, and it only does so automatically after each meeting if you switch that on. Their tokens and webhook addresses live in your Keychain. What the receiving service does next is up to that service.

### AI tools you connect (MCP)

Stenly can serve your meetings to AI tools on your Mac through an MCP server (local only). The tool can search your notes and transcripts and, if you connect a calendar, see upcoming events with attendee names. Content that an AI tool receives may then be sent on to that tool's own model provider, so choose tools you trust.

### Your licence and payment

The official macOS build offers 10 free meetings and then needs a licence ($10, one time). Payment goes through **Polar**, which is the merchant of record. Polar takes your payment, handles tax, and holds your email address and billing details under its own privacy policy. Stenly never sees your card.

When you enter your licence key, Stenly sends the key and a label for your Mac to Polar to activate it. The label is the Stenly version plus a random code, such as "Stenly 1.0 on macOS (a1b2c3)". It does not contain your name, Mac name or hostname. About once a week while you are online, Stenly sends the key to Polar again to confirm it is still valid. Your free-meeting count is kept on your Mac, in a file and the Keychain.

Source builds have no licence check and make no licence requests.

### Update checks

Official release builds of Stenly (macOS and Linux) check for new versions **by default**; you can turn this off in Settings → General → Release notifications, and if you do it stays off. Builds you compile yourself have no release source and never check. A check is one HTTPS request to GitHub (api.github.com) for the latest release of the Stenly repository, about 30 seconds after launch and then every 6 hours while Stenly is open, but not while a recording or dictation is in progress. The request contains what any web request does: your IP address and a user-agent that includes the Stenly version (for example "Stenly/0.1.3"). It sends no meeting content, no account and no system profile. GitHub can see the request under its own privacy statement. If a newer release exists, Stenly shows a notice once per version, a menu bar item and a link to its release page. It never downloads or installs anything for you. Versions before 0.1.3 do not check; new versions are always on the [releases page](https://github.com/wbunting/stenly/releases/latest).

### Downloading speech models

The first time you use speech recognition, Stenly downloads the speech model files (Parakeet, a voice-activity model and a speaker model) from Hugging Face and GitHub. Those requests carry no meeting content, but those sites can see your IP address as for any download. After that, transcription runs on your device. If you pick a local Ollama summary model, Ollama downloads it from its own library.

## What we collect: nothing about you

- **No analytics and no crash reporting.** The app does not report usage, errors or crashes to us or to anyone else.
- **No tracking on the website.** stenly.ai loads its own fonts and styles from its own address, has no analytics or advertising scripts, and sets no cookies. The site is served through Cloudflare, which, like any web host, handles ordinary request logs such as IP addresses to deliver and protect the pages.
- **No accounts.** You do not create a Stenly account.

If any of this changes, this page will be updated before the change ships.

## Recording other people

Stenly records audio. No bot joins your call, but that does not mean nobody needs to be told. You are responsible for telling everyone that you are recording and for getting the consent that the laws where you and they are require. See the [terms](/terms/) too.

## Children

Stenly is not aimed at children, and we do not knowingly collect anything from them.

## Changes

If this page changes in a way that matters, the "last updated" date will change. Earlier versions are in the public history of the website's source code.
