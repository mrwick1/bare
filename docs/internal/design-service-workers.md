# Design: service workers that run

Status: draft for the owner's review. No code is built.

Base: main `4a0baceb`. Inputs: the public WhatsApp Web bundles of 2026-11-26 (`/data/bare-tmp/wa-bundles2`, `wa-bundles3`), and the public service worker script of `https://web.whatsapp.com/sw.js`, downloaded on 2026-11-26. No logged-in page was opened.

## 1. Summary

The owner asked for real service workers, so that WhatsApp Web shows message notifications. The bundles do not support that reason. In the current WhatsApp build, a **message** notification does not use the service worker. It uses `new Notification(title, options)`. Only the **test** notification and the **call** notification use `registration.showNotification`, and only when `Notification.maxActions >= 1` (2 for calls) and `navigator.serviceWorker.controller` is set (section 3.2).

Thus, the live failure ("a message arrived with the page hidden and the grant set, and nothing showed") has another cause. Hypothesis: a mute or squelch rule of `WABaseNotification.triggerNotification` stops it (for example `AppState` or `OfflineResumeInProgress`), or `getIcon()` does not settle. WhatsApp logs `WABaseNotification:triggerNotification muteReason <reason>` when a rule stops a notification. Step 0 of the plan reads that line in a live run.

The video and audio send path does not use the service worker either. The preview comes from a `<video>` or `<audio>` element on a `blob:` URL (`WAWebMediaLoad.loadVideo`, `WAWebFileUtils.getAudioDuration`). bare has no media decoder (`browser_media.rs`), so these elements fail. The service worker serves only **playback** of received videos (`/stream/video?key=…`), and WhatsApp falls back to a full download when no worker controls the page.

Recommendation, if the owner still wants service workers: **S2, a service worker is a worker page that starts for an event and stops when idle (30 s), with its registrations in the profile.** Pages with no registration for their origin pay one hash lookup per navigation and nothing per request. The first steps give `register`, the lifecycle, `ready`, `controller`, `postMessage` and `showNotification`. Fetch interception comes later. Push needs a push service and is out of scope.

## 2. Current state in bare

| Part                                         | Where                                                | State                                                                                                                                                                                                                                                            |
| -------------------------------------------- | ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `navigator.serviceWorker`                    | `browser_web.rs` (`Sw`, `sw_*`)                      | A `ServiceWorkerContainer` (an `EventTarget`). `register()` rejects `NotSupportedError`. `getRegistration()` gives `undefined`, `getRegistrations()` gives `[]`. `controller` is `null`. `ready` is a promise that never settles. Tests: `browser_sw_tests.rs`.  |
| `ServiceWorkerRegistration`, `ServiceWorker` | `browser_web.rs`                                     | Interfaces with no instances. No `showNotification`, no `getNotifications`.                                                                                                                                                                                      |
| `Notification`                               | `browser_notification.rs`, `design-notifications.md` | Page notifications go to a list in the page and an event log, read through bare-mcp `notifications`. `maxActions` is 0. The grant is in `permissions.bin` of the profile.                                                                                        |
| `caches`                                     | `shims/caches.js`                                    | In memory, in one realm (a `WeakMap` in JS). Not shared between pages or with a worker. Lost at navigation. `match` keys only by URL.                                                                                                                            |
| Dedicated workers                            | `browser_worker.rs`, `browser_child.rs`              | A worker is a child page (`Of::Worker`) with its own `Browser` and heap, on the page thread (rule D1 of `design-gmail-frames.md`). A cross-origin isolated page runs it on its own thread (`browser_child_thread.rs`, `ThreadWorker`, `Mailbox`).                |
| Worker global                                | `browser_worker.rs::install_scope`                   | The page global with a prototype swap: `postMessage`, `close`, `importScripts`, `WorkerGlobalScope`, `DedicatedWorkerGlobalScope`.                                                                                                                               |
| Clone between heaps, ports                   | `clone_state_with`, `browser_port_hub.rs`            | Built. A `MessagePort` crosses heaps and threads.                                                                                                                                                                                                                |
| IndexedDB                                    | `browser_idb.rs`, `browser_idb_log.rs`               | One `IdbStore` per origin in the session `WebStorage`, outside any realm. A worker uses the store of its page (`IdbShare`). With a profile, a log per origin on disk.                                                                                            |
| Fetch path                                   | `browser_fetch.rs` (`run_network_tasks`, `run_one`)  | `fetch` and XHR queue a request. The pump does the I/O on worker threads and settles the promise. Scripts, sheets and documents load through `browser_loader.rs` and `browser_session::load_document`. There is no hook where a request can go to another realm. |
| Persistence                                  | `profile.rs`                                         | `cookies.bin`, `local.bin`, `permissions.bin`, `idb/*.log`, with a write to a temporary file, `fsync` and rename. HTTP disk cache is separate.                                                                                                                   |
| Media                                        | `browser_media.rs`                                   | No decoder. Every source fails with `MEDIA_ERR_SRC_NOT_SUPPORTED`.                                                                                                                                                                                               |

