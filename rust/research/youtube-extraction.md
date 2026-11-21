# YouTube Music / InnerTube API & Stream Extraction in Rust

Primary-source technical reference for `crates/bitchord-innertube` (Wave 2, **ADR-004: Native-First with `yt-dlp` Dev Bridge**). Every protocol detail, client identity, timeout, and pitfall below is traced directly to BitChord v1.9.1 upstream Kotlin/TypeScript sources (`reference/BitChord/`), local `yt-dlp 2026.10.26` extractor code, and crates.io / `cargo info` metadata verified on 2026-11-21.

---

## 1. InnerTube Protocol, Client Identities, Headers & Authentication

### 1.1 Endpoints & Request Shapes

BitChord separates **metadata/account/library/write APIs** (`Innertube.kt`, always `WEB_REMIX` against `music.youtube.com`) from **playable stream URL resolution** (`StreamResolver.kt` / `InnerTubeXResolver.kt`, multi-client catalog) (`Innertube.kt:49-55`).

| Category | Method & Endpoint | Body / Query Shape | Auth / Notes | Source |
| :--- | :--- | :--- | :--- | :--- |
| **Visitor bootstrap** | `GET https://www.youtube.com/sw.js_data` | Strip anti-hijacking prefix (`substringAfter("\n", body.drop(5))`), scan JSON arrays for `^Cg[A-Za-z0-9_%-]{40,}$` | Anonymous; Chrome UA | `Innertube.kt:178-193` |
| **Session scope shell** | `GET https://music.youtube.com/` | Regex-extract `LOGGED_IN`, `DATASYNC_ID`, `DELEGATED_SESSION_ID`, `SESSION_INDEX`, `VISITOR_DATA`, `INNERTUBE_CLIENT_VERSION` from `ytcfg` | Cookie + `SAPISIDHASH`; session-bound `visitorData` outranks anonymous | `Innertube.kt:414-460` |
| **Browse** | `POST .../youtubei/v1/browse` | `{ context, browseId, params? }` | Signed when logged in | `Innertube.kt:529-533` |
| **Browse continuation** | `POST .../youtubei/v1/browse?ctoken=T&continuation=T&type=next` | `{ context, continuation: T }` (sent in **both** query and body) | Signed when logged in | `Innertube.kt:541-548` |
| **Search** | `POST .../youtubei/v1/search` | `{ context, query, params? }` (`gl` omitted except `zh-*`) | `anonymous=true` strips Cookie/Auth; typeahead always anonymous so keystrokes don't pollute history | `Innertube.kt:629-640, 676-679` |
| **Search continuation** | `POST .../youtubei/v1/search?ctoken=T&continuation=T&type=next` | `{ context, continuation: T }` | Same `searchRegion` rules | `Innertube.kt:643-649` |
| **Search suggestions** | `POST .../youtubei/v1/music/get_search_suggestions` | `{ context, input }` | Cheap typeahead queries (`searchSuggestionRenderer`) | `Innertube.kt:660-663`, `InnertubeParser.kt:136-143` |
| **Watch / Radio queue** | `POST .../youtubei/v1/next` | Track radio: `{ videoId, playlistId: "RDAMVM<id>", isAudioOnly: true }`; playlist: `{ videoId?, playlistId, params?, isAudioOnly: true }` | Signed-in responses include `playlistPanelVideoWrapperRenderer` audio/video counterparts | `Innertube.kt:603-620`, `InnertubeParser.kt:1281-1310` |
| **Captions / Transcript** | `POST .../youtubei/v1/get_transcript` | `{ params: base64([0x0A, len(videoId), ...utf8(videoId)]) }` | Last-resort lyrics source | `Innertube.kt:623-627` |
| **Account menu** | `POST .../youtubei/v1/account/account_menu` | `{ context }` | Authoritative liveness check for `X-Goog-AuthUser` slot (`activeAccountHeaderRenderer`) | `Innertube.kt:551`, `ytAuth.ts:239-248` |
| **Accounts / Channels** | `POST .../youtubei/v1/account/accounts_list` & `GET https://www.youtube.com/getAccountSwitcherEndpoint` | Switcher GET strips `)]}'` XSSI prefix; both scanned recursively for `accountItem` (`pageId`, `datasyncIdToken`, `isSelected`) | Switcher GET signed with `origin = https://www.youtube.com` | `Innertube.kt:561-597`, `InnertubeParser.kt:1254-1273` |
| **Playback tracking** | `POST .../youtubei/v1/player` (authenticated `WEB_REMIX`) | `{ videoId, contentCheckOk: true, racyCheckOk: true, playbackContext: { contentPlaybackContext: { html5Preference: "HTML5_PREF_WANTS", referer: "https://music.youtube.com/watch?v=<id>", signatureTimestamp } } }` | Requires current `signatureTimestamp` or omits `playbackTracking` silently | `Innertube.kt:715-753` |
| **Stats pings** | `GET <videostatsPlaybackUrl \| atrUrl \| videostatsWatchtimeUrl>` | Query params: `ver=2&c=WEB_REMIX&cver=<ver>&cpn=<16-char>&cplayer=UNIPLAYER&cbr=Chrome&cbrver=141.0.0.0&cos=Windows&cosver=10.0&hl=<hl_>&cr=<gl>`; watchtime adds `st=0&et=<s>&cmt=<s>&state=playing\|paused` + `final=1`; `atrUrl` adds only `cpn` | Must carry `X-Goog-Visitor-Id`, `Cookie`, `Authorization`, `X-Goog-AuthUser`, `X-Goog-PageId`. Sequence: playback → atr (~5s) → watchtime every 30s + final | `Innertube.kt:772-859`, `PlaybackTracker.kt:23-35, 167-286` |
| **Account writes** | `like/like`, `like/dislike`, `like/removelike`, `subscription/subscribe`, `subscription/unsubscribe`, `feedback`, `playlist/create`, `playlist/delete`, `browse/edit_playlist` | `like/*` targets `{ target: { videoId \| playlistId } }`; `edit_playlist` strips `VL` prefix and sends `actions[]` (`ACTION_ADD_VIDEO`, `ACTION_REMOVE_VIDEO`, `ACTION_MOVE_VIDEO_BEFORE`, `ACTION_SET_PLAYLIST_NAME`) | **Inspect JSON body on HTTP 200**: refusals return `200` with `error` object or `status != "STATUS_SUCCEEDED"` | `Innertube.kt:882-1105` |

**Standard Browse IDs & Search Filter Params:**
- **Feeds / Explore:** `FEmusic_home`, `FEmusic_new_releases`, `FEmusic_explore`, `FEmusic_moods_and_genres`, `FEmusic_mixed_for_you` (`YtMusicRepository.kt:84, 97-98, 304, 1204`).
- **Library (signed-in):** `FEmusic_history`, `FEmusic_liked_videos` (Songs), `FEmusic_liked_playlists`, `FEmusic_liked_albums`, `FEmusic_library_corpus_track_artists` (Artists), `FEmusic_library_corpus_artists` (Subscriptions), `FEmusic_library_non_music_audio_list` (Podcasts) (`YtMusicRepository.kt:293, 951-973`).
- **Search filters** (`Models.kt:391-399`):
  - `ALL`: `None`
  - `SONGS`: `"EgWKAQIIAWoKEAkQChAFEAMQBA=="`
  - `VIDEOS`: `"EgWKAQIQAWoKEAkQChAFEAMQBA=="`
  - `ALBUMS`: `"EgWKAQIYAWoKEAkQChAFEAMQBA=="`
  - `ARTISTS`: `"EgWKAQIgAWoKEAkQChAFEAMQBA=="`
  - `PLAYLISTS`: `"EgWKAQIoAWoKEAkQChAFEAMQBA=="`

### 1.2 Client Identities (`PlayerClient` + InnerTubeX + `yt-dlp 2026.10.26`)

