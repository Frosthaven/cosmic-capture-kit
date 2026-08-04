---
title: Audio & Video
---

# Settings: Audio & Video

Three tabs: **Audio**, **Video** and **Mixing**. The page opens on Audio.

!!! note
    The Mixing tab's settings do not turn up in the settings search. Open the
    tab directly to find them.

## Audio

### Output

*Not shown on macOS.*

**Output device**
:   Which speakers count as "system audio" when you record. The first entry hands
    the choice to your system's own default.
:   **The system default** by default.
:   On Linux, choosing a specific device needs `pactl`. Without it the row goes
    amber and you get the system default only.

### Input

The rows here are listed in the order your voice actually passes through them,
which is worth knowing if you are chasing a sound problem.

**Input device**
:   Which microphone to record. First entry follows your system default, which is
    the default here too.

**Echo cancellation**
:   *Cancels speaker sound picked up by the mic.*
:   **On by default.** This is what stops the thing you are recording being heard
    again through your own microphone.

**Noise Suppression**
:   *Reduces background noise.*
:   **On by default.** Fans, air conditioning, traffic.

**Automatic Input Sensitivity**
:   *Controls how much sound your microphone records.*
:   **On by default.** Decides for itself where the line between your voice and
    the room is.

**Input Sensitivity Threshold**
:   A live meter with a threshold you set by hand. Anything quieter than the line
    is treated as room noise.
:   Only shown when Automatic Input Sensitivity is off.

**Automatic Gain Control**
:   *Lifts quiet speech into the ideal range and holds it there, without crossing
    into too-loud.*
:   **On by default.** This is the one that means you do not have to lean into
    your microphone.

**Advanced Voice Activity**
:   *Smarter detection of when you're speaking.*
:   **On by default.**

**Push to talk**
:   *Hold the mic button to talk instead of pressing to toggle.*
:   **Off by default.** With it on, your microphone is live only while you hold
    the microphone key. This row is hidden on systems that cannot support it.

**Microphone test**
:   A **Test Microphone** button. It opens a window that says "Speak normally. All
    active input filters are applied for this session", and draws your voice as a
    live waveform. That last part matters: you are hearing the same processing a
    real recording gets, not a raw feed.

The waveform is banded so you can see where you land:

| Band | Meaning |
|---|---|
| Ideal Peaks | Where you want your loudest moments to sit |
| Normal | Comfortable speaking level |
| Too Loud | Back off, or move away from the microphone |
| Filtered Out | Quiet enough that the app is treating it as room noise |

## Video

### Video

**Frame rate**
:   Frames per second, from 1 to 240. **30** by default.

**Max bitrate**
:   Roughly how much data per second the video is allowed, in Kbps. Higher looks
    better and makes bigger files. From 100 to 500000. **8000** by default.

**Max resolution**
:   A ceiling on the recorded size. Anything bigger is scaled down to fit; nothing
    is ever scaled up. The choices are Original, 360p, 480p, 720p, 1080p, 2K, 4K
    and Custom.
:   **2K (2560x1440)** by default.

**Max width** and **Max height**
:   Your own ceiling in pixels, from 2 to 8192. **1920** and **1080** by default.
    Only shown when Max resolution is set to Custom.

**Encoder**
:   Which piece of hardware turns your screen into a video file. The list is what
    your machine actually has, named after the chip, and the app picks the best one
    for you on the first launch. A note appears if you have no hardware encoder at
    all, in which case recording falls back to your processor and works fine, just
    with more effort.

**Encoder quality preset**
:   How hard the encoder works per frame. The choices depend on which encoder is
    active, and every list runs from fastest to best quality with a sensible middle
    marked as the default. If your encoder does not offer a choice, the row shows
    "Driver default" and does nothing.

**Video codec**
:   **Auto (by resolution)**, **H.264 (max compatibility)** or **HEVC (smaller
    files)**. **Auto** by default, which is the right answer unless you have a
    reason.
:   An amber note appears if your codec and resolution choices would force a
    downscale, so it never happens behind your back.

### Experimental

**GPU zero-copy capture**
:   *Performance setting to preprocess frames on the GPU instead of the CPU when
    available.*
:   **Off by default.** Only offered when your setup can actually do it.

**Benchmark monitor** and **Benchmark encoders**
:   *Encoders that appear in green can sustain your currently configured frame
    rate. Encoders that use fewer cores will leave more processing for other
    programs.*
:   Pick a monitor, press **Run benchmark**, and the app tests each encoder on
    that screen's real resolution. Results list the frame rate each managed and
    roughly how many processor cores it used. Green means it kept up with the
    frame rate you have configured. Neither of these is a saved setting.

## Mixing

**Pause other media during preview editor**
:   *When you capture content that contains audio, this will attempt to pause
    other audio while editing. Paused audio will resume after editing is
    completed.*
:   **On by default.**

**Automatically duck system audio**
:   *Automatically reduces recorded system volume when speaking.*
:   **On by default.** Keeps your narration on top of whatever the screen is
    playing.

**Automatically sync audio with video**
:   **On by default.** Measures and corrects the delay between what you hear and
    what you see, per recording. Leave this on.

**Audio sync offset**
:   *+ms delays audio (if sound is ahead of video), -ms advances it.*
:   A manual correction from -1000 to 1000 milliseconds. **0** by default, and
    only shown when automatic sync is off.
