# Virtual display: match the phone's screen (PC + Android)

Status: built 2026-11-14. PC: `crates/display`. Phone: `data/stream/PcDisplay.kt` and `StreamActivity.prepareVirtualDisplay`.
This file is the contract between the two; change it here first.

## Why

Streaming the PC's own monitor gives the phone the wrong resolution and aspect ratio. A virtual monitor at the
phone's exact mode fixes that, but the usual setup (a virtual display driver that's always plugged in) causes
its own trouble: Windows boots onto the virtual monitor, and apps open on it while nobody is streaming.

So PocketDeck keeps the virtual display **switched off** (the device disabled) whenever no phone is streaming,
and switches it on, at the phone's mode, just before the stream starts.

## Pieces

| Piece | Where | Runs as |
|---|---|---|
| Driver: [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) 25.7.23, MIT, SignPath-signed. Pinned by SHA-256 in `vdd.rs`, installed unchanged. | Windows driver store, device `Root\MttVDD` | — |
| Service `PocketDeckDisplay` (`PocketDeckDisplay.exe --display service`): owns the device, hands out leases, serves the phone. | `%ProgramFiles%\PocketDeck\PocketDeckDisplay.exe` (a copy of `PocketDeck.exe`) | LocalSystem |
| Layout helper (`--display arrange …` / `--display restore`): makes the virtual display the main screen, with the PC's own screens beside it. | started by the service in the signed-in session | SYSTEM, console session |
| Setup / removal (`--display setup <zip>` / `--display remove`) | Settings → Streaming host → "Match your phone's screen" | elevated, one UAC prompt |

Setup records what it did under `HKLM\SOFTWARE\PocketDeck\Display` (`OwnDevice`, `WasEnabled`, `OwnSettings`,
`Version`), so removal undoes exactly that. If the PC already had this driver, setup reuses it, and removal puts
it back the way it was.

The driver reads its modes from `vdd_settings.xml` in `VDDPATH` (`HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver`).
Setup points that at `%ProgramFiles%\PocketDeck\vdd` unless the driver already has a settings folder. Before
switching on, the service adds the phone's mode to the file if it's missing. The driver only reads the file when it
starts, so a new mode means switching it off and on again.

Sunshine needs no configuration. With `output_name` empty (the default) it captures the primary display, and the
virtual display is made the primary one (and games opened from the phone go there too). A Sunshine set to a specific `output_name` keeps capturing that output.

## Lifecycle

```
phone                              PC service                                   Windows
prepare(w,h,fps) ───────────────▶  add mode to vdd_settings.xml (if new)
                                   enable device (restart if mode was new) ───▶ virtual monitor appears
                                   helper in user session: main display,
                                   mode w×h@fps ──────────────────────────────▶ becomes primary; PC screens stay on
◀── 200 ready (lease 90 s)
Moonlight launch/resume ────────────────────────────────────────────────────▶ Sunshine captures primary = virtual
keepalive every 20 s ───────────▶  lease renewed
release (stream ends) ──────────▶  disable device; helper: restore layout ────▶ usual layout and main screen back
```

The display is switched off when any of these happens:
- the phone releases it (stream ended, error screen, app closed);
- the lease runs out (90 s without a keep-alive: the phone crashed, lost Wi-Fi, was killed);
- the service starts (after a reboot or a crash), or stops (shutdown).

The device stays disabled across reboots, so Windows never starts on it.

## The endpoint (phone → PC)

HTTPS on **Sunshine's base port + 30** (default 47989 + 30 = **48019**), all interfaces, TCP. Setup adds an
inbound firewall rule for the service's program.

Trust is the same as Moonlight's: the PC presents **Sunshine's own server certificate** (the one the phone pinned at
pairing), and requires a **client certificate on Sunshine's paired list** (`sunshine_state.json`). No new pairing,
no new secrets. A device that isn't paired fails the TLS handshake.

All bodies are JSON. One request per connection (`Connection: close`).

| Request | Body | Answer |
|---|---|---|
| `POST /pocketdeck/v1/display/prepare` | `{"width": 2400, "height": 1080, "fps": 120}` | `200 ready` once the display is in place (usually 1 to 5 s, up to about 40 s). Starts or renews the lease. |
| `POST /pocketdeck/v1/display/keepalive` | `{}` | `200 on` (lease renewed) or `200 off` (no lease; the display is off) |
| `POST /pocketdeck/v1/display/release` | `{}` | `200 off` |
| `GET /pocketdeck/v1/display` | — | `200 on` / `200 off` / `503 unavailable` |

Answers:

```json
{"state":"ready","width":2400,"height":1080,"fps":120,"note":null,"leaseSeconds":90,"version":"0.1.0"}
{"state":"on","width":2400,"height":1080,"fps":120,"leaseSeconds":90,"version":"0.1.0"}
{"state":"off","version":"0.1.0"}
{"state":"unavailable","message":"The virtual display isn't set up on this PC.","version":"0.1.0"}   (HTTP 503)
{"state":"failed","message":"The virtual display didn't start in time.","version":"0.1.0"}          (HTTP 500)
```

- `note` on `ready`: the display is on but something didn't go to plan, such as nobody being signed in, so it
  couldn't be made the main screen. The stream still works, showing the PC's own main screen.
- Limits: mode 320–7680 × 240–4320 at 24–480 Hz, otherwise `failed`. Headers ≤ 8 KB, body ≤ 4 KB.

## Phone rules

1. Before Moonlight's launch/resume, call `prepare` with the stream's resolution and frame rate.
   Show "Turning on the virtual display…" only if no answer has come within 400 ms.
2. Connect timeout 2 s. Refused, timed out, `404` or `503`: the PC doesn't have the feature. Stream as usual
   and say nothing.
3. `failed`: stream anyway, with a notice that the PC's own screen will show.
4. While streaming, `keepalive` every 20 s. When the stream ends for any reason, `release` (fire and forget).

## Testing on the PC

```
PocketDeck.exe --display list                  # the adapter, its state, every display Windows sees
PocketDeck.exe --display on 2400 1080 120      # what the phone's prepare does (via the service's pipe)
PocketDeck.exe --display status
PocketDeck.exe --display off
```

The service logs to `%ProgramData%\PocketDeck\display.log`.

## Known limits

- One phone at a time: a second `prepare` re-arranges the display for the newer phone.
- While streaming, the PC's own monitors stay on, moved to the right of the virtual display, which is the main
  screen. A game that was already open on a PC monitor stays there, so the phone sees it only once it's moved or
  restarted. The usual layout (and main screen) come back when the display is released.
- If the phone cancels while the display is still turning on, the PC switches it off when the lease runs out
  (90 s), not straight away.