Stream URLs on `googlevideo.com` bake `c=<CLIENT_NAME>` and `cver=<VERSION>` into their query string and enforce header/UA parity on media fetches (`PlayerClient.kt:8-37`). Native mobile/XR clients MUST NOT send `Origin` or `Referer`; browser-shaped clients MUST send both (`PlayerClient.kt:22-42`, `youtube-playback.md:45`).

| Client (`clientName`) | Numeric ID | Version (BitChord / `yt-dlp`) | Origin / Referer | JS Player? | PO Token Policy (2026-10) | Notes | Primary Source |
| :--- | :---: | :--- | :--- | :---: | :--- | :--- | :--- |
| **`WEB_REMIX`** | `67` | Live from `ytcfg` (fallback `1.20250101.01.00`; media `1.20260707.12.00`) | `https://music.youtube.com` | Yes (`sts` + `sig`/`n`) | GVS required (not for Premium); player-request optional/recommended | Primary browse/search/library/write/stats identity; signed-in age-restricted stream fallback | `Innertube.kt:98-99, 353-354`, `PlayerClient.kt:90-95`, `yt-dlp/_base.py` |
| **`VISIONOS`** | `101` | `1.02` (`VISIONOS_0_1` = `0.1` in InnerTubeX) | **None** | **No** (`REQUIRE_JS_PLAYER=False`) | **None** (no BotGuard) | Primary fast-path direct Opus 251 audio client in BitChord web & `yt-dlp` `_DEFAULT_JSLESS_CLIENTS`; excludes "Made for kids" | `PlayerClient.kt:134, 140-144`, `youtube-playback.md:7, 43-45, 62`, `yt-dlp/_base.py`, `yt-dlp/_video.py` |
| **`TVHTML5` / `tv_samsung`** | `7` | `7.20260707.07.00` (BitChord) / `5.20260707` (`tv_samsung` Tizen 2.4.0) | `https://www.youtube.com` (TVHTML5) | Yes | No GVS POT required on `tv_samsung` | `yt-dlp` default #1 client (`_DEFAULT_CLIENTS = ('tv_samsung', 'visionos', 'web')`); supports cookies | `PlayerClient.kt:104-110`, `yt-dlp/_base.py`, `yt-dlp/_video.py` |
| **`TVHTML5_SIMPLY`** | `75` | `1.0` | `https://www.youtube.com` | Yes | GVS HTTPS/DASH required | Narrow 512 KiB range cap on `googlevideo.com` | `PlayerClient.kt:151-152`, `yt-dlp/_base.py` |
| **`WEB`** | `1` | `2.20260708.00.00` | `https://www.youtube.com` | Yes | GVS HTTPS/DASH required (exempt Premium) | Signed-in fallback; Chrome 141 UA in BitChord | `PlayerClient.kt:97-102`, `yt-dlp/_base.py` |
| **`MWEB`** | `2` | `2.20260708.05.00` | `https://www.youtube.com` | Yes | GVS HTTPS/DASH required | Offers `ultralow` formats; iPad Safari UA | `PlayerClient.kt:133`, `yt-dlp/_base.py` |
| **`WEB_EMBEDDED_PLAYER`** | `56` | `2.20260708.00.00` | `https://www.youtube.com` | Yes | None | `yt-dlp` `_DEFAULT_AUTHED_CLIENTS` #1; failed on browser-shaped web tests ("unavailable") | `yt-dlp/_base.py`, `resolver.ts:13` |
| **`IOS`** | `5` | `21.26.4` / `21.29.1` | **None** | **No** | GVS HTTPS & HLS required (exempt with player token) | iPhone16,2 iOS 18.3.2/18.5 UA; fallback client in `PlayerClient.forStreamUrl` | `PlayerClient.kt:52-61, 120-136`, `yt-dlp/_base.py` |
| **`ANDROID_MUSIC`** | `21` | `8.39.42` | **None** | **No** | App attestation / POT | Pixel 9 Pro Android 15 UA | `PlayerClient.kt:70-75` |
| **`ANDROID`** | `3` | `21.26.364` | **None** | **No** | GVS HTTPS/DASH required | Pixel 9 Pro / Cronet 132 UA | `PlayerClient.kt:63-68`, `yt-dlp/_base.py` |
| **`ANDROID_VR`** | `28` | `1.65.10` / legacy `1.43.32` | **None** | **No** | **Degraded**: since 2026-08-17 all formats 403 on `1.65.10` | Narrow 512 KiB range cap (`PlayerClient.kt:151-152`); keep out of top tier | `PlayerClient.kt:77-88, 151`, `yt-dlp/_base.py` |

### 1.3 Common `postMusic` Headers, Body Context & Locale Rules

Every `POST https://music.youtube.com/youtubei/v1/<endpoint>` request carries (`Innertube.kt:1129-1178`):
- **Query params:** `prettyPrint=false`, `hl=<currentLanguage>` (plus continuation query params when paging).
- **Static & session headers:**
  - `Content-Type: application/json`
  - `Accept-Language: <LocaleTags.acceptLanguage>` (`en-US,en;q=0.9`, or script-specific for `zh-Hant`/`zh-Hans`; `LocaleTags.kt:83-93`)
  - `X-Origin: https://music.youtube.com`, `Origin: https://music.youtube.com`, `Referer: https://music.youtube.com/`
  - `X-YouTube-Client-Name: 67`
  - `X-YouTube-Client-Version: <webRemixVersion>`
  - `X-Goog-Visitor-Id: <visitorData>` (when present; harvested from `responseContext.visitorData` on first call if not yet fetched, `Innertube.kt:1181-1184`)
- **Authenticated headers (only when `cookie != null` and NOT `postMusicAnonymous`):**
  - `Cookie: <raw cookie header>`
  - `X-Goog-AuthUser: <authUser>` (from channel override → `SessionScope.authUser` → `"0"`, `Innertube.kt:343-344`)
  - `X-Goog-PageId: <pageId>` (only for brand channels; omitted for personal accounts, `Innertube.kt:219-221, 1151`)
  - `Authorization: SAPISIDHASH <unix_sec>_<hex_sha1>` (`Innertube.kt:1152, 1280-1286`)
- **JSON body `context`:**
  ```json
  {
    "context": {
      "client": {
        "clientName": "WEB_REMIX",
        "clientVersion": "<webRemixVersion>",
        "hl": "<ytHl>",
        "gl": "<ytGl or omitted for non-zh search>",
        "visitorData": "<visitorData>"
      },
      "user": {
        "lockedSafetyMode": false,
        "onBehalfOfUser": "<dataSyncId>"
      },
      "request": { "useSsl": true }
    }
  }
  ```
- **Locale subtlety (`LocaleTags.kt:49-81`):** `zh-Hant` → `hl=zh-TW, gl=TW`; `zh-Hans` → `hl=zh-CN, gl=SG`; other languages → `hl=<iso639>, gl=US` for browse, **but `gl` is omitted for non-Chinese search** (`ytSearchGl`) so YouTube ranks results by IP geolocation rather than forcing US rankings (`LocaleTags.kt:69-81`).

### 1.4 `SAPISIDHASH` Auth Cookie Scheme & Multi-Account Identity

1. **Secret extraction (`Innertube.kt:1266-1278`, `AuthStore.kt:212-220`, `ytAuth.ts:20`):**
   Parse the `Cookie` header by splitting `;` and splitting each pair on the first `=` (trimming both). Match the **exact cookie name** in priority order:
   1. `SAPISID`
   2. `__Secure-3PAPISID`
   3. `__Secure-1PAPISID`
   Value must be non-empty.
2. **Digest formula (`Innertube.kt:1280-1286`, `ytAuth.ts:65-70`):**
   ```text
   ts     = floor(unix_timestamp_seconds)
   digest = lowercase_hex(SHA1(utf8("{ts} {secret} {origin}")))
   header = "SAPISIDHASH {ts}_{digest}"
   ```
   A single `SAPISIDHASH` matches Chrome's signed-in responses identically to the three-hash `SAPISIDHASH … SAPISID1PHASH … SAPISID3PHASH …` form (`youtube-signin.md:12, 56`). `origin` MUST match the target host (`https://music.youtube.com` for YT Music APIs, `https://www.youtube.com` for `/getAccountSwitcherEndpoint`, `Innertube.kt:590`).
