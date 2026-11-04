# Quipcall UI

Less is more. Outside a call the app is a small list. During a call there is no window at all by default: just faces floating over whatever you're working on.

## Home

A small window (about 380 × 560):

```
┌──────────────────────────────┐
│ Quipcall               ⚡ Quick │   ← one-time room, starts a call right away
│                              │
│ Rooms                      + │
│  ● Team standup         2 in │   ← persistent rooms (● = call in progress)
│  ○ Design                    │
│  ○ Family                    │
│                              │
│ Recent                       │
│  Team standup   today 10:02  32 min · Anna, Max
│  Quick call     yesterday    5 min · Lee
└──────────────────────────────┘
```

- **Rooms** are persistent (saved, reusable link) or one-time (Quick call: a fresh room that only shows up in history).
- **Recent** is the call history.
- Clicking a room joins it. The home window hides and the call starts in minimal mode. Leaving brings it back.

## Call: minimal mode (default)

```
                                   your other apps…
                                                           ╭────╮
                                                           │ 🙂 │  Anna
                                                           ╰────╯
                                                        ╭────────╮
                                                        │  you   │
                                                        ╰────────╯
              ╭─────────────────────────────────────╮
              │ 🎙  📷  🖥  🔗  ＋  ⤢  ⏻            │   ← glass toolbar
              ╰─────────────────────────────────────╯
```

- Every participant, you included, is a **floating circle** above all apps and on every Space. Circles never take focus away from what you're doing.
- A circle shows the person's camera, or their avatar (initials on a colour picked from their name) when the camera is off.
- **Move** a circle by dragging it. **Resize** by scrolling or pinching over it, or drag the knob with arrows that sits on its rim while you hover, from 120 to 480 px. Your circle starts at 200 px, everyone else's at 160 px.
- A soft ring shows who is speaking; a small badge shows a muted mic.
- Someone else's **screen share** floats as a rounded rectangle you can move and resize the same way. **Double-click** it, or press ⤢ on it, to watch it **fullscreen**: macOS fullscreen in its own Space, with the circles and toolbar still on top. Esc, a double-click or ⤡ brings the floating window back where it was.
- While **you share**, your own circle turns into a live **preview** of what you share: a rounded rectangle in the shape of the screen or window. With your camera on, a smaller circle with it sits on the preview's left. Stop sharing and it's a circle again.
- **Several people can share at once**: each share gets its own floating window (one share per person).
- The **glass toolbar** has: mic · camera · screen share · copy call link · invite · expand to regular mode · leave. Drag it by its empty space.
- **Screen share** opens a picker window with live thumbnails of your screens and windows and the frame-rate presets (5 / 15 fps for text, 30 / 60 fps for motion). Double-click a thumbnail or press Enter to share. Clicking the share button again stops sharing.

## Call: regular mode

One ordinary window with video tiles and the same toolbar at the bottom. The collapse button returns to circles. What you share gets a tile of its own; double-click someone else's share for fullscreen.

## Decisions so far (easy to change)

- Your circle starts at 200 px, others at 160 px (so ten people still fit along the screen edge).
- Other people's circles show their video when their camera is on (avatars otherwise).
- Screen shares are rounded rectangles, not circles.
- Your own share shows as a preview in place of your circle, repainted at up to 15 fps. Your camera goes beside it, not on it: only the window mask can make video round, and it can't cut a circle out of the middle of the preview.
- Fullscreen is the native kind (its own Space), so you can still swipe back to your apps while watching.
- *Invite* copies the call link until accounts and contacts exist; then it lets you pick people to ring.
- Rooms and history are stored on this Mac until the backend exists; then they move to the server so everyone in a room sees it.
- New rooms are named "Room 1", "Room 2", … until there's a text field to rename them.
- Closing the regular window doesn't end the call; it goes back to circles. Leaving is the red button.
- Your own circle isn't mirrored yet (GPUI can't flip video; needs its own fix).
- The share picker needs macOS's Screen Recording permission once (the system picker didn't, but it has no previews).
- Shares are always H.264, for both text and motion (VP9 cost too much; see FOOTPRINT.md).

## How it's built

- Each circle and the toolbar is its own borderless, transparent, non-activating panel (GPUI `WindowKind::PopUp`): above other apps, on all Spaces, never steals focus.
- macOS clips each window's layer to a circle (corner radius = half the size), so video is clipped by the compositor with no extra CPU or GPU work on our side.
- Glass is the system blur behind the window (GPUI's `Blurred` background, an `NSVisualEffectView`).
- No widget library (we dropped the gpui-component plan): the look is custom, and every dependency costs footprint.
- Icons: [Lucide](https://lucide.dev) (ISC licence), embedded as SVG.
