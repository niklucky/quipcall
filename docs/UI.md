# Quipcall UI

Less is more. Outside a call the app is one window: your rooms and their calls. During a call there is no window by default, just one small widget with the call floating over whatever you're working on.

The clickable prototype these screens were built from is [prototype/ui.html](prototype/ui.html) (open it in a browser). Some of it waits for the backend: sign-in, transcripts, recordings, files.

## First launch

A small window asks what people should call you, with your avatar updating as you type. The name stays on this Mac (`local.json`) until accounts exist; the pencil next to your name on home changes it. `--name` skips the question.

Signing in comes with accounts (Stage 3/4): one screen for sign-in and sign-up (email with a magic link and a 6-digit code, GitHub, Google, Apple later), passkeys offered after the first sign-in and suggested by macOS in the email field, and a guest screen (just a name) that doubles as the pre-join screen for call links. See the prototype.

## Home

One window (960 × 640), two panes:

```
┌──────────────┬───────────────────────────────────────────────┐
│ ●●●  ⚡Quick │ Team standup ✎                  🔗  🗑  [Call] │
│ [Rooms|History] +  (A)(M)(L) Anna, Max and you  ‹ Today 10:02 · 32 min › │
│ (A+3) Team standup │ Transcript   Recordings 0   Files 0    │
│       Today 10:02 · 32 min │ ┌ Transcripts are on their way… ┐ │
│ (M+1) Family │ ── Call started · 10:02 ──                     │
│ …            │ ── Anna joined · 10:02 ──                      │
│ (N) Nikita ✎ │ ── Call ended · 10:34 · 32 min ──              │
└──────────────┴───────────────────────────────────────────────┘
```

- **Rooms**: one avatar (whoever you've talked to most there) with "+N", the name, and the last call's day, time and length. Clicking a row shows its latest call; the phone that appears on hover joins.
- **History**: every call by day (Today, Yesterday, weekday, then the date), quick calls included.
- **The selected call**: title (rename with the pencil), copy link, remove (press twice), **Call**. Below it, who was there and ‹ › through the room's earlier calls. A quick call offers **Keep as room** and **Call again** instead.
- **Tabs**: *Transcript* shows the call's timeline (who joined, left and shared, with times) until speech-to-text exists, then transcript lines join it as a chat. *Recordings* and *Files* show empty states until those features exist.
- **Quick call** is top left; **+** makes a room and puts its name in edit mode.
- When a call ends, home opens on it.

## Call: minimal mode (default)

```
              ╭──────────────────────────────╮
              │ [Anna ▣] [Lee ▣] [You ▣]     │   ← camera on: video tiles above the row
              │ (M) (T)  ((S)))+3 │ 🖥 🎙 ⏻  │   ← the row: recent speakers, the stack, three buttons
              ╰──────────────────────────────╯
                 Camera · Copy link · Call window   ← a row that opens on hover
```

One small glass widget floats above all apps and Spaces and never takes focus. Prototype: [prototype/call-widget.html](prototype/call-widget.html).

- **The row** shows up to three people who were active last (spoke, unmuted or joined). When someone new speaks and the row is full, they take the place of whoever has been quiet longest; the others stay where they are. **You** are always last in the row (a red badge while muted) and don't count toward the three, so muting moves nothing.
- **The stack**, a grey pill with one face and "+N", holds everyone else muted or quiet for two minutes. It keeps one width whatever it holds.
- **Cameras**: people in the row with their camera on (you included) show as video tiles above the row, named on the tile. Faces in the row are avatars.
- **Three buttons, always**: Share, Mic, Leave. Hovering them opens a row underneath with Camera, Copy link and Call window.
- **Hovering the stack** opens a row with everyone in it (name, mic state). **Hovering a face** for a moment shows the name and what they're doing ("speaking", "active 40 s ago").
- **While you share**, the widget grows upward with your screen's live preview on top, and a Stop button.
- **Someone sharing** has a screen badge on their face or tile; click it to bring their share window to the front. Anyone sharing stays in the row.
- **The main row never moves**: the widget grows around it, up for tiles and your preview, down for the hover rows, and sideways from its left end, so Share, Mic and Leave stay under the cursor. Near the bottom of the screen the hover rows open above it instead. A row closes once the cursor has been outside the widget for a quarter of a second.
- "Joining…", permission prompts and errors show in a slim row under the main one.
- **Drag** the widget by any empty spot.
- Someone else's **screen share** floats as its own rounded rectangle you can move and resize. **Double-click** it, or press ⤢ on it, to watch it **fullscreen** (macOS fullscreen in its own Space). Esc, a double-click or ⤡ brings it back.
- **Screen share** opens a picker with live thumbnails of your screens and windows and the frame-rate presets (5 / 15 fps for text, 30 / 60 fps for motion). Clicking share again stops sharing.

## Call: regular mode

One window (1040 × 680): the room's name and the call's time in the title bar, with "Invite" (copies the link). Tiles sit on a dark stage, a control bar floats under them, and a side panel opens on the right.

- **The layout picks itself**: alone, a card with your face and the call link; 2–4 people, a grid; more, whoever is speaking big with a strip of the others (as many as fit, then "+N"); anyone sharing, the share big and faces in a column. Your own share gets the big tile when nobody else shares.
- **Controls**: mic, camera, share | side panel, minimal | Leave.
- **Side panel**: *Transcript* (the live timeline, and transcripts later) and *People* (the call link and everyone, with their mic state).
- Double-click someone's share for fullscreen. Closing the window doesn't end the call; it goes back to the widget. Leaving is the red button.

## Decisions so far (easy to change)

- Dark only, with today's blue accent. Colours are named by use in `theme.rs`, so a light set can follow the system later.
- Avatar colours come from the name, not the call identity, so people keep their colour from call to call.
- One widget instead of a window per face. Video in it loses the compositor's free circle clip; see below.
- Recording, files and the device menus (mic/speaker choice, noise filter) wait for their backends; the prototype shows where they go.
- Rooms and history are stored on this Mac until the backend exists; then they move to the server so everyone in a room sees it.
- Your own camera isn't mirrored yet (GPUI can't flip video).
- The share picker needs macOS's Screen Recording permission once.
- Shares are always H.264, for both text and motion (VP9 cost too much; see FOOTPRINT.md).

## How it's built

- The widget and each floating share are borderless, non-activating panels (GPUI `WindowKind::PopUp`): above other apps, on all Spaces, never stealing focus. The widget's window resizes around its main row (`mac::Anchor::Row`), and macOS rounds its corners. It repaints at most 30 times a second.
- Glass is the system blur behind the window (GPUI's `Blurred` background, an `NSVisualEffectView`) under a faint dark tint.
- GPUI clips video only to rectangles, so rounded tiles and previews are drawn in horizontal bands, each clipped to the shape's width, under an opaque ring that hides the steps (`rounded_video` in `ui.rs`): about 10 draws for a widget tile. Patching GPUI's surface shader for rounded corners would make it one draw, but that means vendoring GPUI itself.
- The text field is GPUI's `input.rs` example, adapted (`text_input.rs`).
- No widget library: the look is custom, and every dependency costs footprint. Shared pieces (avatars, buttons, tooltips, empty states) are in `ui.rs`.
- Icons: [Lucide](https://lucide.dev) (ISC licence), embedded as SVG.
