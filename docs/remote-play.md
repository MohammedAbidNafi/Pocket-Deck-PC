# Play from anywhere (PC + Android)

Status: built 2026-11-18. PC: `crates/remote` (the tunnel), `crates/display/src/tunnel.rs` (the service's host).
Phone: `rust/pocketdeck-tunnel-jni` and `data/stream/RemoteTunnel.kt`. Relay: the `Pocket-Deck-Relay` repo.
This file is the contract between the two; change it here first.

## Rules

- **One path, everywhere.** Once a phone has registered, it always reaches the PC through the tunnel, at home
  and away. Moonlight on the phone always talks to `127.0.0.1`. At home, iroh finds the PC over mDNS and
  uses the LAN directly.
- **The relay is only for connecting.** Our relay (stock `iroh-relay`, the version pinned below) introduces
  the two ends and helps hole punching. Streams never go through it:
  - the phone doesn't open its local ports until the selected path is direct;
  - neither end sends datagrams while the selected path is the relay;
  - a connection that loses its direct path for 5 s (`GRACE`) is closed (`LostDirectPath` on the phone);
  - the relay has a ~128 KiB/s per-client rate limit, so it couldn't carry video anyway.
- **If there's no direct path, the phone says so** ("Can't reach your PC directly on this network") and doesn't
  stream.
- No n0 infrastructure: `RelayMode::Custom(our relay)` only, no n0 DNS or pkarr lookup.
- Stock Sunshine and Moonlight. Nothing is rewritten: the PC end dials Sunshine on `127.0.0.1`, so the
  addresses Sunshine puts in its replies (`rtsp://127.0.0.1:48010`, the UDP ports in RTSP SETUP) match the
  phone end, which listens on `127.0.0.1` on the same port numbers.

## Versions

| Piece | Version |
|---|---|
| `iroh` crate (PC and phone) | 1.4.1, exact (`=1.4.1`) |
| `iroh-relay` binary on the VPS | 1.4.1 |
| `iroh-mdns-address-lookup` | 0.6.1 |
| Rust toolchain | 1.98.0 (iroh needs ≥ 1.91) |
| Tunnel protocol | ALPN `pocketdeck/tunnel/1`, hello version 1 |

Upgrade the crate and the relay together.

## Registration (once per phone, on the LAN)

The phone pairs with Sunshine as it does today. Then, the first time it reaches the PC on the LAN without a
stored `remoteId`, it calls the virtual-display endpoint (HTTPS on base + 30, mutual TLS with its Sunshine
client certificate, exactly like `virtual-display.md`):

```
POST /pocketdeck/v1/remote
{"id": "<phone endpoint id>"}

200 {"id": "<PC endpoint id>", "relay": "https://relay.example.com" | null}
```

The PC stores `phone endpoint id → SHA-256(client certificate DER)` in `%ProgramData%\PocketDeck\remote-peers.json`.
A phone is let into the tunnel only while that certificate is still on Sunshine's paired list, so unpairing in
Sunshine also ends remote access. A reinstalled phone (new endpoint id, same certificate) replaces its old entry.

The PC's key is `%ProgramData%\PocketDeck\remote-key.bin` (32 bytes, SYSTEM and Administrators only). The phone's
key is generated once and stored encrypted with Android Keystore.

## Wire format

One QUIC connection, ALPN `pocketdeck/tunnel/1`. The phone dials `EndpointAddr { id: pc, relay_url }`.

**Hello** (the first bidirectional stream, opened by the phone):

| Direction | Bytes |
|---|---|
| phone → PC | `"PD"` `version` (1 byte, `1`) |
| PC → phone, accepted | `0` then 7 × u16 big-endian: https, http, rtsp, display, video, control, audio |
| PC → phone, wrong version | `1` `pc version` |

**TCP** (every later bidirectional stream, opened by the phone): 1 byte port tag, then raw bytes both ways.
Tags: `0` https (47984), `1` http (47989), `2` rtsp (48010), `3` display endpoint (48019).

**UDP** (QUIC datagrams, both ways): 1 byte channel tag + one UDP payload. Tags: `0` video (47998), `1` control
(47999), `2` audio (48000). The PC keeps one UDP socket per channel per phone, connected to Sunshine's port.
The phone binds `127.0.0.1:<same port>` and answers to the source port Moonlight last used on that channel.

The path must carry datagrams of at least 1100 bytes (`MIN_DATAGRAM`), or the phone fails with
`DatagramsTooSmall`. The phone streams with `setMaxPacketSize(1024)`.

**Close codes:** `1` not allowed (not registered, or unpaired in Sunshine), `2` no direct path, `3` stopped.

## PC

- The `PocketDeckDisplay` service always runs the host. "Play from anywhere" (Settings → Streaming host;
  `RemoteEnabled` under `HKLM\SOFTWARE\PocketDeck\Display`, default on) only decides whether the host uses
  the relay. Off means the tunnel works on the LAN only.
- The relay URL is built in from `POCKETDECK_RELAY_URL` at compile time, and can be overridden with the
  `RelayUrl` value under the same key.
- Setup adds a UDP firewall rule for the service program (direct LAN and hole-punched paths).
- Pipe: `remote status` → `{"state": "off|connecting|lan|internet|relayed|no-direct-path", "rttMs", "peers",
  "enabled", "relay", "id", "message"}`; `remote on`, `remote off`.
- Test tunnels: `PocketDeck.exe --display remote host [relay|none]` and
  `PocketDeck.exe --display remote dial <pc id> [relay|none]` (then point desktop Moonlight at `127.0.0.1`).
  The test host lets every peer in, though Sunshine's own pairing still applies.

## Phone

- `remoteId` and `relayUrl` on the stream host. A host with a `remoteId` is always reached through the tunnel:
  NvHTTP, the app list, the display endpoint and the stream all use `127.0.0.1` with the PC's ports.
- While the tunnel runs, the phone holds a Wi-Fi multicast lock so mDNS works on the LAN.
- The stream shows a "LAN" or "Internet" chip. `NoDirectPath` or `LostDirectPath` ends it with
  "Can't reach your PC directly on this network".
- The tunnel stops after 60 s without use.