3. **Channel & `dataSyncId` scoping (`Innertube.kt:225-453`, `DataSyncId.kt:8-13`):**
   - `DATASYNC_ID` from `ytcfg` often arrives as `"account||delegated"`; `normalizeDataSyncId` extracts the non-blank suffix after `"||"`, falling back to the prefix before `"||"`.
   - For brand channels, `pageId` (`DELEGATED_SESSION_ID`) is sent as `X-Goog-PageId` and used as `dataSyncId` (`Innertube.kt:434-437`).
   - **Important:** BitChord's web extension intentionally omits `onBehalfOfUser` because a stale `dataSyncId` triggers HTTP `400`/`401` on every request, while `X-Goog-AuthUser` + `X-Goog-PageId` already select the channel (`youtube-signin.md:14`, `Innertube.kt:215-218, 294-296`). On `401`/`403`, `YtMusicRepository.call` refreshes `SessionScope` once and retries (`YtMusicRepository.kt:1232-1244`).
   - Browser cookie jars can hold up to 10 Google logins (`X-Goog-AuthUser: 0..9`) with **non-contiguous expired slots**. Probe each slot via `account/account_menu` (never trust `accounts_list` alone — it returns another login's channels for expired indices!) and default to `SESSION_INDEX` from `music.youtube.com` HTML (`youtube-signin.md:18-20`, `ytAuth.ts:233-303`).

---

## 2. BitChord's `StreamResolver` Pipeline Architecture

BitChord's stream resolution (`StreamResolver.kt`, `InnerTubeXResolver.kt`, `PlayerClient.kt`, `ChunkedDataSource.kt`, `potoken/*`) is a layered, coalesced, probe-verified state machine:

```text
resolve(videoId)
  ├─ 1. Latency cache (`recent`, 20m TTL, max 32) ──────────► hit: return URL
  ├─ 2. Verdict cache (`unplayable`, 10m TTL) ──────────────► hit: throw PermanentlyUnplayableException
  └─ 3. Single-flight coalescing (`inFlight` Deferred)
        ├─ 3a. UploadedTracks check (known upload → uploaded=true, playRefusedUpload=true)
        ├─ 3b. Tier 1: InnerTubeXResolver.extract() [up to 3 attempts, skipping refused profileIds]
        │       ├─ Client catalog + Zemercipher remote player_configs.json (6h TTL + ETag)
        │       ├─ Fallback: QuickJS EJS cipher/n-solver + disk cache (`innertubex_players`, keep 3)
        │       ├─ BotGuard PoTokenGenerator (WEB_BOTGUARD clients only)
        │       ├─ Probe URL (Range past 1 MiB, client headers, read 16 KiB, 6s timeout)
        │       └─ On attempt 0 REFUSED + signed in → escalate once via askAsUpload(uploaded=true)
        ├─ 3c. Live check (`live=1` or `source=yt_live_broadcast`) ──► liveHlsStream() (.m3u8)
        └─ 3d. Tier 2 Failsafe: NewPipeExtractor (serialized via extractionGate Mutex)
                ├─ Isolated OkHttp pool (4 conns, 30s keepalive, 5s H2 ping, 5s callTimeout)
                ├─ Intercept `/youtubei/v1/next` → return 81-byte EMPTY_NEXT_RESPONSE
                ├─ Direct `StreamExtractor.audioStreams` (skip video formats: 2.3s vs 49.8s)
                └─ Up to 3 attempts (short-circuit immediately on permanentReason())
```

### 2.1 InnerTubeX Client Catalog & Cipher Tiers (`InnerTubeXResolver.kt`)

BitChord depends on `com.github.MetrolistGroup.innertubex:innertubex:v0.7.0` (`shared/build.gradle.kts:46`) configured with:
- **Stream capabilities:** `allowHls = false`, `allowSabr = false`, `allowBoundedRange = true` (`InnerTubeXResolver.kt:239`) — explicitly prefers progressive/adaptive direct audio URLs over SABR (`check(stream.sabrBootstrap == null)` at `:248`).
- **Two-tier cipher resolution (`YouTubeCipherService` + `RemotePlayerConfigStore`):**
  1. **Tier 1 — Remote config (`ZemerTeam/zemer-cipher`):** Fetches `https://raw.githubusercontent.com/ZemerTeam/zemer-cipher/master/library/src/main/assets/player_configs.json` (`InnerTubeXResolver.kt:400-401`), persisted via `ConfigStore` (`json`, `cached_at_ms`, `source_url`, `etag`, 6h TTL). Maps player JS hashes to `sts` (`signatureTimestamp`), `sig` call expressions, and `nClass` transforms without needing full AST analysis.
  2. **Tier 2 — Embedded QuickJS EJS solver:** When `zemer-cipher` lacks the rotated player hash, InnerTubeX runs an EJS AST solver in QuickJS (~8.7 s cold, `InnerTubeXResolver.kt:99-123`) and persists the **preprocessed player** atomically (`$key.tmp` → rename) in `innertubex_players/`, retaining only the 3 newest files (`KEPT_PLAYERS = 3`, `:124-140`).
- **Prewarming:** `warm(WARM_DELAY_MS = 2_000L)` initializes player config, EJS solver, and BotGuard off the critical start path so age-restricted tracks don't incur 15–20 s cold latency on first tap (`InnerTubeXResolver.kt:97-117`).
- **Self-healing & client exclusion:** Each resolved URL is indexed in `minted` (max 64) so `mediaHeadersFor(url)` returns exact per-client headers (`:207, 365`). On mid-playback `403`/`404`/`410`, `onPlaybackRefused` calls `InnerTubeXResolver.onRefused(url)` which benches that `(videoId, profileId)` for `EXCLUDE_MS = 10 min`, evicts `recent[videoId]`, and triggers `cipherService.refreshAfterStreamRejection()` (`InnerTubeXResolver.kt:372-383`, `StreamResolver.kt:935-947`).
- **Session invalidation:** `StreamResolver.onSessionChanged()` clears `excluded`, `minted`, `unplayable`, `UploadedTracks`, bumps `sessionGeneration`, and re-warms immediately (`InnerTubeXResolver.kt:385-391`, `StreamResolver.kt:361-365`).

### 2.2 BotGuard PoToken Generation (`potoken/*` & `botguard.ts`)

Both Android (`PoTokenWebView.kt`) and Web (`botguard.ts`) implement the **BgUtils v3.2.0** flow (`PoTokenPage.kt:12`, `PoTokenWebView.kt:166-255, 367-398, 442-443`):
1. **`POST https://www.youtube.com/api/jnn/v1/Create`**
   - Headers: `Content-Type: application/json+protobuf`, `Accept: application/json`, `x-goog-api-key: AIzaSyDyT5W0Jh49F30Pqqtyfdf7pDLFKLJoAnw`, `x-user-agent: grpc-web-javascript/0.1`.
   - Body: `["O43z0dpjhgX20SCx4KAo"]`.
   - Response parsing (`JavaScriptUtil.kt:17-61, 99-104`): If `arr[1]` is a string, base64-decode (replacing `-→+`, `_→/`, `.→=`) and add `+97` (`(byte + 97).toByte()`, wrapping mod 256) to each byte, then JSON-parse into `[messageId, safeScript, trustedUrl, interpreterHash, program, globalName, _, clientExperimentsStateBlob]`.
2. **Execute BotGuard VM in a DOM environment:** Load `interpreterJavascript` (inside `PO_TOKEN_HTML` on base URL `https://www.youtube.com`, `blockNetworkLoads = true`), invoke `window[globalName].a(program, vmFunctionsCallback, true, undefined, noop, [[], []])`, wait for `asyncSnapshotFunction`, and call `snapshot({ webPoSignalOutput })` → yields `(botguardResponse, webPoSignalOutput)` (`PoTokenPage.kt:21-119`).
3. **`POST https://www.youtube.com/api/jnn/v1/GenerateIT`**
   - Same headers **plus YouTube Origin (`https://www.youtube.com`)** — an integrity token minted under a non-YouTube origin is silently rejected and streams die at ~70 s (`botguard.ts:6-11`, `youtube-playback.md:33`).
   - Body: `["O43z0dpjhgX20SCx4KAo", "<botguardResponse>"]`.
   - Response: `[base64IntegrityToken, ttlSeconds]`; expiration is set to `now + ttlSeconds - 10 min` (`JavaScriptUtil.kt:68-71`, `PoTokenWebView.kt:213-217`).
4. **Minter creation & token minting:**
   - Call `createPoTokenMinter(webPoSignalOutput, integrityTokenU8)` **exactly once** (`PoTokenPage.kt:129-176`) and cache the resulting `poTokenMinter` callback per `visitorData` session (`PoTokenGenerator.kt:106-148`).
   - Mint tokens via `obtainPoToken(utf8Bytes(identifier))` → `Uint8Array` (100–140 bytes, typically 110–128) → URL-safe base64 (`+→-`, `/→_`, `JavaScriptUtil.kt:86-94`, `PoTokenPage.kt:185-220`).
   - **Two tokens per playback (`PoTokenGenerator.kt:121, 134, 144-147`, `youtube-playback.md:40-43`):**
     - `playerRequestPoToken`: bound to `sessionId` (`visitorData`), sent in `serviceIntegrityDimensions.poToken`.
     - `streamingDataPoToken`: bound to `videoId`, sent as `pot=` query param on direct URLs or in SABR `streamerContext.poToken`.
   - **Guardrails:** 8 s overall timeout (`POTOKEN_TIMEOUT_MS`), 45 s init timeout, 15 s generate timeout, unique per-call `requestKey` (`"$identifier#$counter"`) in IIFE so concurrent calls for the same `videoId` don't clobber continuations, and `BadWebViewException` only for pre-init crashes (post-init errors are transient `PoTokenException`) (`PoTokenGenerator.kt:66, 103`, `PoTokenWebView.kt:92-115, 265-305`).

### 2.3 URL Probing & Bounded-Range Streaming (`StreamResolver.kt:808-923`, `ChunkedDataSource.kt`)

No URL is returned or cached until `probe(url)` proves it serves real audio bytes past YouTube's 1 MiB auth wall:
- **Start offset:** If `clen > 1 MiB + 16 KiB`, start at `AUTH_BOUNDARY_BYTES = 1_048_576` (1 MiB) — some clients' URLs serve the opening megabyte and `403` everything past it (`StreamResolver.kt:831-833, 842-843, 919`).
- **End offset:** Capped by `PlayerClient.rangeBytesFor(url)` — `512 KiB` for `ANDROID_VR` and `TVHTML5_SIMPLY*`, `1 MiB` for all other `googlevideo.com` URLs (`PlayerClient.kt:147-159`). Guard against `Long.MAX_VALUE` overflow (`StreamResolver.kt:844-853`).
- **Checks (6 s `callTimeout`, exact client `mediaHeadersFor(url)`):**
  1. `403`, `404`, `410` → `Probe.REFUSED` (client/token fault → bench client & retry next).
  2. Other non-2xx (except `416`) → `Probe.UNREACHABLE`.
  3. `Content-Type` does not start with `"audio/"` → `Probe.REFUSED` (catches HTML consent/captcha interstitials dressed as HTTP 200).
  4. Must actually read `PROBE_READ_BYTES = 16 KiB` from the body (`StreamResolver.kt:867-871, 922`) — catches header-then-stall throttling.
- **Chunked media reads (`ChunkedDataSource.kt:17-43, 105`):** An unbounded `GET` to `googlevideo.com` is server-paced to **15.5 kB/s** (~playback speed), whereas bounded `Range` requests run at **5.7 MB/s** (measured over the same connection). Every `googlevideo.com` read MUST be split into bounded ranges ≤ `PlayerClient.rangeBytesFor(url)`.

---

## 3. Rust Ecosystem Evaluation & Recommended Architecture (ADR-004)

### 3.1 Primary-Source Crate Evaluation (`cargo info`, 2026-11-21)

| Crate | Version | License | MSRV | Key Technical Facts & Fit for `bitchord-innertube` |
| :--- | :--- | :--- | :--- | :--- |
| **`reqwest`** | `0.13.5` | MIT / Apache-2.0 | `1.85.0` | **Use.** `default-tls` is now `rustls` (`aws-lc-rs` + `rustls-platform-verifier`), `http2` on by default; supports `cookies`, `gzip`/`brotli`, `http2_keep_alive_interval`, `pool_idle_timeout`, `pool_max_idle_per_host`. |
| **`rquickjs`** | `0.14.0` | MIT | `1.87` | **Use.** Safe bindings to **QuickJS-NG (~0.16.2)** — exceeds `yt-dlp 2026.10.26`'s minimum `quickjs-ng >= 0.12.0` recommendation (`quickjs.py`). Lightweight C build (fits ADR-007 laptop budget); supports `futures` (`AsyncRuntime`/`AsyncContext`) and `loader`. |
| **`ytmapi-rs`** | `0.3.4` | MIT | — | **Reference only.** Clean `reqwest 0.13.3` + `sha1 0.11.0` YouTube Music metadata API (browser cookie + OAuth), **no stream extraction/cipher/PoToken**. BitChord has bespoke parser logic (podcasts, uploads, version counterparts, artist credits, shelf layouts) already tested in Kotlin — writing a lean `bitchord-innertube` client avoids fighting wrapper abstractions. |
| **`rustypipe`** | `0.11.4` | **GPL-3.0** | `1.67.1` | **Reference only.** Actively maintained (`rquickjs 0.9.0`, `ress`, `reqwest 0.12.0`, external `rustypipe-botguard`), compatible with BitChord's GPL-3.0 `LICENSE`, **but** pins older `reqwest 0.12`/`rquickjs 0.9`, models YouTube (not BitChord's YT Music shelves/uploads/counterparts), and uses its own regex/AST deobfuscator rather than `yt-dlp/ejs`. Great reference for InnerTube client structs and BotGuard IPC. |
| **`rusty_ytdl`** | `0.7.4` | MIT / Apache-2.0 | `1.79.0` | **Do not use.** Intermittent maintenance; historically breaks on YouTube cipher/n-sig changes and lacks native automated PoToken support. |
| **`rustypipe-botguard`** | `0.1.2` | MIT | `1.70.0` | **External binary only (do not link as library).** Pulls in **`deno_core 0.331.0` (V8 / `rusty_v8`)**, which violates ADR-007's <2.5 GiB build RAM budget and bloats desktop binary size. Works well as an optional external CLI binary with snapshot caching (`~/.local/share/rustypipe/bg_snapshot.bin`). |
| **`bgutil-ytdlp-pot-provider`** | `0.8.1` | GPL-3.0-or-later | — | **External daemon option.** Pure-Rust rewrite (`jim60105/bgutil-ytdlp-pot-provider-rs`) wrapping `rustypipe-botguard 0.1.2` behind `axum 0.8.4` HTTP endpoints (`GET /ping`, `POST /get_pot` on `127.0.0.1:4416`). Zero build/runtime weight when consumed over localhost HTTP. |
| **`wry`** | `0.57.3` | Apache-2.0 / MIT | `1.85` | **Avoid inside `bitchord-desktop`.** Linux `os-webview` pulls `webkit2gtk`, `gtk` (GTK3), `soup3`, `gdkx11` — unacceptable dependency/event-loop conflict inside a pure GPUI Wayland app. Only viable if isolated into a separate optional helper binary. |

