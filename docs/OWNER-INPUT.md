# Owner input required

Items below need the repository owner; nothing else blocks them. Each entry
says exactly what is needed and which ledger row is waiting on it. Never stub
around these — the honest state is BLOCKED.

| # | What | Format | Unblocks |
|---|---|---|---|
| 1 | Android applicationId | e.g. `dev.marfim.app` (reverse-DNS); used in apps/flutter build.gradle + FFI start path | W4.2/W4.5 |
| 2 | Release signing secrets | keystore file + store/key passwords (GitHub Actions secrets `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`); absent → CI debug-signs (documented, not silent) | W4.5 alpha tag |
| 3 | Oracle tag | an `archive/*` git tag (or push of the preserved Node `marfim` v1.2.0 tree) so the ~40k LOC LLM-coupled intelligence port can resume | W13 |
| 4 | Live API keys | at least one real provider key (e.g. `ANTHROPIC_API_KEY`) as a CI secret for the real-provider leg of the alpha gate | W4.5 |
| 5 | Test-phone info | a 6 GB low-end Android device profile (or accept the emulator x86_64 numbers as the recorded baseline) for the performance targets in W4.3 | W4.3/W4.5 |

Notes:
- All five are additive: the work that can ship without them ships (flagged
  default-off where an invariant conflicts), and these rows are the only
  remainder.
- When an input arrives, delete the row here, flip the ledger row, and land
  the work in the normal way.
