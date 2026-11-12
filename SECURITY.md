# Security policy

## Supported versions

Only the latest release of PocketDeck for PC gets security fixes.

## Reporting a vulnerability

Please **don't open a public issue** for security problems.

Report it privately through GitHub instead: go to the repository's **Security** tab and choose **Report a vulnerability** ([direct link](https://github.com/MohammedAbidNafi/Pocket-Deck-PC/security/advisories/new)). Include what you found, how to reproduce it, and the PocketDeck version.

You should get a reply within a week. Once a fix is released, the report can be published as a security advisory, with credit to you if you'd like.

## Scope

In scope: PocketDeck for PC as built here, including:

- pairing phones by QR code (the PIN, certificate fingerprint and which pairing request gets answered)
- the Sunshine web login PocketDeck creates and stores (encrypted with Windows DPAPI)
- downloading and installing Sunshine (checksum checks, the administrator prompt)
- anything that runs elevated or talks to the network

Issues in Sunshine itself should be reported to [Sunshine](https://github.com/SunshineStream/Sunshine). If one affects PocketDeck, a report here is welcome too.