### 3.2 Recommended Trait & Crate Design (`crates/bitchord-innertube`)

Split `bitchord-innertube` into cleanly decoupled modules mirroring `Innertube.kt`, `StreamResolver.kt`, `PlayerClient.kt`, and `potoken/`:

```rust
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerPreference {
    Any,        // Playback: prefer Opus/WebM, accept M4A
    WebmOpus,   // Private download: Opus in WebM
    M4aAac,     // Exported download: AAC in MP4
}

#[derive(Debug, Clone)]
pub struct ResolveRequest {
    pub video_id: String,
    pub max_kbps: u32,
    pub container: ContainerPreference,
    pub uploaded_hint: bool,
    pub skip_profiles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub video_id: String,
    pub url: String,
    pub kbps: u32,
    pub mime_type: String,           // e.g. `audio/webm; codecs="opus"`
    pub ext: String,                 // "webm" or "m4a" (derived from container, never ".opus")
    pub content_length: Option<u64>, // From `clen` query param or format metadata
    pub loudness_db: Option<f64>,    // From playerConfig.audioConfig.loudnessDb (None in yt-dlp)
    pub is_live_hls: bool,
    pub client_name: String,
    pub profile_id: String,          // Key for 10-min exclusion on mid-playback 403
    pub media_headers: BTreeMap<String, String>,
    pub range_chunk_bytes: Option<u64>, // 512 KiB (ANDROID_VR/TVHTML5_SIMPLY) or 1 MiB (other googlevideo)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PermanentReason {
    #[error("This track is age-restricted. Sign in to YouTube to play it.")]
    AgeRestrictedSignedOut,
    #[error("YouTube will not serve this age-restricted track to this account.")]
    AgeRestrictedSignedIn,
    #[error("This track isn't available in your country")]
    GeoRestricted,
    #[error("This track is paid content")]
    PaidContent,
    #[error("This track needs YouTube Music Premium")]
    PremiumRequired,
    #[error("This track is private")]
    Private,
    #[error("The channel behind this track was terminated")]
    AccountTerminated,
    #[error("This track was removed from YouTube")]
    Removed,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("{0}")]
    Permanent(#[from] PermanentReason),
    #[error("stream refused by upstream ({status}): {detail}")]
    Refused { status: u16, profile_id: String, detail: String },
    #[error("temporary extraction failure: {0}")]
    Transient(String),
    #[error("rate limited / reCAPTCHA challenge")]
    RateLimited,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Backends: `NativeStreamResolver` (primary), `YtDlpStreamResolver` (feature-gated dev bridge),
/// and `FallbackResolver<Primary, Secondary>`.
pub trait StreamBackend: Send + Sync {
    fn resolve_uncached<'a>(&'a self, req: &'a ResolveRequest) -> BoxFuture<'a, Result<ResolvedStream, StreamError>>;
    fn on_refused(&self, _video_id: &str, _profile_id: &str, _url: &str) {}
    fn on_session_changed(&self) {}
}

/// Optional BotGuard PO-token provider (`NoopPoTokenProvider`, `BgUtilHttpProvider`,
/// or `RustyPipeBotguardProcessProvider`).
pub trait PoTokenProvider: Send + Sync {
    fn supports_web_botguard(&self) -> bool;
    fn mint<'a>(&'a self, video_id: &'a str, visitor_data: &'a str)
        -> BoxFuture<'a, Result<Option<PoTokenPair>, StreamError>>;
}

pub struct PoTokenPair {
    pub player_request_token: String, // bound to visitor_data
    pub streaming_data_token: String, // bound to video_id
}

/// Orchestrator owning caches, single-flight coalescing, probing, and upload escalation.
/// Callers depend ONLY on this facade (or a `StreamResolver` trait wrapping it).
pub struct StreamResolver { /* recent(20m), unplayable(10m), in_flight(Shared), uploads, prober, backend */ }
```

