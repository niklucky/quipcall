# Quipcall UI

Less is more. Outside a call the app is one window: your rooms and their calls. During a call there is no window by default, just one small widget with the call floating over whatever you're working on.

The clickable prototype these screens were built from is [prototype/ui.html](prototype/ui.html) (open it in a browser). Some parts remain planned, including contacts, ringing and shared files.

## First launch

A small window asks what people should call you, with your avatar updating as you type. The name is cached on this Mac (`local.json`); once signed in, the pencil next to your name also updates your account name. `--name` skips the question.

Sign in opens the browser for an email magic link and explicit approval of this Mac by typing its displayed code in the browser. **Cancel sign-in** stops a pending attempt immediately; **Sign in** can then start a fresh one. Registration requires an account invitation; room invitations grant room access independently. The native session stays in Keychain. Guests can join through the web with a name. Apple and other OAuth providers are planned.

## Home

One window (960 × 640), two panes:

```
┌──────────────┬───────────────────────────────────────────────┐
│ ●●●  ⚡Quick │ Team standup ✎                  🔗 Access [Call] │
│ [Rooms|History] +  (A)(M)(L) Anna, Max and you  ‹ Today 10:02 · 32 min › │
│ (A+3) Team standup │ Transcript   Recordings 0   Files 0    │
│       Today 10:02 · 32 min │ ┌ Transcripts are on their way… ┐ │
│ (M+1) Family │ ── Call started · 10:02 ──                     │
│ …            │ ── Anna joined · 10:02 ──                      │
│ (N) Nikita ✎ │ ── Call ended · 10:34 · 32 min ──              │
└──────────────┴───────────────────────────────────────────────┘
```

