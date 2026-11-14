# Pair with one scan: plan (PC + Android)

Status: plan, nothing built yet. Written 2026-11-13.
PC repo: `Pocket-Deck-PC` (Rust/Slint). Phone repo: `Android-Game-Launcher` (Kotlin, vendored Moonlight).

## Goal

Replace "phone shows a PIN, you type it into Sunshine's web page" with "PC shows a QR code, phone scans it, done".
Stock Sunshine, no fork, no new ports. LAN only for v1; the remote (iroh) field is reserved for play-from-anywhere.

## How it works

Moonlight pairing always needs a 4-digit PIN that the *client* picks and someone enters into Sunshine
(`POST /api/pin`). The PC app already holds a Sunshine web-API login, so it can enter the PIN itself.
So the PC picks the PIN, puts it in the QR code, and submits it to Sunshine when the phone's pairing request arrives.

```
PC (PocketDeck)                         Phone (PocketDeck)                 Sunshine
1. pick PIN, show QR  ───── scan ─────▶ 2. parse QR, probe addresses
                                        3. Moonlight pair(PIN) ───────────▶ waits for PIN
4. poll POST /api/pin {pin,name} ─────────────────────────────────────────▶ accepts, pairing completes
                                        5. check cert fingerprint, save host, sync games
6. see new client in /api/clients/list → "Paired" + sound, hide QR
```

Security is the same as today: only someone who can see the PC screen gets the PIN, and Sunshine still checks the
PIN cryptographically, so another client that starts pairing during the window with a different PIN fails.
The fingerprint in the QR also stops a LAN man-in-the-middle, which today's typed-PIN flow doesn't.

## The contract (both sides must match exactly)

The QR code holds one URI (also a deep link, so the system camera app works too):

```
pocketdeck://pair?v=1&n=<pc name>&a=<addr>[,<addr>...]&p=<http port>&pin=<4 digits>&fp=<cert sha256>
```

| Key   | Meaning |
|-------|---------|
| `v`   | Format version, `1`. The phone rejects any other value with "Update PocketDeck on your phone". |
| `n`   | PC display name (URL-encoded), e.g. Sunshine's `sunshine_name`, else the Windows computer name. |
| `a`   | Comma-separated LAN IPv4 addresses, best first (skip loopback, link-local, virtual adapters). |
| `p`   | Sunshine's base HTTP port (default `47989`). HTTPS is derived the usual way (`p - 5`). |
| `pin` | 4 digits, new random value every time the QR is shown. |
| `fp`  | SHA-256 of Sunshine's server certificate (DER), lowercase hex, no separators. |
| `r`   | *Reserved, not in v1:* iroh node id for remote play. The phone must ignore unknown keys. |

Rules:
- The PC enforces expiry (it stops entering the PIN after 2 minutes or after one success). The phone does not check time.
- One QR code pairs one phone. To pair another, the PC shows a fresh code.
- Name sent to Sunshine in `/api/pin`: `"PocketDeck phone"` in v1 (the PC doesn't know the phone's name; see open questions).

## PC side (this repo): built 2026-11-13