**Critical HTTP client topology (matching `Http.kt` & `StreamResolver.kt:218-253`):**
1. **Shared primary `reqwest::Client`** (`Arc<reqwest::Client>`) used by **both** InnerTube `/player` calls and Wave 3 `bitchord-audio` media streaming + URL probing (sharing DNS resolution & IPv4/IPv6 address family so `googlevideo.com` never sees an IP-family mismatch → 403, `Http.kt:19-23`). Configure `.connect_timeout(20s)`, `.read_timeout(30s)`, `.pool_max_idle_per_host(16)`, `.pool_idle_timeout(Duration::from_secs(300))`, `.http2_keep_alive_interval(Duration::from_secs(15))`, `.http2_keep_alive_while_idle(true)`.
2. **Short-leash `prober` request timeout:** Per-request `.timeout(Duration::from_secs(6))` on the **same** shared `reqwest::Client` (preserves DNS/pool affinity while bounding probe latency, `StreamResolver.kt:901-916`).
3. **Isolated fallback/watch-page client:** Separate `reqwest::Client` with `.pool_max_idle_per_host(4)`, `.pool_idle_timeout(Duration::from_secs(30))`, `.http2_keep_alive_interval(Duration::from_secs(5))`, `.timeout(Duration::from_secs(5))` so stale NAT sockets never stall fallback HTML/JS fetches (`StreamResolver.kt:195-252`).
4. **Single-flight coalescing (`StreamResolver.kt:369-425`):** Spawn the uncached resolution onto a long-lived `tokio::task::spawn`, wrap its join handle or `oneshot` receiver in `futures::future::Shared` (or `tokio::sync::watch`), and remove the `in_flight` map entry **inside the spawned task's completion guard** — NEVER in the awaiting caller's drop/cancel path!

### 3.3 `yt-dlp` Dev Bridge Contract (Feature-Gated `ytdlp-bridge`)

Verified against local `yt-dlp 2026.10.26` (`/home/adttsonaisse/.local/bin/yt-dlp`; local `/usr/bin/node` available on `PATH`).

**Low-latency audio-only invocation flags:**
```bash
yt-dlp \
  --dump-single-json \
  --no-playlist \
  --no-warnings \
  --skip-download \
  --cache-dir "${XDG_CACHE_HOME:-$HOME/.cache}/bitchord/yt-dlp" \
  -f "bestaudio[ext=webm][acodec=opus][abr<=MAX]/bestaudio[ext=m4a][abr<=MAX]/bestaudio/best" \
  --extractor-args "youtube:player_skip=webpage,configs,initial_data;skip=hls,dash,translated_subs" \
  --js-runtimes node \
  -- "https://www.youtube.com/watch?v=<VIDEO_ID>"
```
- **Why each flag matters (verified in `yt_dlp/extractor/youtube/_video.py`):**
  - `--dump-single-json` (`YoutubeDL.py`): Guarantees exactly one JSON object on stdout (clean deserialization with `serde_json::from_slice`) and merges the chosen single format's fields directly onto the top-level JSON object (`info_dict.update(best_format)`).
  - `--no-playlist --skip-download --no-warnings`: Eliminates playlist expansion, disk I/O, and stderr noise.
  - `player_skip=webpage,configs,initial_data`: Skips the heavy watch-page HTML download, per-client `ytcfg` fetches, AND the `/youtubei/v1/next` call (`_video.py`, cutting latency from multi-second to ~1–2 InnerTube calls).
  - `skip=hls,dash,translated_subs`: Skips downloading/parsing HLS/DASH manifests and subtitle translation dictionaries (`_video.py`). (Omit `hls` only when `is_live == true` retry is needed.)
  - `--js-runtimes node`: Local Arch machine has `/usr/bin/node` (no `deno`/`qjs`), so passing `node` avoids runtime-probing warnings when `web`/`tv_samsung` requires EJS `sig`/`n` solving. For fastest JS-less resolution when signed out, prepend `player_client=visionos,tv_samsung,web;` inside `--extractor-args`.
  - **Signed-in bridge:** Write active session cookies to a `0600` temp Netscape cookie jar (`# Netscape HTTP Cookie File`) and pass `--cookies <path>` (delete immediately after process exit).
  - **Format selector adjustment:** For `ContainerPreference::M4aAac`, use `bestaudio[ext=m4a][abr<=MAX]/bestaudio[ext=m4a]`; for `WebmOpus`, use `bestaudio[ext=webm][acodec=opus][abr<=MAX]/bestaudio[ext=webm][acodec=opus]`. Omit `[abr<=MAX]` when `max_kbps == u32::MAX` (with fallback to `bestaudio` without cap so tracks above ceiling still play, matching `StreamResolver.kt:746-759`).