- **Rooms**: one avatar (whoever you've talked to most there) with "+N", the name, room type, and the last call's day, time and length. Clicking a row shows its latest call; the phone that appears on hover joins.
- **History**: every call by day (Today, Yesterday, weekday, then the date), quick calls included.
- **The selected call**: title (owners rename shared rooms; saved links have a local name), copy link, **Access**, **Call**. Below it, who was there and ‹ › through the room's earlier calls. A quick call stays in History and offers **Keep as room** and **Call again** instead. Keeping it saves a bookmark for the current account; renaming that bookmark changes only its name on this Mac, including for someone else’s room. Bookmarks survive server refresh and account switching.
- **Tabs**: *Transcript* shows the call's timeline (who joined, left, shared, recorded and transcribed, with times) with what was said between, as a chat, if the call was transcribed. *Recordings* lists what you recorded in the call (time, length, size), with Open (QuickTime Player) and Show in Finder, and *Voices* for the folder of each voice on its own. A recorded call's Transcript tab has a player at the bottom (▶, the time, a bar to jump along); click a line to play from where it was said, and the line playing is outlined. *Files* shows an empty state until files exist.
- **Quick call** is top left; **+** opens a name field and the choice of **Private** (default) or **Public**. Creation requires an account.
- When a call ends, home opens on it.

## Joining from a link

Call and invitation links open a focused join page, separate from account and room management. The page checks access without creating a guest, claiming an invitation or reserving a 1-to-1 guest slot. A valid guest invitation is claimed only when the person chooses to join or open the Mac app.

The browser starts with microphone and camera off. Guests can enter their name, preview their camera, check microphone level and choose input devices before joining. Permission denial explains how to recover and still allows joining with the device off. The call uses the preview's selected devices and on/off choices; preview tracks stop when joining, leaving the page or opening sign-in. Browser preview settings apply to browser calls; the Mac app uses its own device settings.

After accepting an invitation, the address becomes the ordinary room link so the same browser can return. Unclaimed invitations survive reload and sign-in in the current history entry. Consumed or expired invitations, removed access, full 1-to-1 calls and unavailable/private rooms have distinct recovery screens. Member invitations ask for the invited account before claiming access. The layout adapts to small screens and keeps the room name above the preview.

**Open in the Mac app** uses `quipcall://open/<room>` (build 56+) to request explicit browser approval, even when a different identity is already signed in on the Mac. The private polling proof exchanges the browser identity for a Keychain session; session tokens never appear in the link. Complete approval in the browser where the invitation was accepted, using the code displayed in the Mac app. Existing `quipcall://join/<room>` links still join using the Mac's current identity.

## Room access

**Access** on Home and **Invite** during a call open the same native window. Public rooms admit anyone with their address. A private room address only works for people who already have access.

Owners of private rooms can create a single-use guest link or an invitation bound to a member’s account email. The app displays the generated link for copying; it does not email the room invitation. One guest can claim a guest link and return until manually removed. Unclaimed invitations expire after seven days. Members need an existing account or a separate registration invitation.

Owners see people and pending invitations, can revoke unused invitations, and can remove someone after confirmation. Access is revoked even if disconnecting the ongoing call fails; the error remains visible and **Disconnect again** retries it. **Close room** requires confirmation, prevents new joins and disconnects everyone while keeping local history. Non-owners see the room address and their room information. Refresh updates people and invitations; switching accounts clears the access window.

## Call: minimal mode (default)

```
              ╭──────────────────────────────╮
              │ [Anna ▣] [Lee ▣] [You ▣]     │   ← camera on: video tiles above the row
              │ (M) (T)  ((S)))+3 │ 🖥 🎙 ⏻  │   ← the row: recent speakers, the stack, three buttons
              ╰──────────────────────────────╯
                 Camera · Invite · Call window   ← a row that opens on hover
```

One small glass widget floats above all apps and Spaces and never takes focus. Prototype: [prototype/call-widget.html](prototype/call-widget.html).

- **The row** shows up to three people who were active last (spoke, unmuted or joined). When someone new speaks and the row is full, they take the place of whoever has been quiet longest; the others stay where they are. **You** are always last in the row (a red badge while muted) and don't count toward the three, so muting moves nothing.
- **The stack**, a grey pill with one dimmed face, the next two peeking out behind it and "+N", holds everyone else muted or quiet for two minutes.
- **Cameras**: people in the row with their camera on (you included) show as video tiles above the row, named on the tile. Faces in the row are avatars.
- **Three buttons, always**: Share, Mic, Leave. Hovering them opens a row underneath with Camera, Invite, Call window, Record and Transcribe.
- **Transcription**: *Transcribe* opens a row that says which model will run ("Parakeet v3 on this Mac. Everyone sees it.") with *Start*, or *Download 740 MB and start* when the model isn't here yet, and *Settings*. When others' Macs have a model, it asks "Transcribe on whose Mac?" with *This Mac* and up to two names; asking someone puts "Tester asks your Mac to transcribe" with *Accept* and *Not now* in their widget (Settings can accept by itself). If the transcriber leaves, everyone who can carry on gets "Anna left. Transcribe here?". While anyone transcribes, a captions button with a red dot sits left of Share in everyone's widget (right of the record button), and Transcribe leaves the hover row: one transcript per call. Click it to open or close the transcript window; hover it for "On your Mac · Parakeet v3" or "On Anna's Mac · Parakeet v3", with Stop, which anyone may press. The download's progress shows there too, with Cancel. A dropped connection is tried twice more by itself; if the download still fails, the widget's row (and the call window's title bar) says "Download failed" with *Settings*, where *Download* goes on from where it stopped.
- **The transcript window** is a floating panel like a share window (above other apps, on all Spaces, never takes focus), opened by itself for everyone when a transcript starts (Settings can turn that off). A chat: others on the left with their name over each turn, you on the right, joins, leaves, shares and recordings as dividers. A line being spoken is grey and grows about once a second, then turns solid. It follows the end while you're there; scroll up to read. ⋯ has Copy transcript, Hand over to … (each Mac that can, while yours transcribes) and Stop for everyone. Drag it by any empty spot, resize it from its corner, close it with ×: the transcript goes on.
- **Recording**: *Record* opens a row with *Audio* and *Audio and screen*. Audio records everyone's voice on your Mac (Movies › Quipcall). Audio and screen opens the share picker, titled "Record your screen", and records that screen or window with everyone's voice (an .mp4). While anyone records, a red record button sits left of Share in everyone's widget, and Record leaves the hover row: one recording per call. Hovering the red button shows "Recording audio · 4:12" with Stop, or "Anna is recording. Only Anna can stop it." Others get a notice when a recording starts; you get "Recording saved" with Show in Finder when it stops. Design: [prototype/transcription.html](prototype/transcription.html).
- **Hovering the stack** opens a row with everyone in it (name, mic state). **Hovering a face** for a moment shows the name and what they're doing ("speaking", "active 40 s ago").
- **While you share**, the widget grows upward with your screen's live preview on top (once its first frame arrives, in its shape), and a Stop button.
- **Someone sharing** has a screen badge on their face or tile; click it to bring their share window to the front. Anyone sharing stays in the row.
- **The main row never moves**: the widget grows around it, up for tiles and your preview, down for the hover rows, and sideways from its left end, so Share, Mic and Leave stay under the cursor. It's as wide as its row (wider only while your preview or a notice shows, never narrower than the hover row of controls, which sits right-aligned under the buttons), so hovering never changes the width; people joining and leaving do. Near the bottom of the screen the hover rows open above the main row instead, and near the top, tiles and your preview open below it. A row closes once the cursor has been outside the widget for a quarter of a second.
- "Joining…", permission prompts and errors show in a slim row under the main one.
- **Drag** the widget by any empty spot.
- Someone else's **screen share** floats as its own rounded rectangle you can move and resize. **Double-click** it, or press ⤢ on it, to watch it **fullscreen** (macOS fullscreen in its own Space). Esc, a double-click or ⤡ brings it back. When the share ends it says so and closes a moment later, but only once you're on its Space: macOS can't take a window out of fullscreen from another Space, and closing it anyway left a black Space that ignored every key and click.
- **Screen share** opens a picker with live thumbnails of your screens and windows and the frame-rate presets (5 / 15 fps for text, 30 / 60 fps for motion). Clicking share again stops sharing.

## Call: regular mode

One window (1040 × 680): the room's name and the call's time in the title bar, with "Invite" (opens Room access). Tiles sit on a dark stage, a control bar floats under them, and a side panel opens on the right.

- **The layout picks itself**: alone, a card with your face and the call link; 2–4 people, a grid; more, whoever is speaking big with a strip of the others (as many as fit, then "+N"); anyone sharing, the share big and faces in a column. Your own share gets the big tile when nobody else shares.
- **Controls**: mic, camera, share, record, transcribe | side panel, minimal | Leave. Record opens a small menu over the toolbar (Audio, Audio and screen); pressed again while you record, it stops. While the call is recorded, the title bar shows "REC · 4:12" (yours) or "Anna is recording".
- **Side panel**: *Transcript* (the call's timeline with the transcript as a chat, as in the transcript window) and *People* (the call link and everyone, with their mic state, and *Ask to transcribe* or *Hand over* next to those whose Mac can transcribe).
- **Transcribe** sits after Record in the toolbar: it asks to start (or download first), stops the transcript, or cancels the download. The title bar shows "Transcribing" or "Anna's Mac transcribes".
- Double-click someone's share for fullscreen. Closing the window doesn't end the call; it goes back to the widget. Leaving is the red button.

## Decisions so far (easy to change)

- Dark only, with today's blue accent. Colours are named by use in `theme.rs`, so a light set can follow the system later.
- Avatar colours come from the name, not the call identity, so people keep their colour from call to call.
- One widget instead of a window per face. Video in it loses the compositor's free circle clip; see below.
- Recording, files and the device menus (mic/speaker choice, noise filter) wait for their backends; the prototype shows where they go.
- Rooms and access are stored on the server and cached on this Mac. Call history, transcripts and recordings remain local.
- Your own camera isn't mirrored yet (GPUI can't flip video).
- The share picker needs macOS's Screen Recording permission once.
- Shares are always H.264, for both text and motion (VP9 cost too much; see FOOTPRINT.md).

## How it's built

- The widget and each floating share are borderless, non-activating panels (GPUI `WindowKind::PopUp`): above other apps, on all Spaces, never stealing focus. The widget's window resizes around its main row (`mac::Anchor::Row`), and macOS rounds its corners. It repaints at most 30 times a second.
- Glass is the system blur behind the window (GPUI's `Blurred` background, an `NSVisualEffectView`) under a dark tint (78%). The blur adds no colour, so over a white page the tint is what keeps the widget dark.
- GPUI clips video only to rectangles, so rounded tiles and previews are drawn in horizontal bands, each clipped to the shape's width, under an opaque ring that hides the steps (`rounded_video` in `ui.rs`): about 10 draws for a widget tile. Patching GPUI's surface shader for rounded corners would make it one draw, but that means vendoring GPUI itself.
- The text field is GPUI's `input.rs` example, adapted (`text_input.rs`).
- Screenshots and screen recordings show our floating windows. Our own screen shares leave every Quipcall window out through their ScreenCaptureKit filter, so nobody sees the widget (or a preview of the preview) in your share.
- Resizing draws the new frame in the same AppKit transaction, so the widget never shows its old frame stretched to the new size. That needed two GPUI patches (see `vendor/README.md`).
- No widget library: the look is custom, and every dependency costs footprint. Shared pieces (avatars, buttons, tooltips, empty states) are in `ui.rs`.
- Icons: [Lucide](https://lucide.dev) (ISC licence), embedded as SVG.

## Settings

⌘, or the gear next to your name on Home. Two tabs:

- *Transcription*: the language spoken (Auto-detect, or one of thirteen, which suggests the model that knows it best and greys out the others), the model (Parakeet v3 or Qwen3-ASR, each with what it's good at, whether it's on this Mac, and Download, Cancel or Remove), *Try it with your voice* (up to 30 s of your microphone through the model, drafts in grey, nothing kept), whether the transcript window opens by itself, whether this Mac starts when someone asks, and the models' credits.
- *Recording*: what Record offers first (Audio, or Audio and screen), each voice in its own file too, and the folder (Show in Finder, Change…).
