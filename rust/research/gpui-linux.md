# GPUI on Linux Desktop — Primary-Source Research Report

> **Target:** BitChord Linux Desktop (`crates/bitchord-desktop`)  
> **Host verified:** Arch Linux `x86_64`, `rustc 1.100.0` / `cargo 1.100.0`, Intel Core i3-1220P (10C/12T), 7.5 GiB RAM (~3.5 GiB available, `/dev/zram0` 7.5 GiB in-RAM swap), Intel Alder Lake-UP3 GT1 UHD Graphics (`8086:46b3`) with `vulkan-intel 1:26.2.7-1` (`intel_icd.json`, Vulkan 1.4.354), Wayland (`WAYLAND_DISPLAY=cutefish-0`, `XDG_CURRENT_DESKTOP=Cutefish`) + XWayland (`DISPLAY=:1`), `clang 23.1.5` + `lld 23.1.5`.  
> **Sources:** `cargo info`, published crate manifests & source tarballs in crates.io registry (`gpui 0.2.2`, `gpui-component 0.5.1` / `0.10.0`, `gpui-kit 0.10.0`, `gpui-pre 0.3.13`, `gpui-ce 0.3.0`, `gpui-tray 0.3.0`, `gpui-tokio-bridge 0.1.0`, `ksni 0.3.7`, `tray-icon 0.26.1`), official Zed GitHub repository, docs.rs, and local Arch package/driver queries.

---

## 1. Crate Versions & Ecosystem Compatibility (crates.io)

GPUI on crates.io has **three mutually incompatible crate lines**. Mixing crates from different lines produces duplicate-crate type errors (`multiple different versions of crate gpui in the dependency graph`).

| Ecosystem Line | Core Crate(s) (Latest) | Linux GPU Backend | Matching Component Crate | Matching Tokio Bridge | Matching Tray Crate | Notable Capabilities / Limits |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **A. Official Zed (crates.io)** | `gpui = "0.2.2"` (monolithic) | `blade-graphics 0.7.0` (Vulkan) | `gpui-component = "0.5.1"` (**frozen** on `gpui 0.2.2`) | `gpui-tokio-bridge = "0.1.0"` (or vendor ~65 LOC) | `gpui-tray = "0.3.0"` (default feature `gpui`) | Smaller compile graph (`blade` vs `wgpu`); **no** `backdrop_filter`, **no** custom shaders, **no** `QuitMode`, **atlas leak bug** in `BladeAtlas::remove()` (see §5.4). |
| **B. GPUI Kit / `gpui-pre`** | `gpui-pre = "0.3.13"` + `gpui-pre-platform = "0.3.13"` (`zed@125ca86`) | `wgpu 30.0.1` (`gpui-pre-wgpu`) | `gpui-kit = "0.10.0"` / `gpui-component = "0.10.0"` (**pins `gpui-pre = "=0.3.12"`**) | Vendor ~65 LOC (no `gpui-pre` tokio crate on crates.io) | `gpui-tray = "0.3.0"` (`features = ["gpui-kit"]`) | Upstream Zed 2026 architecture: `wgpu` device-loss recovery, `QuitMode::Explicit`, `accesskit_unix`, fixed atlas `deallocate()`, `taffy 0.15`. Still **no** `backdrop_filter` blur or custom shaders. |
| **C. GPUI CE (Community Fork)** | `gpui-ce = "0.3.0"` + `gpui_ce_platform = "0.3.0"` (MSRV 1.95) | `wgpu 29.0.3` (`gpui_ce_wgpu`) | `gpui_ce_components = "0.3.0"` | `gpui_ce_tokio = "0.3.0"` (first-party) | `gpui-tray = "0.3.0"` (`features = ["gpui-ce"]`) | Includes upstream `wgpu` + atlas fix + `QuitMode::Explicit`, **plus** native CSS `backdrop_filter` (`Filter::Blur(Pixels)`), `custom-gpu` shaders, and `wgpu-surfaces`. |