**Top-level JSON fields promoted by `yt-dlp` (`YoutubeDL.py` + `_video.py`):**
```rust
#[derive(serde::Deserialize)]
struct YtDlpSingleJson {
    id: String,
    url: Option<String>,                           // Direct playable stream URL (already sig/n-solved)
    http_headers: Option<BTreeMap<String, String>>, // Exact UA/Origin/Referer headers to reuse
    format_id: Option<String>,                     // e.g. "251", "140", "251-drc"
    ext: Option<String>,                           // "webm" or "m4a"
    acodec: Option<String>,                        // "opus", "mp4a.40.2", "none"
    vcodec: Option<String>,                        // "none" for audio-only
    abr: Option<f64>,                              // Audio bitrate kbps (may be None → fall back to tbr)
    tbr: Option<f64>,                              // Total bitrate kbps (`averageBitrate || bitrate / 1000`)
    asr: Option<u32>,                              // Sample rate Hz (`audioSampleRate`)
    audio_channels: Option<u8>,                    // Channel count (`audioChannels`)
    filesize: Option<u64>,                         // Exact bytes (`contentLength`)
    filesize_approx: Option<u64>,
    protocol: Option<String>,                      // "https", "m3u8_native", "http_dash_segments"
    is_live: Option<bool>,
    live_status: Option<String>,                   // "is_live", "not_live", "was_live", "is_upcoming"
    duration: Option<f64>,
}
```
- **Known gap vs native:** `yt-dlp`'s `_video.py` does **not** extract `playerConfig.audioConfig.loudnessDb` (verified absent in `yt-dlp 2026.10.26`). Leave `ResolvedStream::loudness_db = None` on the bridge path and populate it once the native `/player` resolver is active.
- **Process hygiene:** Enforce a hard `tokio::time::timeout(Duration::from_secs(15), ...)` with `Command::kill_on_drop(true)` and classify non-zero exit `stderr` into `PermanentReason` vs `StreamError::Transient` (e.g., `"Sign in to confirm your age"`, `"Private video"`, `"not made this video available in your country"`, `"This video is only available to Music Premium members"`, `"Video unavailable"`).

### 3.4 Phasing In Native Extraction (`rquickjs` + Cipher Tiers + PoToken)

1. **Phase A (Wave 2 Milestone — immediate playback):**
   - Implement native `InnertubeClient` (`browse`, `search`, `search_suggestions`, `next`, `transcript`, `account_menu`, `accounts_list`, `account_switcher`, writes, `playback_tracking` + stats pings, `SAPISIDHASH`, `SessionScope`, `/sw.js_data` visitor bootstrap) on `reqwest 0.13.5`.
   - Implement `StreamResolver` facade (20m URL cache, 10m permanent verdict cache, `Shared` single-flight coalescing, 1 MiB + 16 KiB `probe()`, `UploadedTracks`, `PlayerClient` header & range-size tables).
   - Wire `YtDlpStreamResolver` behind `#[cfg(feature = "ytdlp-bridge")]` to unblock Wave 2 CLI (`bitchord-cli`), Wave 3 audio engine, and Wave 4 GPUI immediately.

2. **Phase B (Native JS-Less + `rquickjs 0.14.0` Cipher Solver):**
   - Add `NativeStreamResolver` walking a live-prioritized client catalog:
     - **Anonymous order:** `VISIONOS` (ID `101`, `1.02`, JS-less, no PoT — instant ~1 s direct Opus 251) → `TV_SAMSUNG` (`TVHTML5` ID `7`, `5.20260707`, JS cipher, no GVS PoT) → `WEB_REMIX` (`67`, JS cipher + PoT if provider available) → `WEB` (`1`).
     - **Signed-in / age-restricted / upload order:** `WEB_REMIX` (with cookies + PoT) → `TV_SAMSUNG` (with cookies) → `WEB` (with cookies) → `VISIONOS`.
   - **Native `rquickjs 0.14.0` EJS cipher engine (replaces InnerTubeX QuickJS + NewPipe Rhino):**
     - Bundle or cache `yt-dlp/ejs` (`yt.solver.lib.min.js` + `yt.solver.core.min.js`, verified by SHA3-512 hash just like `yt-dlp/extractor/youtube/jsc/_builtin/ejs.py`).
     - Run a dedicated worker thread / `rquickjs::AsyncRuntime` with memory limit (`set_memory_limit(64 * 1024 * 1024)`) and execution interrupt handler (timeout guard).
     - Exact JSON call contract verified from `yt-dlp 2026.10.26` (`ejs.py`):
       - **First time per player hash:** Call `jsc({ type: "player", player: raw_js, requests: [{ type: "n", challenges: [...] }, { type: "sig", challenges: [...] }], output_preprocessed: true })`. Persist the returned `preprocessed_player` string atomically to `$XDG_CACHE_HOME/bitchord/players/<player_hash>.js` (retain newest 3, mirroring `InnerTubeXResolver.kt:124-140`).
       - **Subsequent tracks on same player hash:** Call `jsc({ type: "preprocessed", preprocessed_player, requests: [...] })` — avoids re-parsing the ~2 MB `base.js` AST (~8.7 s cold → milliseconds warm).
       - **Optional Tier 0 fast cache:** Support fetching `ZemerTeam/zemer-cipher`'s `player_configs.json` (6 h TTL + `If-None-Match` ETag) to also obtain current `sts` (`signatureTimestamp`) for `PlaybackTracker` without waiting on full player JS parsing (`InnerTubeXResolver.kt:175-191, 400-401`).
   - Run `StreamResolver` background `prewarm()` 2 s after startup (`InnerTubeXResolver.kt:103-117, 406`) to warm `visitorData`, `SessionScope`, and the latest player's preprocessed solver.

3. **Phase C (PoToken Integration for Web / Signed-In / Age-Restricted):**
   - Implement `PoTokenProvider` in priority order:
     1. **`BgUtilHttpProvider` (recommended default on Linux desktop):** Probes `GET http://127.0.0.1:4416/ping` (configurable URL); if alive, calls `POST /get_pot` with `{"content_binding": "<identifier>", "bypass_cache": false}` (`bgutil-ytdlp-pot-provider 0.8.1`). Keeps V8/Deno completely out of BitChord's binary and compile time.
     2. **`ExternalCommandPoTokenProvider` (optional):** Spawns `rustypipe-botguard` if present in `$PATH` (reuses `~/.local/share/rustypipe/bg_snapshot.bin`).
     3. **`NoopPoTokenProvider` (graceful fallback):** Reports `supports_web_botguard() == false` so the client catalog automatically skips PoToken-requiring profiles (`InnerTubeXResolver.kt:83-85, 143-153`) and serves tracks via `VISIONOS` / `TV_SAMSUNG` / signed-in cookies, falling back to optional `yt-dlp` bridge if enabled.
   - **Note on WebPO attestation caveat (`youtube-playback.md:51-60`):** Even BitChord's own real-browser WebPO token flow observed `stream_protection_status = 2` ("attestation pending", ~60–70 s cutoff) on web clients in Oct 2026 while `VISIONOS` played full tracks reliably. Always keep `VISIONOS` + `TV_SAMSUNG` ahead of `WEB_REMIX` for anonymous playback and validate every web-client URL past `AUTH_BOUNDARY_BYTES` (1 MiB) in `probe()`.

---

## 4. Semantic Traps & Gotchas