## 3. What WhatsApp uses

### 3.1 Registration

`WAWebAppSwInitializer.initializeSw({swScriptPath: "/sw.js", swScope: "/"})` runs after the backend starts (`f6.js`). The steps:

1. Stop when `WAWebFeatureDetectionSwSupport.supported` is false (`"serviceWorker" in navigator`).
2. `refreshStreamingSupport()`. With a controller, `WAWebSwUpdater.updateSw()`.
3. With a controller and gate `26258` off: `getRegistrations()`, then `unregister()` each one whose scope is not the page path. The live gate value is `true` (in the `sw.js` data), so this step does not run.
4. With no controller: `navigator.serviceWorker.register("/sw.js", {scope: "/", updateViaCache: "all"})`. A failure logs `service-worker-registration-failure` and the page goes on.

The page then listens for `controllerchange` and `error` on the container and on the controller (`WAWebSWBusInit`).

### 3.2 Notifications

`WAWebBaseNotificationBanner.$7` chooses the path:

- `showViaServiceWorker === true`: `$14` awaits `navigator.serviceWorker.ready`, then calls `registration.showNotification(title, {...options, data: {notificationBannerKey}})`. Close uses `registration.getNotifications({tag})` and `close()` on the match.
- Else: `$15` calls `new window.Notification(title, options)` and listens for `click`, `close` and `error`.

Who sets `showViaServiceWorker` (all call sites in the bundles):

| Notification                         | Sets it when                                                                                                     |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| `WAWebMsgNotification` (messages)    | Never. Its `getBannerOptions` has no such key.                                                                   |
| `WAWebNotificationsTestNotification` | `Notification.maxActions >= 1` and a controller.                                                                 |
| `WAWebNotificationsCallNotification` | Rich call notifications on, `Notification.maxActions >= 2` and a controller. It adds Accept and Decline actions. |

So with `maxActions` 0, bare never takes the service worker path, also after a service worker runs. A message notification needs only `Notification`, which bare has.

### 3.3 Messages between page and worker (`WAWebSWBus`)

`WAWebSWBus.request(target, action, message)` makes a `MessageChannel`, posts `{action, message, version}` with `port2` to the controller (page side) or to a client (worker side, `self.clients.get(id)`), and waits for the reply on `port1`. The page handles `REQUEST_STREAMING_INFO`, `SEND_STREAMING_CHUNK`, `LOG`, `UPLOAD_LOGS`, `HEARTBEAT`, the call and banner actions. The worker handles `STREAMING_SUPPORTED`, `LOGOUT`, `STOP_COMMS`, `CLEAN_ASSETS` and others. The page probes `STREAMING_SUPPORTED` with a 100 ms timeout.

### 3.4 The worker script

`/sw.js` (16 KB) sets `self.__swData` (gates and config) and calls `importScripts` for one script: `static.whatsapp.net/rsrc.php/v4/yJ/r/Sf0vO6zpQwp.js`, **6.4 MB** of JS (1.2 MB compressed). It contains `WAComms`, Signal crypto, a liboqs WASM module, protobuf code and `dexie`. `WAWebSw.initHandlers` installs these handlers through `WAWebSwBase.addSWBaseHandlers`:

| Handler                           | Events and APIs                                                                                                                                                                                                      |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `WAWebSwBase`                     | `install` → `waitUntil(... self.skipWaiting())`; `activate` → `waitUntil(self.clients.claim() ...)`; `fetch` → `respondWith`; `push`, `notificationclick`, `notificationclose`, `periodicsync`; `message` (the bus). |
| `WAWebSwNotificationClickHandler` | `notificationclick`, `notificationclose`, `clients.matchAll`, `client.focus()`, `clients.openWindow`.                                                                                                                |
| `WAWebSwCallPopoutHandler`        | `fetch` of a same-origin navigation to the call pop-out path; `fetch("/", {method: "HEAD"})`; makes a `Response`.                                                                                                    |
| `WAWebStatusSwPrefetchHandler`    | Status video prefetch actions.                                                                                                                                                                                       |
| `WAWebSwVideoStreamingHandler`    | `fetch` of `GET /stream/video?key=…`: asks the page for keys (`REQUEST_STREAMING_INFO`), downloads, decrypts with `crypto.subtle`, answers ranges. Answers `STREAMING_SUPPORTED` with `!!crypto.subtle`.             |
| `WAWebSwStickerHandler`           | `fetch` of sticker media: `caches` (`wa-stickers`) match or fetch.                                                                                                                                                   |
| `WAWebSwStopCommsHandler`         | Bus actions.                                                                                                                                                                                                         |
| `WAWebSwPeriodicSyncHandler`      | `periodicsync`.                                                                                                                                                                                                      |
| `WAWebSwPushHandler`              | `push`: opens `WAComms`, runs an offline resume, then shows notifications with `showNotification`. Needs Web Push.                                                                                                   |

Counts in the script (text matches): `waitUntil` 85, `indexedDB` 21, `WebSocket` 17, `postMessage` 14, `fetch(` 15, `clients.matchAll` 6, `showNotification` 4, `getNotifications` 10, `caches.open` 3, `WebAssembly` 12, `crypto.subtle` 36.

### 3.5 Video and audio send

`WAWebProcessRawMedia` → `WAWebMediaDataUtils.processRawAudioVideo` → `WAWebImageUtils.generateVideoThumbsAndDuration` → `WAWebMediaLoad.loadVideo`. That makes a `<video>`, sets `src` to `URL.createObjectURL(blob)`, and waits for `loadedmetadata`, `canplaythrough` and `seeked`, with a 20 s timeout. Audio uses `WAWebFileUtils.getAudioDuration` (an `<audio>` element and `seekable.end(0)`) and `WAWebWaveformUtils.computeWaveform`. No step uses `navigator.serviceWorker`. The spinner is a media problem. Hypothesis: bare fires `error`, `loadVideo` rejects, and a retry or a missing catch higher up keeps the spinner. To verify in a live trace. That is a separate task; this design does not fix it.

### 3.6 What a service worker would give WhatsApp in bare

| Feature                     | Needs                                                      | Value in bare                                                                        |
| --------------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Message notifications       | nothing (page `Notification`)                              | None from a service worker.                                                          |
| Call and test notifications | worker + `showNotification` + `maxActions >= 1`            | Low. bare has no actions UI; agents read the log.                                    |
| Streamed playback of videos | worker + fetch interception + bus + `crypto.subtle`        | None while bare has no decoder.                                                      |
| Sticker cache               | worker + fetch interception + persistent `caches`          | Small. The HTTP disk cache already keeps media.                                      |
| Notifications with no tab   | worker + Web Push service                                  | Not possible without a push service.                                                 |
| Cost                        | 6.4 MB script parse and run per worker start, WASM compile | Hypothesis: 30 to 60 MB of heap and 1 to 3 s of CPU per start. To measure in step 1. |

## 4. How other engines do it

- **Chrome.** The browser process owns the registrations (`ServiceWorkerRegistry`, LevelDB in the profile; scripts in a separate store). A worker runs on its own thread in a renderer process (`EmbeddedWorkerInstance`). The browser starts it for an event and stops it after about 30 s idle; one event may run 5 min at most. A navigation looks up the registration by the longest scope prefix in the browser process; a controlled page gets a subresource loader that sends each request to the worker. Chrome skips the worker for a script with no `fetch` handler (hypothesis: for navigations only). `ready` resolves when the registration for the page scope has an active worker.
- **Firefox.** A parent-process `ServiceWorkerManager` keeps registrations in `serviceworker.txt` in the profile. Workers run as remote workers in a content process. Idle timeout `dom.serviceWorkers.idle_timeout` 30 s, extended to 5 min while events are pending (hypothesis: pref names and values to verify).
- **Servo.** A `ServiceWorkerManager` on its own thread starts a `ServiceWorkerGlobalScope` per scope. Registration and `install` work; fetch interception is partial. Hypothesis: to verify in the source.
- **Ladybird.** It has `ServiceWorkerContainer` and the registration job queue, but runs no worker script; `register` rejects (bare copied that). Hypothesis: to verify against the current source.

Common points: registrations live in the profile, apart from any page. A worker starts for an event and stops when idle. A page is controlled or not from the start of its navigation; the choice does not change per request.

## 5. Designs

Each design keeps the same outside rules: the registration map lives in the session (next to `WebStorage`) and in the profile; a page is controlled or not from its navigation; the job queue (register, update, unregister) runs per scope in order.