**Primary-source verification:**
- `cargo info gpui` → `gpui 0.2.2` ([crates.io/crates/gpui/0.2.2](https://crates.io/crates/gpui/0.2.2), [docs.rs/gpui/0.2.2](https://docs.rs/gpui/0.2.2), [github.com/zed-industries/zed](https://github.com/zed-industries/zed)).
- `cargo info gpui-tokio` / `gpui_tokio` → **not published on crates.io**. Upstream `crates/gpui_tokio/src/gpui_tokio.rs` lives strictly inside the Zed workspace ([github.com/zed-industries/zed/blob/main/crates/gpui_tokio/src/gpui_tokio.rs](https://github.com/zed-industries/zed/blob/main/crates/gpui_tokio/src/gpui_tokio.rs)).
- `gpui-component-0.10.0/Cargo.toml` pins `gpui = { package = "gpui-pre", version = "=0.3.12" }` (incompatible with `gpui 0.2.2`). `gpui-component-0.5.1/Cargo.toml` is the latest release targeting `gpui = "0.2.2"`.
- `gpui-kit-0.10.0/Cargo.toml` re-exports `gpui-pre = "=0.3.12"` + `gpui-pre-platform = "=0.3.12"` (`features = ["font-kit", "x11", "wayland", "runtime_shaders"]`); when using `gpui-kit`, depend on `gpui-kit` alone and never add a separate `gpui` dependency ([github.com/longbridge/gpui-kit](https://github.com/longbridge/gpui-kit)).
- `gpui-ce 0.3.0` ([crates.io/crates/gpui-ce/0.3.0](https://crates.io/crates/gpui-ce/0.3.0), [github.com/gpui-ce/gpui-ce](https://github.com/gpui-ce/gpui-ce)) requires `rustc >= 1.95` (satisfied by local `1.100.0`) and ships matching `gpui_ce_platform`, `gpui_ce_linux`, `gpui_ce_wgpu`, `gpui_ce_tokio`, and `gpui_ce_components` at `0.3.0`.
- `gpui-tray 0.3.0` ([crates.io/crates/gpui-tray/0.3.0](https://crates.io/crates/gpui-tray/0.3.0), [github.com/domenkozar/gpui-tray](https://github.com/domenkozar/gpui-tray)) supports all three lines via mutually exclusive features (`gpui`, `gpui-kit`, `gpui-ce`).

**Recommendation for BitChord:**
- Choose **`gpui = "0.2.2"`** (with a vendored 65-line `gpui_tokio` bridge and optional `gpui-component = "0.5.1"` or custom minimal primitives) if minimizing cold-build RAM/time and staying on Zed's official crates.io release is top priority—provided you apply the album-art atlas mitigation in §5.4 and render the mesh gradient as a low-res GPU texture (§4.1).
- Choose **`gpui-ce = "0.3.0"`** (`gpui_ce_platform`, `gpui_ce_tokio`) if BitChord requires true in-window frosted-glass `backdrop_filter: Blur`, animated custom WGSL mesh-gradient shaders (`custom-gpu`), `wgpu` suspend/resume device-loss recovery, and the fixed `wgpu_atlas` `deallocate()` allocator—accepting a heavier one-time `wgpu 29.0.3` compile.

---

## 2. Linux Rendering Stack & System Dependencies

### 2.1 Graphics & Windowing Backends
- **`gpui 0.2.2` (`Cargo.toml`):**
  - Default features: `["font-kit", "wayland", "x11", "windows-manifest"]`.
  - GPU backend: `blade-graphics 0.7.0`, `blade-macros 0.3.0`, `blade-util 0.3.0`, plus build-dependency `naga 25.0` (`wgsl-in`). Linux renders strictly via Vulkan.
  - Wayland feature pulls: `wayland-backend 0.3.3` (`client_system`, `dlopen`), `wayland-client 0.31.2`, `wayland-cursor 0.31.1`, `wayland-protocols 0.31.2` (`client`, `staging`, `unstable`), `wayland-protocols-plasma 0.2.0` (`client`), `calloop 0.13.0` + `calloop-wayland-source 0.3.0`, `xkbcommon 0.8.0`, `ashpd 0.11` (`async-std`, `wayland`), `open 5.2.0`.
  - X11 feature pulls: `x11rb 0.13.1` (`xkb`, `randr`, `xinput`, `cursor`, `resource_manager`, `sync`), `as-raw-xcb-connection 1`, `zed-xim 0.4.0-zed`, `x11-clipboard 0.9.3`, `xkbcommon 0.8.0`.
  - Always-on Linux deps in `0.2.2`: `cosmic-text 0.14.0`, `zed-font-kit 0.14.1-zed` (`source-fontconfig-dlopen`), `image 0.25.1` (default features: PNG, JPEG, WebP, GIF, BMP, etc.), `resvg 0.45.0` + `usvg 0.45.0`, `oo7 0.5.0` (`async-std`, `native_crypto` — Linux Secret Service).
- **`gpui-pre 0.3.13` / `gpui-ce 0.3.0` (`gpui-*-linux/Cargo.toml`):**
  - Replaces `blade-graphics` with `wgpu` (`30.0.1` / `29.0.3`, Vulkan primary on Linux), updates `cosmic-text` to `0.19.0`, and adds `accesskit_unix` (AT-SPI accessibility) and `wayland-protocols-wlr` (`layer_shell`).

### 2.2 Runtime Display Server Selection
In `gpui-0.2.2/src/platform.rs` (`guess_compositor()`):
1. `ZED_HEADLESS` set → `HeadlessClient`.
2. `WAYLAND_DISPLAY` set and non-empty (and `wayland` feature enabled) → `WaylandClient`.
3. `DISPLAY` set and non-empty (and `x11` feature enabled) → `X11Client`.
4. Otherwise → `HeadlessClient`.

To force XWayland/X11 fallback for testing on this machine: `WAYLAND_DISPLAY="" cargo run -p bitchord-desktop`.

### 2.3 Local Arch Linux Environment Verification
All native libraries required to compile and run GPUI on Wayland + X11 + Vulkan are **already installed**:

| Native Library / Tool | Local Installed Version (`pkg-config` / `pacman -Q`) | Used By |
| :--- | :--- | :--- |
| `vulkan-intel` / `mesa` | `1:26.2.7-1` (`vulkan.pc 1.4.363`, `/usr/share/vulkan/icd.d/intel_icd.json`) | `blade-graphics` / `wgpu` Vulkan on Alder Lake `8086:46b3` |
| `wayland` | `1.26.0-1` (`wayland-client`, `wayland-cursor`) | `wayland-backend` (`dlopen`), cursor themes |
| `libxkbcommon` & `libxkbcommon-x11` | `1.13.2-1` | Keyboard layout & keymap translation (`xkbcommon 0.8`) |
| `libxcb` | `1.17.0-1` (`xcb`, `xcb-xkb`) | X11 fallback (`x11rb`, `zed-xim`) |
| `fontconfig` & `freetype2` | `2:2.18.3-2` / `2.15.0-1` | `cosmic-text` + `zed-font-kit` (`dlopen`) |
| `clang` & `lld` | `23.1.5-1` | Fast, low-RAM linking (`.cargo/config.toml`) |

*(Note: `vulkan-tools` / `vulkaninfo` and `wayland-utils` / `wayland-info` are not installed; optional if you later want to inspect live Cutefish Wayland globals or Vulkan limits.)*

---

## 3. Bridging Tokio (`bitchord-core` / `reqwest` / `bitchord-audio`) with GPUI

### 3.1 Why a Bridge Is Mandatory
GPUI's Linux runtime (`gpui-0.2.2/src/platform/linux/dispatcher.rs`) runs:
- **1 main UI thread** driven by `calloop::EventLoop` (`ForegroundExecutor`, `cx.spawn`),
- **1 `Timer` thread** driven by `calloop`, and
- **`available_parallelism()` background threads** (`Worker-0..11` = **12 threads** on i3-1220P) pulling `async_task::Runnable` from a `flume::unbounded` queue (`BackgroundExecutor`, `cx.background_executor().spawn`).

There is **no Tokio reactor** on GPUI threads. Awaiting `reqwest`, `tokio::time::sleep`, `tokio::net`, or `tokio-tungstenite` directly inside `cx.spawn` or `cx.background_spawn` panics with *"there is no reactor running"*.

### 3.2 Recommended Pattern: Vendored `gpui_tokio` + Bounded Thread Pool
Do **not** depend on `gpui-tokio-bridge 0.1.0`—inspecting its `src/lib.rs` shows it hardcodes an owned runtime, omits `init_from_handle()`, `handle()`, `spawn_result()`, and `Drop::shutdown_background()`, and restricts `spawn` to `&mut Context<T>` (not `&App` / `C: AppContext`).

Instead, vendor a ~65-line module modeled directly on Zed's `crates/gpui_tokio/src/gpui_tokio.rs` (and `gpui_ce_tokio-0.3.0/src/gpui_tokio.rs`):

```rust
use std::future::Future;
use gpui::{App, AppContext, Global, Task};

struct GlobalTokio {
    owned_runtime: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
}
impl Global for GlobalTokio {}

impl Drop for GlobalTokio {
    fn drop(&mut self) {
        // Avoid blocking GPUI's main thread on shutdown if background sockets linger.
        if let Some(rt) = self.owned_runtime.take() {
            rt.shutdown_background();
        }
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

pub fn init_tokio(cx: &mut App) {
    // CRITICAL on i3-1220P: GPUI already spawns 12 Worker threads; cap Tokio at 2!
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("bitchord-tokio")
        .enable_all()
        .build()
        .expect("Failed to initialize Tokio runtime");
    let handle = rt.handle().clone();
    cx.set_global(GlobalTokio { owned_runtime: Some(rt), handle });
}

pub fn spawn_tokio<C: AppContext, Fut, R>(cx: &C, f: Fut) -> Task<Result<R, tokio::task::JoinError>>
where
    Fut: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    cx.read_global(|tokio: &GlobalTokio, cx| {
        let join_handle = tokio.handle.spawn(f);
        let cancel = AbortOnDrop(join_handle.abort_handle());
        cx.background_spawn(async move {
            let res = join_handle.await;
            drop(cancel); // Completed normally; abort() is a no-op. Dropping Task early aborts Tokio!
            res
        })
    })
}
```

### 3.3 Pushing Events from `bitchord-audio` / WebSockets / MPRIS2 to GPUI Views
- **One-shot RPC (search, fetch lyrics, resolve stream):** Inside a GPUI view event handler, spawn a foreground task with `cx.spawn(async move |this, cx| ...)` that `.await`s `spawn_tokio(cx, ...)` and then updates entity state via `this.update(cx, |view, cx| { ...; cx.notify(); })`. Store the `Task` handle on the view so a new search automatically cancels the stale Tokio request on drop.
- **Continuous streams (playback state, queue changes, Listen Together WebSocket, MPRIS2 commands):** Send compact snapshots over **`async_channel::bounded(64)`** (or `futures::channel::mpsc` / `tokio::sync::watch::Receiver::changed()`). `async_channel::Receiver::recv()` is executor-agnostic and can be `.await`ed directly inside a long-lived `cx.spawn` foreground loop (detached on an `App`-level `PlayerBridge` entity), calling `cx.notify()` only when discrete state changes—never polling on a timer.
- **Audio thread rule:** Never send from the real-time PipeWire/ALSA callback to GPUI directly; publish position + timestamp (`(pos_ms: AtomicU64, updated_at_ns: AtomicU64, playing: AtomicBool)`) lock-free and let the UI interpolate on vsync (§4.4).

---

## 4. Implementing BitChord's Visuals, Lists, Shortcuts & System Tray

### 4.1 Dynamic Color Mesh Gradient Background
**Primary-source shader constraint:** In both `gpui 0.2.2` (`src/platform/blade/shaders.wgsl:106`) and `gpui-pre 0.3.13` (`gpui-pre-wgpu/src/shaders.wgsl:139`), `struct Background` has only `Solid`, **2-stop** `LinearGradient` (`array<LinearColorStop, 2>`, supporting `ColorSpace::Srgb` or `ColorSpace::Oklab`), `PatternSlash` (and `Checkerboard` in `0.3.13`). **GPUI has no built-in radial gradient or mesh gradient shader.**

Two production-grade ways to render BitChord's dynamic artwork mesh gradient:
1. **Low-Res Procedural Mesh Texture (`RenderImage`, works on all GPUI versions, cheapest GPU cost):**
   - When the active track changes, extract 4–5 dominant colors from album art on a background thread and synthesize a **64×64** (16 KiB) or **96×96** (36 KiB) smooth Oklab-interpolated mesh/radial-orb bitmap on CPU (takes <0.5 ms).
   - Convert RGBA → BGRA (`pixel.swap(0, 2)`), wrap in `Arc<RenderImage>`, and paint it stretched behind the player with `img(mesh).size_full().object_fit(ObjectFit::Fill)`. Both `BladeRenderer` (`blade_renderer.rs:367`) and `WgpuRenderer` use **`FilterMode::Linear`**, so the GPU bilinear-upscales 64×64 to full viewport seamlessly with zero banding and negligible VRAM!
   - Crossfade old→new mesh textures over 600 ms via `with_animation`, then call `cx.drop_image(old_mesh, Some(window))` once the crossfade completes.
   - Optionally overlay subtle angled 2-stop Oklab `linear_gradient(angle, stop0, stop1).color_space(ColorSpace::Oklab)` quads via `canvas(prepaint, paint)` + `window.paint_quad(fill(bounds, bg))` for slow ambient motion without re-uploading textures.
2. **Custom WGSL Mesh Shader (`gpui-ce 0.3.0` only):** Enable `features = ["custom-gpu"]` and render an animated multi-orb Oklab mesh fragment shader into `WgpuRenderTarget` (see `gpui_ce_wgpu-0.3.0/examples/custom_gpu.rs`).

### 4.2 Translucent / Frosted Surfaces
- **On `gpui 0.2.2` and `gpui-pre 0.3.13`:** There is **no in-app backdrop blur**. `WindowBackgroundAppearance::Blurred` in `wayland/window.rs:1133` only binds KDE's **`org_kde_kwin_blur_manager`** protocol—on Cutefish, GNOME/Mutter, Sway, or X11 (`x11/window.rs:1380`), it does **not** blur content behind the window or behind child panels.
  - **Practical visual parity:** Because BitChord's frosted cards/sidebar/player bar sit over the *already-blurred* dynamic mesh gradient background (§4.1), semi-transparent dark tinted quads (`bg(hsla(0., 0., 0.08, 0.58))`) + subtle top/edge specular borders (`border_1().border_color(white().opacity(0.08))`) + soft `BoxShadow` deliver crisp glassmorphism at zero blur-pass cost.
- **On `gpui-ce 0.3.0`:** Native CSS `.backdrop_filter(...)` (`Filter::Blur(px(20.0))`) is implemented in `gpui_ce_wgpu/src/wgpu_renderer/filters.rs` via multi-pass downsampled Gaussian blur, working identically on Cutefish, GNOME, Sway, and X11. (Keep blurred regions modest on Intel UHD integrated graphics to preserve 60fps bandwidth.)

### 4.3 Album Artwork Loading, Decoding & Caching (`img()`)
Key facts verified from `gpui-0.2.2/src/elements/img.rs` and `image_cache.rs`:
- **Default HTTP client is `NullHttpClient` (`app.rs:137`):** Passing an `https://` URL directly to `img(...)` fails unless `Application::new().with_http_client(...)` is installed. For BitChord, **bypass GPUI's HTTP fetcher**: download, disk-cache, and downscale artwork in `bitchord-core` (Tokio + `reqwest` + `image`), then feed `ImageSource::Render(Arc<RenderImage>)` or a custom `ImageCache` implementation.
- **Channel-order trap (BGRA!):** `ImageAssetLoader` swaps every pixel's R and B channels (`pixel.swap(0, 2)` in `img.rs:685`, or `swap_rgba_pa_to_bgra` for premultiplied buffers) before calling `RenderImage::new(...)` because `BladeAtlas` uploads polychrome images as `TextureFormat::Bgra8Unorm`. **If you build `RenderImage::new()` from `image::RgbaImage` without swapping R↔B, all album art renders with inverted red/blue channels.**
- **Do NOT use `RetainAllImageCache`:** It never evicts entries until `.clear()`/`.remove()` or entity release (`image_cache.rs`). Implement a bounded LRU `ImageCache` (see `gpui-0.2.2/examples/image_gallery.rs:SimpleLruCache`) capped at e.g. **64–96 entries** that calls `cx.drop_image(image, Some(window))` on eviction and `cx.drop_image(image, None)` in `cx.on_release(...)`.
- **Downscale before upload:** Never upload raw 1200×1200–3000×3000 covers to GPUI; resize to target device pixels (`128×128` thumbnails, `512×512` Now Playing) before constructing `RenderImage`.

### 4.4 Smooth 60fps Word-Synced Lyrics & Virtualized Track Lists
- **Track lists / Queues / Search results (fixed row height):** Use **`uniform_list("tracks", count, cx.processor(|this, range, window, cx| ...))`** (`elements/uniform_list.rs`). It measures only item 0 and renders only visible rows. Attach `UniformListScrollHandle` via `.track_scroll(handle)` and jump to the playing track with `handle.scroll_to_item_strict(ix, ScrollStrategy::Center)`.
- **Word-synced lyrics (variable wrapped height + optional translations):** **`uniform_list` breaks if lyric lines wrap to 2–3 lines or show translations.** Use GPUI's variable-height virtualized **`list(list_state, ...)`** (`elements/list.rs`, backed by a `SumTree`), initializing `ListState::new(line_count, ListAlignment::Top, px(200.))` and scrolling smoothly with `list_state.scroll_to(ListOffset { item_ix, offset_in_item })`.
- **60fps word highlighting without wasting CPU/GPU:**
  - `with_animation(id, Animation::new(dur).with_easing(ease_in_out), |el, delta| ...)` (`elements/animation.rs:148`) works by calling `window.request_animation_frame()` on every layout pass until `delta >= 1.0`.
  - For active word-synced karaoke progress (and animated smooth scroll interpolation), call `window.request_animation_frame()` inside the lyrics view's `render()` **only when `is_playing && lyrics_visible`** (ensuring 0% idle GPU usage when paused or viewing library tabs).
  - Compute exact playback time in `render()` via `base_position + last_sync_instant.elapsed()` (no 16 ms Tokio timer spamming `cx.notify()`!). Render active-word fill/glow either by splitting styled text runs (`StyledText` / inline flex-wrap word spans with animated opacity/scale) or custom `canvas` gradient sweep clipped to the word bounds.

### 4.5 Keyboard Shortcuts & Focus Scoping
From `gpui-0.2.2/src/keymap/binding.rs` and `examples/on_window_close_quit.rs`:
- Declare actions with `actions!(bitchord, [PlayPause, NextTrack, PrevTrack, SeekForward, SeekBack, FocusSearch, CloseWindow, Quit]);`.
- Register bindings in `App` startup with `cx.bind_keys([KeyBinding::new("space", PlayPause, Some("Player && !TextInput")), KeyBinding::new("ctrl-f", FocusSearch, None), KeyBinding::new("ctrl-q", Quit, None)])`.
- Always attach `.key_context("Player")` on the root view and `.key_context("TextInput")` on search/filter inputs so single-key shortcuts (`space`, `j`/`k`, `m`, `l`) never hijack text typing. Note: on Linux use `ctrl-` (or `secondary-`), not macOS `cmd-`.

### 4.6 System Tray & Close-to-Tray on Linux
- **Crate choice:** Use **`gpui-tray = "0.3.0"`** (pure-Rust `zbus 6` talking directly to `org.kde.StatusNotifierItem` + `com.canonical.dbusmenu`, zero GTK3/`libappindicator` dependency, dispatches native GPUI actions!). Alternative if decoupled from GPUI types: `ksni = "0.3.7"` (`tokio` feature enabled by default). Avoid `tray-icon 0.26.1` with default features because it pulls `libappindicator` + GTK3 (`muda-gtk3`).
- **`cx.hide()` trap:** `gpui-0.2.2/src/platform/linux/platform.rs:235` explicitly logs `"hide is not implemented on Linux, ignoring the call"`.
- **Close-to-tray lifecycle:**
  1. Store all playback/audio/MPRIS2/queue state in **`App` globals/entities**, never owned solely by the window's root view.
  2. Wire `window.on_window_should_close(cx, ...)` (connected to Wayland `xdg_toplevel::Event::Close` and X11 `WM_DELETE_WINDOW`). When "close to tray" is enabled, let the window close (or call `window.remove_window()`), while keeping the `App` alive with 0 windows:
     - On **`gpui 0.2.2`**: the app naturally stays alive unless `on_window_closed` calls `cx.quit()`.
     - On **`gpui-pre 0.3.x` / `gpui-ce 0.3.x`**: `QuitMode::Default` quits on last window closed on Linux—you **must** call `.with_quit_mode(QuitMode::Explicit)` on `gpui_platform::application()` (`gpui-tray/examples/tray.rs:41`).
  3. On tray click / "Show BitChord", call `cx.open_window(...)` to mount a fresh root view onto the running `App` state. **Bonus:** closing the window calls `BladeRenderer::destroy()` / `WgpuRenderer::destroy()`, immediately freeing swapchain, MSAA path buffers, and GPU atlas memory while minimized to tray!

---

## 5. Semantic Traps & Linux Caveats

### 5.1 Wayland CSD / Window Decorations (`cutefish-0`, GNOME, KWin)
- In `gpui-0.2.2/src/platform/linux/wayland/window.rs:176`, `WaylandWindowState` initializes with `decorations: WindowDecorations::Client`.
- If the compositor exposes `zxdg_decoration_manager_v1`, GPUI requests a `zxdg_toplevel_decoration_v1` and switches to `Server` or `Client` on `Configure`. If the compositor does **not** expose `zxdg_decoration_manager_v1` (e.g., GNOME/Mutter, or compositors enforcing CSD), `decoration` is `None` and **the window has no OS titlebar or resize borders at all**.
- **Action:** Check `window.window_decorations()` in the root view. When `Decorations::Client { tiling }`, render a custom BitChord titlebar with drag (`window.start_window_move()`), min/max/close buttons, and edge resize handles (`window.start_window_resize(edge)`), plus `window.set_client_inset(...)`. Also note `wayland/window.rs:1122`: whenever `decorations == Client` (or `background_appearance != Opaque`), GPUI clears `wl_surface::set_opaque_region(None)` so the compositor blends the entire surface with alpha.

### 5.2 IME & Unicode Text Layout (`CosmicTextSystem`)
- **Wayland IME single-byte quirk (`wayland/client.rs:1438`):** In `zwp_text_input_v3::Event::CommitString`, GPUI reroutes 1-byte ASCII commits (`commit_text.len() == 1`) as synthetic `KeyDown` events (a Zed Vim-mode workaround), while multi-byte UTF-8 commits (CJK, accented chars, emoji) go through `ImeInput::InsertText`. Custom text inputs must handle both `KeyDown` and `EntityInputHandler::replace_text_in_range`!
- **X11 IME:** Uses `zed-xim` with `InputStyle::PREEDIT_CALLBACKS` (`x11/xim_handler.rs:51`). X11 DPI scaling can be overridden via `GPUI_X11_SCALE_FACTOR=<float|randr>` (`x11/client.rs:79`).
- **Synchronous font DB scan & fallback (`linux/text_system.rs:55,216`):** `CosmicTextSystem::new()` calls `FontSystem::new()` synchronously on startup (`// todo(linux) make font loading non-blocking`) and falls back to `"IBM Plex Sans"`. For BitChord's multilingual song titles and lyrics (Latin, Japanese, Korean, Chinese, Cyrillic, Emoji), explicitly load a bundled sans font via `cx.text_system().add_fonts(...)` and verify system fontconfig fallback covers CJK (`noto-fonts-cjk`) and color emoji (`noto-fonts-emoji`).

### 5.3 Intel Alder Lake ANV Vulkan Quirks (`8086:46b3`)
- **Dual Intel ICDs on Arch:** `/usr/share/vulkan/icd.d/` contains both `intel_icd.json` (ANV — correct for Alder Lake `8086:46b3`) and `intel_hasvk_icd.json` (legacy Haswell/Broadwell). Vulkan's loader selects ANV automatically; if any multi-GPU/loader mismatch ever occurs, pin with `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/intel_icd.json` or GPUI's built-in `ZED_DEVICE_ID=46b3` (`blade_context.rs:13`, `wgpu_context.rs:328`).
- **Intel Xe buffer alignment:** Already handled inside `gpui 0.2.2` (`blade_atlas.rs:48` and `blade_renderer.rs:362` align `BufferBelt` to 64 bytes for Intel Xe's `optimalBufferCopyOffsetAlignment` / `minStorageBufferOffsetAlignment`).
- **Path MSAA VRAM overhead:** `BladeRenderer` (`blade_renderer.rs:841`) defaults `path_sample_count` to `4` (4× MSAA), allocating both a full-window intermediate texture AND a full-window 4× MSAA texture on every window resize (`5×` framebuffer VRAM!). Setting `ZED_PATH_SAMPLE_COUNT=1` eliminates the MSAA texture if BitChord doesn't rely heavily on complex vector paths.
- **Suspend/resume device loss:** `gpui 0.2.2` (`blade_renderer.rs:418`) has **no device-loss recovery**—if a Vulkan driver reset occurs across laptop sleep, `wait_for_gpu()` logs `"GPU hung"` and spins forever. `wgpu`-based GPUI (`gpui-pre 0.3.13` / `gpui-ce 0.3.0`) installs `DeviceErrorState::device_lost()` recovery (`wgpu_context.rs:35`).

### 5.4 GPU Texture & Shared-VRAM Limits (7.5 GiB Shared-Memory Laptop)
Intel UHD Graphics allocates Vulkan textures directly from the host's **7.5 GiB unified system RAM** (~3.5 GiB free, with `/dev/zram0` also backed by RAM).

- **CRITICAL `gpui 0.2.2` `BladeAtlas` Leak (`src/platform/blade/blade_atlas.rs:95-115`):**
  - `BladeAtlas` allocates `1024×1024` `Bgra8Unorm` Polychrome pages (**4 MiB each**), or a dedicated oversized texture whenever `min_size > 1024` (a raw 3000×3000 cover = **36 MiB**!).
  - When `cx.drop_image()` calls `BladeAtlas::remove()`, it decrements `live_atlas_keys` and destroys the Vulkan texture **only when `live_atlas_keys == 0`**. It **never calls `etagere::BucketedAtlasAllocator::deallocate()`**!
  - **Consequence:** If a single never-evicted color emoji or static raster UI icon shares a `1024×1024` Polychrome atlas page with churned album thumbnails, that page's `live_atlas_keys` never reaches 0, its tile rectangles are never recycled, and browsing a large library leaks 4 MiB pages indefinitely until OOM.
  - **Mitigations:** (1) On `gpui 0.2.2`, render all UI icons as monochrome SVGs (`svg()`, stored in the separate `Monochrome` `R8Unorm` atlas) so they never share pages with `Polychrome` album art; downscale covers ≤512 px; cap LRU at 64–96 entries; and periodically flush or rely on close-to-tray window recreation. (2) On `gpui-pre 0.3.13` / `gpui-ce 0.3.0`, **`wgpu_atlas.rs` explicitly calls `texture.allocator.deallocate(tile.tile_id.into())`** and checks `device.limits().max_texture_dimension_2d`, completely eliminating the leak.

---

## 6. Cargo Build & RAM-Saving Recommendations (7.5 GiB RAM + zram)

Building on **ADR-007** (`.cargo/config.toml` already sets `jobs = 4`, `linker = "clang"`, `rustflags = ["-C", "link-arg=-fuse-ld=lld"]`, and `bitchord-desktop` is excluded from workspace `default-members`):

1. **Cap linker threads & cold-build parallelism for `bitchord-desktop`:**
   - Because `/dev/zram0` compresses swap inside physical RAM, peak compilation spikes >4 GiB cause aggressive desktop lag or OOM kills.
   - Add `"-C", "link-arg=-Wl,--threads=2"` to `.cargo/config.toml` `rustflags` so `lld` does not allocate 12 concurrent thread arenas while linking `gpui` + `naga` + `wgpu`/`blade`.
   - For the first cold build of `bitchord-desktop`, invoke `cargo build -p bitchord-desktop -j 2` (or `-j 3`) rather than `-j 4`; incremental edits to `bitchord-desktop` itself stay fast at `jobs = 4`.
2. **Surgical `Cargo.toml` dev profile tuning (keep debug UI at 60fps without high LLVM RAM):**
   ```toml
   [profile.dev]
   debug = "line-tables-only"
   incremental = true

   [profile.dev.package."*"]
   debug = 0        # Massive RAM & disk savings on gpui / naga / wgpu / cosmic-text

   # Optimize ONLY frame-critical image/layout/audio crates in dev; keep gpui at opt-level = 0 or 1.
   [profile.dev.package.image]
   opt-level = 2
   [profile.dev.package.resvg]
   opt-level = 2
   [profile.dev.package.taffy]
   opt-level = 2
   [profile.dev.package.cosmic-text]
   opt-level = 2
   ```
   *(Compiling `gpui` / `wgpu` itself at `opt-level = 2` in dev spikes LLVM memory; leaving `gpui` at `opt-level = 0` with `debug = 0` while optimizing `image`/`taffy`/`cosmic-text` keeps peak RAM under ~2.5 GiB while keeping scrolling and artwork decoding smooth.)*
3. **Feature-gate X11 during local Wayland-only dev iteration:**
   - Expose `default = ["wayland", "x11"]` on `bitchord-desktop` forwarding to GPUI's `wayland` / `x11` features. During daily Wayland dev, build with `--no-default-features --features wayland` to skip compiling `x11rb`, `zed-xim`, and `x11-clipboard` (re-enable both for release builds).
4. **Never enable heavy optional component features:**
   - If adopting `gpui-component` / `gpui-kit` / `gpui_ce_components`, keep `default-features = false` and **never** enable `tree-sitter-languages` (25+ C/C++ grammars) or `webview` (`wry` / WebKitGTK).
5. **Fast edit loop & release profile:**
   - Prefer `cargo check -p bitchord-desktop` during UI iteration (skips LLVM codegen + linking completely).
   - For release builds, keep ADR-007's `lto = "thin"`, `codegen-units = 4`, `strip = "symbols"`—avoid `lto = "fat"` or `codegen-units = 1`, which easily exceed 7.5 GiB RAM when linking GPUI + Tokio + audio codecs.