### 4.1 UTF-16 JS String Slicing vs. Rust UTF-8 `&str` in Signature / `n` Ciphers
- **Root cause:** JavaScript strings are sequences of **UTF-16 code units** (ECMA-262); YouTube's `sig` and `n` cipher functions use `.split("")`, `.slice(a, b)`, `.splice(a, b)`, `.charCodeAt(i)`, and `.length`, all indexed in UTF-16 code units. Rust `&str` is UTF-8 byte-indexed (and `.chars()` is Unicode scalar-value-indexed, which still disagrees with UTF-16 on non-BMP code points `>= U+10000` where JS sees 2 surrogate code units).
- **Trap 1 (re-implementing cipher ops in Rust):** Porting `s.slice(n)` or `a.splice(0, n)` naively onto Rust `&str` or `Vec<char>` corrupts indices whenever any non-ASCII or astral character appears in cipher tables or string literals extracted from `base.js`.
- **Trap 2 (`rquickjs` FFI boundary):** `rquickjs::String::to_string()` converts QuickJS's internal Latin-1/UTF-16 representation to Rust UTF-8 `String`, which fails or inserts U+FFFD if a JS string contains lone UTF-16 surrogates (`\uD800`–`\uDFFF`) produced by splitting surrogate pairs.
- **Mitigation:** Execute the entire YouTube `sig` and `n` transform **inside `rquickjs` (QuickJS-NG)** so all `.split("")`/`.slice()`/`.charCodeAt()` run with native ECMAScript UTF-16 semantics, and pass only the final URL-safe ASCII `sig`/`n` results (plus ASCII JSON envelopes for `jsc()`) across the Rust↔QuickJS boundary. For any AST/regex pre-extraction in Rust, match byte offsets strictly on ASCII tokens or run the parser inside JS (`yt-dlp/ejs` uses `meriyah` inside QuickJS).

### 4.2 `SAPISIDHASH` Calculation & Session Identity Traps
- **Cookie-name substring bug (`AuthStore.kt:200-220`, `Innertube.kt:1249-1265`, `AuthCookieTest.kt:12-65`):** Testing `cookie.contains("SAPISID")` false-positives on `__Secure-3PAPISID`, causing the app to think it is signed in while `sapisid_from` misses the cookie and sends no `Authorization` header → Google silently treats every write and history ping as anonymous. Always split on `;`, parse `name=value`, require non-empty value, and check exact names `["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"]` in that exact priority order.
- **Origin mismatch:** `SAPISIDHASH` signs `"{ts} {secret} {origin}"` using UTF-8 bytes and lowercase 40-char hex SHA-1 (`Innertube.kt:1280-1286`). Signing with `https://music.youtube.com` and sending to `https://www.youtube.com/getAccountSwitcherEndpoint` fails; `origin` MUST match the request's `Origin`/`X-Origin` header (`Innertube.kt:584-591`).
- **Stale `dataSyncId` (`onBehalfOfUser`) bricking the whole client (`Innertube.kt:114-117, 215-218, 294-296, 422-437`, `youtube-signin.md:14`):** Sending a `DATASYNC_ID` from a signed-out shell or from a different Google account produces HTTP `400`/`401` on **every** InnerTube request. Only populate `onBehalfOfUser` from a shell with `"LOGGED_IN": true`, normalize `account||delegated` via `normalizeDataSyncId`, and clear `scope`, `visitorData`, and `channelOverride` atomically whenever the cookie changes.
- **Non-contiguous `X-Goog-AuthUser` slots (`youtube-signin.md:18`, `ytAuth.ts:27-31, 233-248`):** Users frequently have expired slots (e.g., slots `0` and `3` expired while `1, 2, 4` are active). Always validate each slot `0..9` with `account/account_menu` (expecting `activeAccountHeaderRenderer`) — never rely on `account/accounts_list` alone — and use `SESSION_INDEX` from `music.youtube.com` HTML as the default instead of hardcoding `"0"`.
- **Silent HTTP 200 refusals on writes (`Innertube.kt:878-952, 1004-1022`):** `like/*`, `subscription/*`, and `browse/edit_playlist` return HTTP `200` on rejected mutations. Always inspect top-level `response["error"]` and (for `edit_playlist`) `response["status"] == "STATUS_SUCCEEDED"`.

### 4.3 `/youtubei/v1/next` Rate-Limiting & Latency Traps
- **7 s–12 s hangs on `/next` during stream extraction (`StreamResolver.kt:72-104`):** BitChord measured every other extraction request completing in `110–670 ms`, while NewPipe's unconditional `/youtubei/v1/next` call took 7 s or hung until the 12 s timeout, causing total stream failure.
- **Never call `/next` on the stream-resolution path:** In `yt-dlp` dev bridge, always pass `player_skip=webpage,configs,initial_data` (`_video.py`) to skip `/next`. If any fallback extractor insists on calling `/youtubei/v1/next`, intercept and return a ≥50-byte synthetic JSON envelope (`{"responseContext":{},"contents":{},"currentVideoEndpoint":{},"trackingParams":""}`, `StreamResolver.kt:75-76, 98-104`).
- **Keep `Innertube::next` isolated to AutoPlay / Lyrics / Version-pair lookups**, with request timeouts and cancellation propagation when the user skips tracks.

### 4.4 URL Expiry, `n`-Throttling, 403 Walls & Silent Failure Traps
- **Missing `visitorData` → 1-byte-then-403 silent death (`Innertube.kt:125-135`):** A `/player` call without `visitorData` either returns `LOGIN_REQUIRED` ("Sign in to confirm you're not a bot") or returns HTTP 200 with URLs that serve 1 byte and `403` every real range read. Always call `ensure_visitor_data()` (`/sw.js_data`) before the first `/player` call, and preserve session-bound `visitorData` over anonymous ones (`Innertube.kt:140-170`).
- **`adPlaybackContext: { pyv: true }` 70-second cutoff (`youtube-playback.md:20-24`, `resolver.ts:158-161`):** Never include `adPlaybackContext: { pyv: true }` in `/player` requests — measured across 4/4 `VISIONOS` URLs and SABR, `googlevideo.com` hard-refuses every range past ~1.3 MB (~70 s of audio).
- **1 MiB CDN auth boundary (`StreamResolver.kt:831-843, 919`):** Some clients' URLs succeed on `bytes=0-65535` and `403` past byte `1_048_576`. Always probe starting at `1 MiB` (when `clen > 1 MiB + 16 KiB`) and actually read `16 KiB` of body bytes.
- **Unbounded `GET` throttling vs. bounded `Range` (`ChunkedDataSource.kt:19-43`, `PlayerClient.kt:147-159`):** An un-ranged `GET` on `googlevideo.com` is throttled to **15.5 kB/s** even with a valid `n`-param; chunking into `Range` requests (`≤ 512 KiB` for `ANDROID_VR`/`TVHTML5_SIMPLY*`, `≤ 1 MiB` for all others) restores **5.7 MB/s** line rate.
- **Dual-stack IPv4/IPv6 affinity (`Http.kt:19-23`):** `googlevideo.com` binds minted URLs (`ip=` param) to the resolver's egress IP family. Using one `reqwest::Client` for `/player` and another (or `yt-dlp` subprocess resolving IPv6 while `reqwest` uses IPv4) causes immediate `403` on playback. Share one `reqwest::Client` in native mode; in `yt-dlp` dev bridge mode, pass `-4` (or align `reqwest::ClientBuilder::local_address` / DNS family) if dual-stack 403s appear on IPv6-enabled networks.
- **URL TTL & mid-playback invalidation (`StreamResolver.kt:925-947, 1215-1224`):** Although YouTube `expire=` timestamps span ~6 hours, cache URLs for at most **20 minutes** (`URL_TTL_MS`), and wire `on_playback_refused(url, status)` from `bitchord-audio` on `403`/`404`/`410` (for `*.googlevideo.com` only!) to evict the cache, bench the `profile_id` for 10 min, and refresh remote cipher configs.
- **Missing or stale `signatureTimestamp` silently kills listening history (`Innertube.kt:697-714`, `player-web-remix-no-potoken.json:67-76`):** Authenticated `WEB_REMIX /player` without a current `signatureTimestamp` answers HTTP 200 with `playabilityStatus.status = "UNPLAYABLE"`, subreason `"The page needs to be reloaded."`, and **no `playbackTracking` block**.
- **Container vs. codec extension trap (`StreamResolver.kt:604-630, 1166-1193`):** YouTube's Opus arrives inside a **WebM** container (`audio/webm; codecs="opus"`), never Ogg. Naming downloaded files `.opus` breaks external players expecting Ogg-Opus; always emit `.webm` for Opus and `.m4a` for AAC-in-MP4.
- **Coalesced resolution cancellation bug (`StreamResolver.kt:404-418`):** Never remove `videoId` from `in_flight` inside a caller's `finally`/drop hook; if a read-ahead caller cancels, removing the live task causes the next caller to launch an overlapping second walk (doubling latency from ~250 ms to >4 s).