### S1. One worker per registration, kept alive for the session

A service worker is a worker page (`Of::Worker` with a `ServiceWorkerGlobalScope`) that the session starts at the first event and keeps until the session ends.

- Data: `SwRegistry { by_origin: HashMap<Origin, Vec<Reg>> }`; `Reg { scope, script_url, update_via_cache, installing, waiting, active: Option<SwVersion> }`; `SwVersion { id, state, script: Arc<[u8]>, imported: Vec<(Url, Arc<[u8]>)>, has_fetch: bool, runner: Option<Child> }`.
- Event: a message to the child mailbox; the reply comes back through the port hub.
- Memory: one heap per active worker for the whole session. For WhatsApp, hypothesis 30 to 60 MB kept all the time, for a worker that bare uses for almost nothing.
- CPU: one start per session (1 to 3 s for WhatsApp, hypothesis). No restart cost.
- Fault: simple; no stop logic.

### S2. Start on event, stop when idle (recommended)

Same data as S1. The runner starts for an event (a `message` from a client, a `fetch`, a `notificationclick`, `install`, `activate`) and stops after 30 s with no event and no pending `waitUntil` promise. A hard cap of 5 min per event stops a worker that never settles, as Chrome does.

- Memory: the heap exists only while events flow. A page that never posts to its worker pays only the registry entry (some hundred bytes) after the first 30 s.
- CPU: one start per burst of events. WhatsApp sends a `LOG` or `HEARTBEAT` to the worker during normal use (hypothesis: frequency to measure), so the worker may stay alive. The HTTP disk cache and the in-session JS cache lower the cost of a restart (hypothesis: to measure in step 1).
- `has_fetch`: set at the first run, from the `fetch` listeners at the end of script evaluation (the standard "set of event types to handle"). A worker with no `fetch` listener never starts for a request.

### S3. No worker script; a host-side emulation of `showNotification` and `ready`

`register()` resolves a registration object whose `active` is a stub `ServiceWorker`. `ready` resolves. `showNotification` goes to the notification log. No script runs, so `install`, `message` and `fetch` handlers never run.