Sunshine 2026 (checked on 2026.914) changed PIN entry: `GET /api/pin` lists waiting devices as
`{"pairings":[{"id":"<32 hex>","address":"<phone IP>","name":"roth"}]}` (Moonlight clients always send `roth`),
`POST /api/pin {pairing_id, pin, name}` answers `{"status":true}`, and `DELETE /api/pin {pairing_id}` turns one down.
Requests stay listed after the client gives up, so the PC only answers a request that **appeared after the code went up**, and of those the **newest** (Sunshine's list has no order, so the PC remembers when it first saw each one). Older requests still waiting from the same address (an earlier attempt, or one from before the code) are turned down (`DELETE /api/pin`) so the new one goes first. Requests from other addresses are left alone. A retry from the same phone (same address) after the PIN went in also gets the PIN; requests from any other device are then ignored.
Older Sunshine (no `GET /api/pin`) falls back to polling `POST /api/pin {pin, name}`.

1. `crates/sunshine/src/api.rs`: `pending_pairings`, `submit_pin`, `submit_pin_legacy`, `cancel_pairing`, `clients`, `unpair_client`, `random_pin`.
2. `crates/sunshine/src/cert.rs`: `fingerprint(https_port)` reads the cert off a TLS handshake with Sunshine (same cert on the web UI port).
3. `crates/core/src/pairing.rs`: LAN addresses (default-route address first, then adapters with a gateway), the URI, which request is ours, device naming.
4. `app/src/pairing.rs`: the session thread (polls every 500 ms, code lives 2 min, waits up to 20 s for the phone to finish after the PIN goes in).
5. UI: `app/ui/pages/devices.slint`: "Pair a phone", the QR panel with countdown and every state, paired phones with two-step Unpair.
   The Home card "Pair with one scan" now opens it. Not done: an onboarding step for pairing.

## Phone side (Android-Game-Launcher)

1. Scanner: CameraX + ZXing core (pure Java, no Play Services, GPL-friendly). Request camera permission only when the scanner opens.
   Alternatively the ML Kit code scanner if Play Services is acceptable.
2. Entry points: Settings → PC streaming → "Scan PC's code" (primary action, above "Add by address"); the onboarding streaming step;
   and an intent filter for `pocketdeck://pair` so scanning with the system camera opens the app on the same flow.
3. Parse and validate the URI (strict on `v`, `pin` = 4 digits, `fp` = 64 hex; ignore unknown keys).
4. `HostRepository`: add `pairFromScan(qr)`:
   - probe each address in `a` with `/serverinfo` on port `p`, take the first that answers;
     none answers → "Can't reach <n>. Make sure the phone is on the same Wi-Fi as the PC."
   - upsert the host as a manual address (reuse `upsert(..., manual = true)`).
   - call the existing `pair()` path with the PIN from the QR instead of `PairingManager.generatePinString()`
     (add `pin: String? = null` to `pair`); show "Pairing with <n>…" instead of the PIN overlay.
   - after `PAIRED`, compare SHA-256 of `pm.pairedCert.encoded` with `fp`; mismatch → unpair, delete the cert, fail with
     "This code doesn't match the PC that answered."
   - then the usual `refresh` + `syncApps`.
5. Errors to map: `PIN_WRONG` → "The code expired. Show a new one on the PC." (most likely cause is a stale QR);
   `ALREADY_IN_PROGRESS` → keep the existing message.
6. Keep the typed-PIN flow as the fallback ("Pair with a PIN instead").

## Order of work

1. Both: agree on the URI above (this file is the source of truth; change it here first).
2. PC: API calls + fingerprint + URI builder, testable with an `examples/` binary that prints the URI and polls.
3. Phone: parser + `pairFromScan` tested by pasting the URI (debug menu), before any camera work.
4. Phone: scanner + deep link. PC: QR panel + Devices list.
5. Together: end-to-end on real hardware, then wrong-network, stale-QR and second-phone cases.

## Open questions

- Phone name in Sunshine: v1 sends `"PocketDeck phone"`. Later the phone could put its name in the pairing request,
  or the PC could rename the client via the API if the pinned Sunshine supports it.
- Two phones scanning the same code at once: the PC answers the newest request at the time; once the PIN has gone in, the other phone's request is ignored and times out.

## Notes for the phone side

- Nothing in the URI changes. The phone just runs Moonlight's normal `pair()` with the PIN from the code.
- The phone's pairing request must reach Sunshine within the 2-minute window. The PIN goes in about half a second after
  the request shows up, so `pair()` doesn't need a longer timeout than today.
- If `pair()` comes back `PIN_WRONG`, the most likely cause is that the PC took the code down or showed a new one: say
  "The code changed or expired. Scan the one on the PC now."