### 4.5 Playability & Restriction Error Taxonomy

Classify `playabilityStatus` (`status`, `reason`, `desktopLegacyAgeGateReason`) and extractor errors strictly into **Permanent** (cache 10 min in `unplayable`, fail fast without retry) vs. **Transient / Client-Specific** (bench profile 10 min, try next client) to prevent multi-layer retry storms (measured at 27 walks / 50 `youtubei` requests in 2m41s before BitChord added verdict caching, `StreamResolver.kt:314-334`):

| Signal (`playabilityStatus` / Exception / `yt-dlp` stderr) | Classification | Action | Primary Source |
| :--- | :--- | :--- | :--- |
| `status` in `AGE_CHECK_REQUIRED`, `AGE_VERIFICATION_REQUIRED`, `desktopLegacyAgeGateReason` present, or `reason` matches `confirm your age \| age-restricted \| inappropriate \| sign in to confirm` | **Permanent** (per auth state) | If signed out: prompt sign-in (`AgeRestrictedSignedOut`), cache 10m. If signed in: retry with signed-in web clients (`WEB_REMIX` → `TV_SAMSUNG` → `WEB`) before caching `AgeRestrictedSignedIn`. Clear on `on_session_changed()`. | `StreamResolver.kt:1083-1088`, `resolver.ts:184-192`, `yt-dlp/_video.py` `_is_agegated` |
| `reason` matches `country \| region` / `GeographicRestrictionException` / `UnsupportedContentInCountryException` | **Permanent** | Cache `GeoRestricted` 10m; do not retry across clients. | `StreamResolver.kt:1089-1090`, `resolver.ts:193` |
| `PaidContentException` / `YoutubeMusicPremiumContentException` / `reason` matches `premium` | **Permanent** | Cache `PaidContent` / `PremiumRequired` 10m. | `StreamResolver.kt:1091-1092`, `resolver.ts:196` |
| `PrivateContentException` / `AccountTerminatedException` / `status == ERROR` with `removed \| no longer available \| does not exist` | **Permanent** | Cache `Private` / `AccountTerminated` / `Removed` 10m. | `StreamResolver.kt:1093-1094`, `resolver.ts:194-197` |
| `status == UNPLAYABLE` with `"The page needs to be reloaded"` or `LOGIN_REQUIRED` with `"not a bot"` | **Transient / Client fault** | Do **not** cache as permanently unplayable! Refresh `visitorData` once (`ensure_visitor_data(refresh=true)`), refresh `signatureTimestamp` / cipher config, bench current `profile_id` 10m, advance to next client. | `Innertube.kt:127-131, 154-156, 703-705`, `player-web-remix-no-potoken.json:67-76` |
| HTTP `429` | **RateLimited** | Surface `StreamError::RateLimited` immediately; do not hammer retries. | `StreamResolver.kt:165-168` |
| Signed-in track `Probe.REFUSED` (403/404/410) on attempt 0 | **Upload heuristic** | Escalate once with `uploaded_hint = true` (authenticated clients only); on success, record `videoId` in `UploadedTracks` (max 20,000, cleared on session change). | `StreamResolver.kt:528-582`, `UploadedTracks.kt:6-24` |
| `live=1` or `source=yt_live_broadcast` in resolved URL | **Live stream** | Adaptive URL is a dying window; switch to HLS `.m3u8` manifest (`is_live_hls = true`). | `StreamResolver.kt:432-437, 1029-1064` |

---

## 5. Primary Sources Cited

- **BitChord v1.9.1 (local upstream reference, `reference/BitChord/`):**
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/Innertube.kt` (lines 49–1287)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/InnertubeParser.kt` (lines 35–250, 1215–1310, 1765–1839)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/PlayerClient.kt` (lines 8–161)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/StreamResolver.kt` (lines 44–1236)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/InnerTubeXResolver.kt` (lines 43–408)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/UploadedTracks.kt` (lines 5–24)
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/innertube/potoken/{PoTokenGenerator.kt, PoTokenPage.kt, JavaScriptUtil.kt, PoTokenException.kt, PoTokenResult.kt}`
  - `app/src/main/java/com/music/bitchord/data/innertube/potoken/PoTokenWebView.kt` (lines 39–490)
  - `app/src/main/java/com/music/bitchord/data/innertube/PlaybackTracker.kt` (lines 19–287)
  - `app/src/main/java/com/music/bitchord/playback/ChunkedDataSource.kt` (lines 16–211)
  - `app/src/main/java/com/music/bitchord/auth/AuthStore.kt` (lines 200–221) & `app/src/test/java/com/music/bitchord/AuthCookieTest.kt`
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/auth/DataSyncId.kt`
  - `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/{Http.kt, LocaleTags.kt, YtMusicRepository.kt}` & `shared/src/commonMain/kotlin/com/music/bitchord/data/model/Models.kt` (lines 391–399)
  - `shared/build.gradle.kts` (lines 35–48) & `LICENSE` (GPL-3.0)
  - `web/docs/youtube-playback.md`, `web/docs/youtube-signin.md`, `web/extension/lib/ytAuth.ts`, `web/app/src/playback/youtube/{innertube.ts, resolver.ts, botguard.ts}`, `web/fixtures/innertube/player-web-remix-no-potoken.json`
- **External / Local Tooling & Crates (verified 2026-11-21):**
  - `yt-dlp 2026.10.26` (`/home/adttsonaisse/.local/bin/yt-dlp`: `yt_dlp/extractor/youtube/{_base.py, _video.py}`, `yt_dlp/extractor/youtube/jsc/_builtin/{ejs.py, quickjs.py, vendor/_info.py}`, `yt_dlp/YoutubeDL.py`)
  - `rquickjs v0.14.0` / `rquickjs-sys v0.14.0` — https://docs.rs/rquickjs/0.14.0, https://github.com/DelSkayn/rquickjs
  - `reqwest v0.13.5` — https://docs.rs/reqwest, https://github.com/seanmonstar/reqwest
  - `rustypipe v0.11.4` (GPL-3.0) — https://docs.rs/rustypipe/0.11.4, https://codeberg.org/ThetaDev/rustypipe
  - `rustypipe-botguard v0.1.2` (MIT) — https://docs.rs/rustypipe-botguard/0.1.2, https://codeberg.org/ThetaDev/rustypipe-botguard
  - `ytmapi-rs v0.3.4` (MIT) — https://docs.rs/ytmapi-rs/0.3.4, https://github.com/nick42d/youtui
  - `rusty_ytdl v0.7.4` (MIT/Apache-2.0) — https://docs.rs/rusty_ytdl/0.7.4, https://github.com/Mithronn/rusty-ytdl
  - `bgutil-ytdlp-pot-provider v0.8.1` (GPL-3.0-or-later) — https://docs.rs/bgutil-ytdlp-pot-provider, https://github.com/jim60105/bgutil-ytdlp-pot-provider-rs
  - `wry v0.57.3` (Apache-2.0/MIT) — https://docs.rs/wry, https://github.com/tauri-apps/wry
  - `MetrolistGroup/innertubex` (`v0.7.0`) & `ZemerTeam/zemer-cipher` (`player_configs.json`) — https://github.com/MetrolistGroup/innertubex, https://github.com/ZemerTeam/zemer-cipher