- Memory: about zero. CPU: about zero.
- Risk: a page that posts to the controller and waits for a reply (WhatsApp's bus with a 100 ms or 5 s timeout) sees a worker that never answers. A page that relies on `clients.claim()` and `controllerchange` gets inconsistent signals. This gives the page a false platform. bare's stubs act as a browser in which the feature fails (`browser_worker.rs`, `browser_media.rs`); S3 breaks that rule.

### Fetch interception (S1 and S2)

- At navigation, `Session` looks up `SwRegistry::by_origin` (one hash lookup) and then the longest scope prefix. No registration: the page is not controlled and **no later step looks at the registry**. The request path does not change; that is the zero-cost case for all pages but those of a registered origin.
- Controlled page with `has_fetch == false`: the same as not controlled for requests; `controller` is set.
- Controlled page with `has_fetch == true`: `browser_fetch.rs` and the loader send each request of the page to a `FetchEvent` in the worker. `respondWith` resolves a `Response` that is cloned into the page heap. No `respondWith`: the request goes on to the network. The check is one `Option` on the page (`Dom::sw_fetch`).
- Navigation interception (a `fetch` event for the document itself) needs the document load in `browser_session::load_document` to wait for the worker. That is the most invasive part, and WhatsApp needs it only for the call pop-out. It is the last step.

### Threading

A worker page on the page thread (rule D1) runs only between page tasks. That is correct for events. For `fetch` events, the page waits for the worker during its own load; on one thread this works because the pump drives both. Use the thread path (`ThreadWorker`) only when the page is cross-origin isolated, as dedicated workers do. Hypothesis: WhatsApp is cross-origin isolated (its call pop-out handler copies COOP and COEP), so its worker would run on a thread.

### Comparison

| Design | Memory, idle page   | Memory, active   | CPU per event burst    | Honest platform | Work    |
| ------ | ------------------- | ---------------- | ---------------------- | --------------- | ------- |
| S1     | full worker heap    | full worker heap | none after first start | yes             | medium  |
| S2     | registry entry only | full worker heap | one start              | yes             | medium+ |
| S3     | ~0                  | ~0               | ~0                     | no              | small   |

S2 gives the platform with the least memory. S3 is cheapest but breaks the stub rule and does not help WhatsApp (section 3.2).

## 6. Plan

Step 0 is a check, not a build. The owner decides after it whether steps 1 to 8 run.

| Step | Change                                                                                                                                                                                                                                                      | Tests (Chrome is the reference)                                                                                                                   | Cost rows                               |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| 0    | Live WhatsApp run with a message from a second account and the page hidden. Read the console for `muteReason`, `Permission Denied`, `no service worker available`. Fix the real cause of the missing message notification in its own PR.                    | The live check; a unit test per fix.                                                                                                              | `browser_load` rows if the page changes |
| 1    | Measure: run `/sw.js` and its import in a dedicated worker page (no new API) to get heap, parse and run cost of the 6.4 MB script.                                                                                                                          | None (measure only); numbers in the progress file.                                                                                                | none                                    |
| 2    | `SwRegistry` in the session, persisted in `<profile>/sw.bin` (same file rules as `permissions.bin`). Register job: same-origin script, scope rules, `Service-Worker-Allowed`, `updateViaCache`, script MIME check. `getRegistration(s)`, `unregister`.      | `browser_sw_register_tests.rs`: scope and origin errors, MIME errors, resolve order, persistence round trip.                                      | `browser_load` (zero-cost check)        |
| 3    | Runner: `ServiceWorkerGlobalScope` on the worker page, `install` and `activate` with `waitUntil`, `skipWaiting`, `clients.claim`, `ready`, `controller`, `controllerchange`, idle stop (S2).                                                                | `browser_sw_lifecycle_tests.rs`: state order, `ready` timing, `claim` fires `controllerchange`, waiting worker, idle stop with the virtual clock. | `browser_load`, `worker_*` rows         |
| 4    | `postMessage` both ways: `ServiceWorker.postMessage`, `clients.get`, `clients.matchAll`, `Client.postMessage`, `ExtendableMessageEvent` with ports. This is what WhatsApp's bus needs.                                                                      | `browser_sw_message_tests.rs`: round trip with a `MessageChannel`, `source`, start on message.                                                    | `worker_*` rows                         |
| 5    | `registration.showNotification`, `getNotifications`, `notificationclick` and `notificationclose` in the worker, `Client.focus`, `clients.openWindow` (opens a tab in the session). Same log as page notifications. `maxActions` stays 0 (owner question 3). | `browser_sw_notification_tests.rs`: show without grant rejects `TypeError`, tag replace, click starts the worker, `openWindow` same-origin only.  | `browser_load`                          |
| 6    | Persistent shared `caches` (Rust store per origin, on disk in the profile), used by pages and workers. Replace `shims/caches.js`.                                                                                                                           | `browser_caches_store_tests.rs`: share between page and worker, `ignoreSearch`, `Vary`, persistence.                                              | `browser_load`, storage rows            |
| 7    | Subresource `fetch` events for controlled pages with `has_fetch`; `respondWith`, fall-through, `FetchEvent.clientId`, `request.destination`.                                                                                                                | `browser_sw_fetch_tests.rs`: intercepted `fetch`, script and image; no handler means no start; uncontrolled page unchanged.                       | `browser_load`, `resources_load`        |
| 8    | Navigation `fetch` events.                                                                                                                                                                                                                                  | `browser_sw_navigate_tests.rs`.                                                                                                                   | `browser_load`, session rows            |

Out of scope: Push API and `pushsubscriptionchange` (needs a push service), `periodicsync`, Background Sync, navigation preload, update checks on a 24 h timer (an update runs at each registration and navigation only).

For WhatsApp: notifications need step 0 only. Call notifications need steps 2 to 5 and `maxActions >= 2`. Video send needs no step here; it needs a media task. Video playback streaming needs step 7 and a media decoder.

## 7. Open questions for the owner

1. Section 3.2 shows that WhatsApp message notifications do not use a service worker. Do we run step 0 first and hold steps 1 to 8?
2. If we build: S2 (start on event, stop when idle) as recommended, or S1 (kept for the session)?
3. Should `Notification.maxActions` become 2, so that WhatsApp uses the worker path for call notifications? That needs an action field in the notification log and a bare-mcp way to click an action.
4. Video send: open a separate task for the `<video>` preview on a `blob:` URL (section 3.5)? A real decoder is a large dependency; a metadata-only parser (MP4 `moov` for size and duration) plus a blank frame is smaller. That choice needs its own design.
5. Which other journey sites need a service worker? A count over the journey set before step 2 would show if the work pays.

## 8. Not verified

- The live cause of the missing WhatsApp message notification (step 0).
- The heap and CPU cost of the WhatsApp worker script (step 1). The numbers in section 3.6 are hypotheses.
- The engine details marked "hypothesis" in section 4.
- That bare's `<video>` error is the cause of the video send spinner.
