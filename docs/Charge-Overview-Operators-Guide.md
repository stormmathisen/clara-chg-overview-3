# Charge Overview — Operator's Guide

Live charge readback and front-end control for the CLARA charge diagnostics.

**Where:** <http://charge.dsastvx10.dl.ac.uk>
No login, no install — open it in a browser. Multiple people can have it open at
once; everyone sees the same data and each other's commands.

---

## Contents

- [The window at a glance](#the-window-at-a-glance)
- [The devices](#the-devices)
- [Reading a strip chart](#reading-a-strip-chart)
- [Per-device controls](#per-device-controls)
- [Global controls](#global-controls)
- [Notifications](#notifications)
- [Common tasks](#common-tasks)
- [Troubleshooting](#troubleshooting)

---

## The window at a glance

<!-- SCREENSHOT 1 — full window, browser at ~1600x900, several devices visible with
     live data. Caption below. -->
![Charge Overview main window](images/overview-main.png)

Four regions:

| Region | What lives there |
|---|---|
| **Top bar** | Title, connection dot, and the global controls (two rows) |
| **Left panel** | Filters, then one control group per device |
| **Centre** | One strip chart per visible device, in the left panel's order |
| **Bottom bar** | The latest notification, with an arrow to expand the history |

The green dot next to the title is **your browser's connection to the app**, not
the beamline. Red "Disconnected" means the page has lost the server — the charts
freeze. It reconnects on its own; reload if it doesn't.

---

## The devices

Three kinds of charge diagnostic feed this app, plus a derived one.

### WCM — Wall Current Monitor

`CLA-S01-DIA-WCM-01`. Non-intercepting: reads the beam charge without stopping
the beam, so it can run during normal operation. Two sensitivity levels, **FB3**
and **FB4**.

### DQ — Dark Charge

`CLA-S01-DIA-WCM-01:DQ`. Not a separate box — it is the *same* WCM hardware read
over a different, much wider sample window. It **integrates the current over that
window to calculate the dark charge**: the charge present outside the main bunch.
It shares the WCM's physical front end and its sensitivity levels.

> **Coupled to the WCM.** Setting the WCM's sensitivity automatically rewrites
> the DQ calibration to match. You never set DQ's sensitivity separately — it
> has no controls of its own beyond Restore Defaults and Clear.

### FCUP — Faraday Cup

`CLA-SP1/SP2/SP3/S07/FED-DIA-FCUP-01`. Intercepting: the cup is in the beam and
absorbs it, so anything downstream sees nothing while you are reading one. Five
sensitivity levels, **FB0** (most sensitive, for small charge) through **FB4**
(least sensitive, for large charge).

### ICT — Integrating Current Transformer

`CLA-S04/S05/FEA/FEH-DIA-ICT-01`. Non-intercepting, and **read-only in this
app** — ICTs have no front-end box, so they have no sensitivity buttons, no Zero,
and no Sweep Timing. You get the chart, the stats, and Restore Defaults (which
writes their hold-delay PV).

### Full list

| Device | Type | Sensitivities | Front-end box |
|---|---|---|---|
| `CLA-S01-DIA-WCM-01` | WCM | FB3, FB4 | 192.168.114.14 |
| `CLA-S01-DIA-WCM-01:DQ` | DQ | (follows the WCM) | shares .14 |
| `CLA-SP1-DIA-FCUP-01` | FCUP | FB0–FB4 | 192.168.114.10 |
| `CLA-SP2-DIA-FCUP-01` | FCUP | FB0–FB4 | 192.168.114.11 |
| `CLA-SP3-DIA-FCUP-01` | FCUP | FB0–FB4 | 192.168.114.12 |
| `CLA-S07-DIA-FCUP-01` | FCUP | FB0–FB4 | 192.168.114.13 |
| `CLA-FED-DIA-FCUP-01` | FCUP | FB0–FB4 | 192.168.114.9 |
| `CLA-S04-DIA-ICT-01` | ICT | — | none |
| `CLA-S05-DIA-ICT-01` | ICT | — | none |
| `CLA-FEA-DIA-ICT-01` | ICT | — | none |
| `CLA-FEH-DIA-ICT-01` | ICT | — | none |

---

## Reading a strip chart

<!-- SCREENSHOT 2 — one strip chart, cropped tight: name, stats line, plot with
     axes. Pick a device with real beam data if possible. -->
![A single strip chart](images/strip-chart.png)

Each chart is one device's charge against time:

- **Y axis** — charge in **pC**.
- **X axis** — wall-clock time, `HH:MM:SS`. Newest data is on the right.
- **Stats line** — Mean, Min, Max and **RMSD** (root-mean-square deviation — the
  shot-to-shot spread) over everything currently in the buffer.

The charts are display-only: you cannot drag, zoom or scroll inside them. Change
what you see with the **Buffer** and **Y Axis** controls in the top bar instead.

The stats are computed over the **rolling buffer**, so they follow the Buffer
setting. A buffer of 1000 points at 10 Hz is roughly the last 100 seconds — halve
the buffer and the mean responds twice as fast to a change.

Two yellow warnings can appear next to the stats line:

> **⚠ SATURATING** — the rolling average is past the saturation limit for the
> device's current sensitivity. The sensor is clipping and the numbers can no
> longer be trusted: move to a higher FB level, or tick **Auto gain** in the top
> bar and let the app do it.

> **⚠ CHECK TIMING** — the app checks (at startup, and whenever the sample
> window PVs change) that the configured peak window actually brackets the pulse
> in the digitizer trace — the negative-going dip for an FCUP, the positive peak
> for the WCM. The check only judges a trace with a clear pulse in it (with beam
> off there is only noise, so it waits and retries). If the window misses the
> pulse, charge is being integrated in the wrong place: open the device's trace
> in Phoebus and confirm the peak falls between the peak window lines, then run
> **Sweep Timing** if it doesn't.

---

## Per-device controls

Each device has a group in the left panel.

<!-- SCREENSHOT 3 — one FCUP device group from the left panel, cropped: status
     dots, name, Last time, Up/Dn, the FB0–FB4 row, and the button row. An FCUP
     shows the most controls, so use one of those. -->
![Device control group](images/device-controls.png)

### Status dots

The two dots watch **two different pieces of hardware in two different places**,
and knowing which is which is most of the diagnosis:

| Dot | Reaches | Green | Red |
|---|---|---|---|
| **E** | **EPICS** — the **digitizers in the rack rooms**, over Channel Access | Charge data arriving | No data for over 60 s |
| **FE** | **The front-end box in the bunker**, on its own network | Box answers | Box unreachable |

**E** is the read path: the digitizer sees the pulse, its IOC publishes the PV,
this app subscribes. **FE** is the control path: the box in the bunker is what
holds the gain, and it is what the FB buttons write to.

ICTs show only **E** — they have no box in the bunker. Hover either dot for the
same explanation. `Last: HH:MM:SS` next to the name is when data last arrived.

- **E red, FE green** — the bunker box is fine; the problem is the digitizer, its
  IOC, or the CA route from the rack room.
- **E green, FE red** — data is still flowing from the rack room, but you cannot
  change the gain: the bunker box is off, or off the network.
- **Both red** — the device or its whole signal path is down.

### Sensitivity (FB buttons)

Click a level to apply it. This does two things at once: pushes the gain to the
front-end box **and** writes the matching calibration factors over EPICS. That
pairing is the whole point — changing gain anywhere else (the box's own web page,
the front panel) moves the gain but *not* the calibration, and the readings go
quietly wrong.

Lower FB = more sensitive. Use a low FB for small charge, go up to a higher FB if
the signal saturates — the chart shows a yellow **⚠ SATURATING** when the rolling
average is past the limit for the current level.

> **The orange warning.** If someone changes a sensitivity outside this app, the
> row turns orange and the active level goes solid orange:
>
> <!-- SCREENSHOT 4 — a device group showing the orange calibration-mismatch
>      band. To reproduce: change a box's integrator from its own web UI, or ask
>      a controls engineer to POST to it; the band appears within a second. If
>      you cannot reproduce it safely, omit this screenshot. -->
> ![External sensitivity change warning](images/calibration-mismatch.png)
>
> It means: the gain moved, but the calibration factors may be stale, so the pC
> numbers may be wrong. **Click the already-selected level** to re-apply it — that
> rewrites the calibration and clears the warning. Do this before you trust the
> readings.

### Zero WCM (WCM only)

Removes the WCM's baseline offset. It zeroes the offset, averages 100 fresh
readings (~10 s), and writes that mean back as the new offset.

> **Beam must be OFF. RF must be ON.** The point is to measure what the monitor
> reads with no beam but the RF environment present. Any beam during those ten
> seconds gets baked into the offset and every later reading is wrong by that
> amount.

### Sweep Timing (WCM and FCUP)

Finds where the charge pulse actually sits in the digitizer trace and re-centres
the sample windows on it. Averages 100 waveforms (~10 s), then writes the window
PVs.

> **Beam must be ON the device**, and for an FCUP that means the cup is in and
> taking beam. No pulse in the trace, no meaningful peak.

Use it after timing changes, or when a device reads low/noisy for no apparent
reason. Not offered for DQ (its window is deliberately wide) or ICT (no
digitizer window).

### Restore Defaults

Writes every configured default PV for that device — sample windows, calibration
factors for the current sensitivity, ICT hold delay. The safe "put it back how it
was" button. Hover it to see exactly which PVs and values it will write before
you click.

Best-effort: a PV that won't take is reported in the notifications and the rest
still go.

### Clear

Empties that device's rolling buffer — chart and stats start fresh. Display only;
touches no hardware.

---

## Global controls

<!-- SCREENSHOT 5 — the top bar, cropped: title + connection dot, the button row,
     and the Y Axis row with the dropdown open showing Auto / Zero-based / Manual. -->
![Global controls](images/global-controls.png)

### Clear Calibration (All)

Puts **every** front-end box back into normal operation: calibration mode off,
input back to EXT, at each device's currently selected sensitivity. Use it if a
box was left in calibration mode after an engineering session.

### Reset Front Ends

Reboots the PICs in all the front-end boxes by cutting their trigger for **65
seconds**, then restoring it and re-applying every device's sensitivity.

<!-- SCREENSHOT 6 — the top bar mid-reset, the button replaced by the progress
     bar reading "Resetting front ends... 42s". Click Reset Front Ends and grab
     it within the 65s window. Do this on the virtual/test instance, not during
     beam operation. -->
![Front-end reset in progress](images/reset-progress.png)

> **This cuts the trigger for over a minute.** Charge readings stop for the
> duration. Don't do it mid-measurement.

The button becomes its own countdown while it runs, so you cannot start a second
one — and anyone else's window shows the same countdown, including a window
opened halfway through. The sensitivity re-apply at the end is not optional
housekeeping: the boxes come back up defaulted to FB4, and without the resend
every reading would be silently wrong.

### Clear Data (All)

Empties every device's buffer. Display only.

### Buffer

How many points each rolling buffer holds (10–10000, default 1000). At 10 Hz,
1000 points ≈ 100 seconds of history. Type a number and click away or press Tab
to apply — a value that isn't a number snaps back.

This is **shared**: it changes the buffer for everyone, and resizing discards
points, so expect the charts to jump.

### Auto gain

When ticked, any WCM or FCUP whose recent rolling average (roughly the last
second of data) exceeds the saturation limit for its current level is switched
to the next less sensitive FB level automatically — exactly as if you had
clicked the button, calibration factors and all. Each switch announces itself
with an orange warning notification naming the device. A device already at its
least sensitive level is left alone and just keeps showing **⚠ SATURATING**.

Off by default. This is **shared** — it turns auto gain on for everyone — and
the server remembers it across restarts.

### Freeze Stats

Snapshots the Mean/Min/Max/RMSD as they stand so you can write them down while
the beam keeps moving. Frozen charts show **❄ FROZEN** next to the name; the
plot keeps updating, only the numbers hold. Click **Unfreeze Stats** to resume.

This is local to your browser — freezing does not freeze anyone else's numbers.

### Y Axis

| Mode | Behaviour |
|---|---|
| **Auto** | Fits the data. Default. |
| **Zero-based** | Forces zero into view — honest sense of scale, no exaggerated wobble |
| **Manual** | Fixed Min/Max you type. Best for comparing runs on identical axes |

In Manual, enter Min and Max and click away; Min must be below Max or the entry
is ignored. Applies to all charts at once, and is remembered in your browser.

---

## Notifications

<!-- SCREENSHOT 7 — the bottom bar with the history expanded, showing a few
     entries at different levels (a green success, a red error if you can get
     one). Click the ^ arrow at bottom-left. -->
![Notification history](images/notifications.png)

The bottom bar shows the latest message. The **^** arrow expands the history
(newest first, up to 200 entries, this session only). Each line is time, device,
message.

Colour = level: green success, blue/white info, orange warning, **red error**.

Errors **stay** in the bar until something newer replaces them; everything else
fades after 10 seconds but remains in the history. So if the bar is red, a
command failed — read it before carrying on.

You see everyone's notifications, not just your own. Someone else's Zero WCM will
appear in your bar.

---

## Common tasks

**Read charge on a Faraday cup**
Insert the cup. Confirm **E** and **FE** are green. Pick an FB level that doesn't
saturate — start low, go up if **⚠ SATURATING** appears (or tick **Auto gain**
and let the app step up for you). Let the buffer fill, then read the stats or hit
**Freeze Stats** to write them down.

**Zero the WCM**
Beam off, RF on. Click **Zero WCM**, wait ~10 s for the "Zeroed … offset = x"
success. If it says "timed out waiting for charge readings", the device isn't
delivering data — check the **E** dot.

**A device reads low or noisy**
Check the sensitivity row isn't orange (if it is, re-apply the level first). Get
beam onto the device and try **Sweep Timing**. Still wrong: **Restore Defaults**.

**Compare two devices on the same scale**
Y Axis → **Manual**, set Min and Max, and untick the types you don't need in
**Filters** so only the two charts remain.

**Tidy the display**
Untick a type in **Filters** to drop it entirely, or use **Individual Devices**
for one at a time. Reorder with the **Up**/**Dn** buttons or by dragging the ⣿
handle. Filters and order live in *your* browser only — nobody else's view moves,
and yours survives a reload.

---

## Troubleshooting

| Symptom | Meaning | Do this |
|---|---|---|
| Title dot red, "Disconnected" | Your browser lost the server | Wait; reload if it persists |
| **E** red on one device | No EPICS data for 60 s — digitizer in the rack room | Check the digitizer and its IOC |
| **FE** red | Front-end box in the bunker unreachable | Check box power and network |
| Sensitivity row orange | Gain changed outside this app; calibration may be stale | Click the selected level to re-apply |
| **⚠ SATURATING** on a chart | Rolling average past the limit for the current sensitivity | Pick a higher FB level, or tick **Auto gain** |
| **⚠ CHECK TIMING** on a chart | Sample window doesn't bracket the digitizer peak | Check in Phoebus that the peak sits between the window lines; if not, get beam on the device and run **Sweep Timing** |
| Chart flat at zero | No beam, or the device isn't reading | Check the **E** dot and `Last:` time |
| Red error in the bar | A command failed | Expand the history and read it |
| Stats frozen unexpectedly | Freeze Stats is on | Click **Unfreeze Stats** |
| Buttons do nothing | Disconnected, or the box is unreachable | Check the title dot and **FE** |

---

## Notes

- **Shared vs. local.** Buffer size, sensitivities, Auto gain and every hardware
  command are shared with everyone. Device order, filters, Y-axis and Freeze
  Stats are yours alone, kept in your browser.
- **Everything is logged.** Connections and commands go to an append-only audit
  log. If something moved and nobody remembers doing it, ask a controls engineer
  to check.

---

## Screenshot checklist

For whoever fills these in. Point a browser at
<http://charge.dsastvx10.dl.ac.uk>, put the PNGs in `docs/images/`.

| # | File | Shot |
|---|---|---|
| 1 | `overview-main.png` | Full window ~1600×900, several devices with live data |
| 2 | `strip-chart.png` | One chart cropped: name, stats line, both axes |
| 3 | `device-controls.png` | One FCUP group from the left panel (most controls) |
| 4 | `calibration-mismatch.png` | The orange band — needs an externally-set sensitivity; omit if not safely reproducible |
| 5 | `global-controls.png` | Top bar, Y Axis dropdown open |
| 6 | `reset-progress.png` | Mid-reset progress bar — **virtual/test instance only** |
| 7 | `notifications.png` | Bottom bar with history expanded, mixed levels |
