---
title: Editing a recording
---

# Editing a recording

Open a recording and the editor gives you a timeline instead of annotation
tools. It is a small editor with one job: cut the boring parts out.

## The timeline

Under a ruler there are three lanes:

1. The **video track**.
2. The soundtrack's **left** channel, drawn as a waveform.
3. The soundtrack's **right** channel.

**The ruler is the seek bar.** Click or drag anywhere on it to move through the
recording, whichever tool you have armed. Clicking the lanes below does not
move the playhead, because those clicks belong to cutting and selecting.

The playhead is a red vertical line with a round head at the top.

Times are shown as hours, minutes, seconds and frames, and the readout gives you
the position and the total length. The length shown is the length of your
**edited** recording, so it goes down as you delete things.

## Playing

| Control | Key |
|---|---|
| Play and pause | P |
| Previous frame | , |
| Next frame | . |

There is a play button on the toolbar. Frame stepping is keys only, with no
button.

Deleted stretches are skipped during playback, so what you hear and see while
playing is what you would get if you exported.

## Cutting

There is a two-way toggle on the toolbar for what a click on the lanes does:

- **Pointer**: "click to seek, click a segment to select".
- **Scissor**: "click the timeline to cut it".

Switch to Scissor and a thin red line follows your pointer, showing exactly
where the cut will land, and the pointer becomes a crosshair. Click to cut.

The cut **snaps to the playhead** when you get near it, which is how you cut
precisely at a spot you found by playing. Cuts that would leave a sliver too
short to be useful are refused rather than made.

**A cut on its own changes nothing about your recording.** It just marks a
boundary. Nothing is re-encoded, and the app does not consider the recording
edited yet. Cutting is safe and free.

## Deleting

Switch back to Pointer and select the piece you want gone.

| Gesture | What it does |
|---|---|
| Click | Selects a segment |
| Ctrl+click | Adds or removes a segment |
| Shift+click | Selects everything between |
| Drag | Sweeps up everything in the box |
| Click empty space | Deselects |

Then press **Delete**, or the delete button on the toolbar, whose tooltip reads
"Delete selected segments". You can also right-click.

Everything after a deleted piece **slides left to close the gap**. That is not
an option you can turn off; positions are worked out from what is left, so a gap
cannot exist. Deleting several selected segments is one undo step.

You cannot delete the last remaining segment. There has to be a recording left.

## The right-click menu

| Item | When it appears |
|---|---|
| Cut here | Always |
| Delete segment | Only when there is more than one segment |

## Undo

Cuts and deletions go into
[the same undo history](index.md#undo-covers-everything) as everything else, in
the order you did them. Ctrl+Z, or Cmd+Z, walks back through them.

## What is not here

This is a trimming tool, not a video editor. There are no transitions or
crossfades, no volume or mute controls, no loop, and no playback speed. The
audio lanes are there to show you where the sound is so you can cut around it.
