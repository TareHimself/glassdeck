// Glassdeck's web renderer on the keyboard: WPE WebKit inside a bubblewrap sandbox of our own.
// WebKit's sandbox refuses to pass devices through, and the video decoder (MPP) is a device, so our sandbox replaces
// it: no network, no root, read-only system files, and only the decoder, GPU and page folder visible.
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <glib-unix.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wpe/fdo.h>
#include <wpe/unstable/fdo-dmabuf.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <wpe/webkit.h>

#include "compositor.h"
#include "background.h"

#define PANEL_WIDTH 1920
#define PANEL_HEIGHT 1080
#define INSIDE_FLAG "--inside-sandbox"
#define SANDBOXED_SELF "/app/glassdeck-local-web"
#define SANDBOXED_BACKGROUND "/app/wallpaper.deck"
#define BACKGROUND_ORIGIN "background://wallpaper/"
#define BACKGROUND_FLAG "--background="
#define DISPLAY_DRIVER "rockchip"
#define MAX_CARDS 16

static const char* const DEVICES[] = {"/dev/mpp_service", "/dev/dma_heap", "/dev/dri"};
// MPP identifies the chip from the device tree (/proc/device-tree links here); libdrm finds GPUs through /sys.
static const char* const READ_ONLY_SYSTEM[] = {
    "/usr", "/etc/ld.so.cache", "/etc/fonts", "/sys/firmware/devicetree/base", "/sys/dev/char", "/sys/devices/platform",
    "/sys/devices/system/cpu",
};

// Headless for tests; display drives the screen for previews while no receiver holds it; compositor is the receiver.
enum output_kind {
  OUTPUT_HEADLESS,
  OUTPUT_DISPLAY,
  OUTPUT_COMPOSITOR,
};

// What to show: a page by URI (for tests), or a web background .deck, served to WebKit as background://.
enum content_kind {
  CONTENT_NONE,
  CONTENT_URI,
  CONTENT_BACKGROUND,
};

struct options {
  enum content_kind content;
  const char* location;
  guint seconds;
  gboolean inside;
  enum output_kind output;
  int compositor_fd;
};

static int usage(void) {
  fprintf(stderr, "usage: glassdeck-local-web [--seconds=N] [--display | --compositor-fd=N] (--background=PATH | <uri>)\n");
  return 2;
}

static gboolean parse_options(int argc, char** argv, struct options* o) {
  for (int i = 1; i < argc; i++) {
    if (strcmp(argv[i], INSIDE_FLAG) == 0) {
      o->inside = TRUE;
    } else if (g_str_has_prefix(argv[i], BACKGROUND_FLAG) && o->content == CONTENT_NONE) {
      o->content = CONTENT_BACKGROUND;
      o->location = argv[i] + strlen(BACKGROUND_FLAG);
    } else if (strcmp(argv[i], "--display") == 0 && o->output == OUTPUT_HEADLESS) {
      o->output = OUTPUT_DISPLAY;
    } else if (strncmp(argv[i], "--compositor-fd=", 16) == 0 && o->output == OUTPUT_HEADLESS) {
      o->output = OUTPUT_COMPOSITOR;
      o->compositor_fd = (int)strtol(argv[i] + 16, NULL, 10);
    } else if (strncmp(argv[i], "--seconds=", 10) == 0) {
      o->seconds = (guint)strtoul(argv[i] + 10, NULL, 10);
    } else if (argv[i][0] == '-' || o->content != CONTENT_NONE) {
      return FALSE;
    } else {
      o->content = CONTENT_URI;
      o->location = argv[i];
    }
  }
  return o->content != CONTENT_NONE;
}

static void add(GPtrArray* args, const char* first, ...) {
  va_list more;
  va_start(more, first);
  for (const char* a = first; a; a = va_arg(more, const char*)) g_ptr_array_add(args, g_strdup(a));
  va_end(more);
}

// The folder a file:// page loads from, shown read-only; other schemes need nothing from the filesystem.
static char* content_dir(const char* uri) {
  if (!g_str_has_prefix(uri, "file://")) return NULL;
  g_autofree char* path = g_filename_from_uri(uri, NULL, NULL);
  return path ? g_path_get_dirname(path) : NULL;
}

static int exec_in_sandbox(const struct options* o) {
  if (getuid() == 0) {
    fprintf(stderr, "glassdeck-local-web: refusing to run pages as root; run as the glassdeck-local-web user\n");
    return 1;
  }
  for (size_t i = 0; i < G_N_ELEMENTS(DEVICES); i++) {
    int needed = g_file_test(DEVICES[i], G_FILE_TEST_IS_DIR) ? R_OK | X_OK : R_OK | W_OK;
    if (access(DEVICES[i], needed) != 0) {
      fprintf(stderr, "glassdeck-local-web: cannot open %s (%s); is this user in the video group?\n", DEVICES[i], strerror(errno));
      return 1;
    }
  }
  g_autofree char* self = g_file_read_link("/proc/self/exe", NULL);
  if (!self) return fprintf(stderr, "glassdeck-local-web: cannot find its own executable\n"), 1;

  g_autoptr(GPtrArray) args = g_ptr_array_new_with_free_func(g_free);
  add(args, "bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL", NULL);
  for (size_t i = 0; i < G_N_ELEMENTS(READ_ONLY_SYSTEM); i++) add(args, "--ro-bind-try", READ_ONLY_SYSTEM[i], READ_ONLY_SYSTEM[i], NULL);
  add(args, "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/sbin", "/sbin", NULL);
  add(args, "--proc", "/proc", "--dev", "/dev", NULL);
  for (size_t i = 0; i < G_N_ELEMENTS(DEVICES); i++) add(args, "--dev-bind", DEVICES[i], DEVICES[i], NULL);
  add(args, "--tmpfs", "/tmp", "--tmpfs", "/run", "--dir", "/tmp/runtime", NULL);
  g_autofree char* content = NULL;
  g_autofree char* background_flag = NULL;
  if (o->content == CONTENT_BACKGROUND) {
    content = realpath(o->location, NULL);
    if (!content) return fprintf(stderr, "glassdeck-local-web: cannot find %s: %s\n", o->location, strerror(errno)), 1;
    add(args, "--ro-bind", content, SANDBOXED_BACKGROUND, NULL);
    // Media in the page reaches the background through our GStreamer source, which reads this
    add(args, "--setenv", "GLASSDECK_BACKGROUND", SANDBOXED_BACKGROUND, "--setenv", "GST_PLUGIN_PATH", GST_BACKGROUND_PLUGIN_DIR, NULL);
    background_flag = g_strdup(BACKGROUND_FLAG SANDBOXED_BACKGROUND);
  } else {
    content = content_dir(o->location);
    if (content) add(args, "--ro-bind", content, content, NULL);
  }
  add(args, "--ro-bind", self, SANDBOXED_SELF, NULL);
  add(args, "--setenv", "HOME", "/tmp", "--setenv", "XDG_RUNTIME_DIR", "/tmp/runtime", NULL);
  add(args, "--unsetenv", "DBUS_SESSION_BUS_ADDRESS", "--unsetenv", "WAYLAND_DISPLAY", "--unsetenv", "DISPLAY", NULL);
  // WebKit's own sandbox cannot nest in ours or pass the decoder through; ours stands in for it.
  add(args, "--setenv", "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1", NULL);
  add(args, "--", SANDBOXED_SELF, INSIDE_FLAG, NULL);
  if (o->output == OUTPUT_DISPLAY) add(args, "--display", NULL);
  if (o->output == OUTPUT_COMPOSITOR) {
    g_autofree char* fd = g_strdup_printf("--compositor-fd=%d", o->compositor_fd);
    add(args, fd, NULL);
  }
  if (o->seconds) {
    g_autofree char* seconds = g_strdup_printf("--seconds=%u", o->seconds);
    add(args, seconds, NULL);
  }
  add(args, background_flag ? background_flag : o->location, NULL);
  g_ptr_array_add(args, NULL);

  execvp("bwrap", (char**)args->pdata);
  fprintf(stderr, "glassdeck-local-web: cannot start bwrap: %s\n", strerror(errno));
  return 1;
}

// The screen, when previewing: one frame on screen, at most one more waiting for the next vblank.
struct display {
  int fd;
  uint32_t crtc;
  uint32_t connector;
  drmModeModeInfo mode;
  gboolean mode_set;
  struct wpe_dmabuf_pool_entry* on_screen;
  struct wpe_dmabuf_pool_entry* flipping;
};

// The receiver shows our frames: each buffer is registered once, then presented by id.
struct compositor_output {
  struct compositor* socket;
  // Presented and not yet released: the one on screen, plus the newest waiting for a flip.
  guint outstanding;
  gboolean awaiting_complete;
  gint64 last_complete_us;
  guint complete_timer;
};

// Three buffers in turn: WebKit draws the next frame while one is on screen and the newest waits for its flip.
#define MAX_PRESENTED 2
// The receiver drops superseded frames, so frames beyond the panel's refresh would only burn CPU and GPU.
#define REFRESH_INTERVAL_US (G_USEC_PER_SEC / 60)

struct output {
  enum output_kind kind;
  union {
    struct display display;
    struct compositor_output compositor;
  };
};

struct renderer {
  struct gbm_device* gbm;
  struct wpe_view_backend_dmabuf_pool_fdo* pool;
  GMainLoop* loop;
  // WPEBackend-fdo 1.16 never calls destroy_entry, so the renderer frees what it allocated.
  GPtrArray* entries;
  struct output output;
  guint frames;
  gboolean stopping;
  int exit_code;
};

struct frame_buffer {
  struct gbm_bo* bo;
  int fd;
  // Set once known to the display (preview) or the compositor
  uint32_t fb_id;
  uint32_t compositor_id;
};

static void free_entry(struct renderer* r, struct wpe_dmabuf_pool_entry* entry) {
  struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
  wpe_dmabuf_pool_entry_destroy(entry);
  if (r->output.kind == OUTPUT_DISPLAY && fb->fb_id) drmModeRmFB(r->output.display.fd, fb->fb_id);
  close(fb->fd);
  gbm_bo_destroy(fb->bo);
  g_free(fb);
}

static void stop_with_error(struct renderer* r, const char* message) {
  fprintf(stderr, "glassdeck-local-web: %s: %s\n", message, strerror(errno));
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
}

// WebKit renders into dma-bufs we allocate; this is what the receiver's compositor will put on a plane.
static struct wpe_dmabuf_pool_entry* on_create_entry(void* data) {
  struct renderer* r = data;
  struct gbm_bo* bo = gbm_bo_create(r->gbm, PANEL_WIDTH, PANEL_HEIGHT, GBM_FORMAT_XRGB8888,
                                    GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
  if (!bo) {
    fprintf(stderr, "glassdeck-local-web: cannot allocate a frame buffer\n");
    return NULL;
  }
  struct frame_buffer* fb = g_new0(struct frame_buffer, 1);
  fb->bo = bo;
  fb->fd = gbm_bo_get_fd(bo);
  struct wpe_dmabuf_pool_entry_init init = {
      .width = PANEL_WIDTH,
      .height = PANEL_HEIGHT,
      .format = gbm_bo_get_format(bo),
      .num_planes = 1,
      .fds = {fb->fd, -1, -1, -1},
      .strides = {gbm_bo_get_stride(bo)},
      .modifiers = {gbm_bo_get_modifier(bo)},
  };
  if (r->output.kind == OUTPUT_COMPOSITOR) {
    struct buffer_layout layout = {
        .width = PANEL_WIDTH,
        .height = PANEL_HEIGHT,
        .fourcc = gbm_bo_get_format(bo),
        .stride = gbm_bo_get_stride(bo),
        .offset = 0,
        .modifier = gbm_bo_get_modifier(bo),
    };
    g_autoptr(GError) error = NULL;
    if (!compositor_register(r->output.compositor.socket, fb->fd, &layout, &fb->compositor_id, &error)) {
      // Without a buffer the compositor can show there is nothing to draw into; the receiver logs our status.
      fprintf(stderr, "glassdeck-local-web: %s\n", error->message);
      exit(1);
    }
  }
  struct wpe_dmabuf_pool_entry* entry = wpe_dmabuf_pool_entry_create(&init);
  wpe_dmabuf_pool_entry_set_user_data(entry, fb);
  g_ptr_array_add(r->entries, entry);
  return entry;
}

static void on_destroy_entry(void* data, struct wpe_dmabuf_pool_entry* entry) {
  struct renderer* r = data;
  if (g_ptr_array_remove(r->entries, entry)) free_entry(r, entry);
}

static gboolean import_frame(struct display* d, struct frame_buffer* fb) {
  if (fb->fb_id) return TRUE;
  uint32_t handle;
  if (drmPrimeFDToHandle(d->fd, fb->fd, &handle) != 0) return FALSE;
  uint32_t handles[4] = {handle};
  uint32_t pitches[4] = {gbm_bo_get_stride(fb->bo)};
  uint32_t offsets[4] = {0};
  uint64_t modifiers[4] = {gbm_bo_get_modifier(fb->bo)};
  int added = drmModeAddFB2WithModifiers(d->fd, PANEL_WIDTH, PANEL_HEIGHT, gbm_bo_get_format(fb->bo), handles, pitches,
                                         offsets, modifiers, &fb->fb_id, DRM_MODE_FB_MODIFIERS);
  drmCloseBufferHandle(d->fd, handle);
  return added == 0;
}

static void show(struct renderer* r, struct wpe_dmabuf_pool_entry* entry) {
  struct display* d = &r->output.display;
  struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
  if (!import_frame(d, fb)) return stop_with_error(r, "the display refused the frame buffer");
  if (!d->mode_set) {
    if (drmModeSetCrtc(d->fd, d->crtc, fb->fb_id, 0, 0, &d->connector, 1, &d->mode) != 0)
      return stop_with_error(r, "cannot set the display mode");
    d->mode_set = TRUE;
    d->on_screen = entry;
    wpe_view_backend_dmabuf_pool_fdo_dispatch_frame_complete(r->pool);
    return;
  }
  if (drmModePageFlip(d->fd, d->crtc, fb->fb_id, DRM_MODE_PAGE_FLIP_EVENT, r) != 0)
    return stop_with_error(r, "cannot flip to the new frame");
  d->flipping = entry;
}

static void on_flip(G_GNUC_UNUSED int fd, G_GNUC_UNUSED unsigned sequence, G_GNUC_UNUSED unsigned seconds,
                    G_GNUC_UNUSED unsigned microseconds, void* data) {
  struct renderer* r = data;
  struct display* d = &r->output.display;
  wpe_view_backend_dmabuf_pool_fdo_dispatch_release_entry(r->pool, d->on_screen);
  d->on_screen = d->flipping;
  d->flipping = NULL;
  wpe_view_backend_dmabuf_pool_fdo_dispatch_frame_complete(r->pool);
}

static gboolean on_display_event(int fd, G_GNUC_UNUSED GIOCondition condition, G_GNUC_UNUSED gpointer data) {
  drmEventContext events = {.version = 2, .page_flip_handler = on_flip};
  drmHandleEvent(fd, &events);
  return G_SOURCE_CONTINUE;
}

static void complete_frame(struct renderer* r) {
  r->output.compositor.last_complete_us = g_get_monotonic_time();
  wpe_view_backend_dmabuf_pool_fdo_dispatch_frame_complete(r->pool);
}

static gboolean on_complete_timer(gpointer data) {
  struct renderer* r = data;
  r->output.compositor.complete_timer = 0;
  complete_frame(r);
  return G_SOURCE_REMOVE;
}

// Lets WebKit start its next frame now, or at the next refresh interval if the last one began less than one ago.
static void request_next_frame(struct renderer* r) {
  struct compositor_output* c = &r->output.compositor;
  if (c->complete_timer) return;
  gint64 wait_us = c->last_complete_us + REFRESH_INTERVAL_US - g_get_monotonic_time();
  if (wait_us <= 0) return complete_frame(r);
  c->complete_timer = g_timeout_add_full(G_PRIORITY_HIGH, (guint)((wait_us + 999) / 1000), on_complete_timer, r, NULL);
}

static void present_to_compositor(struct renderer* r, struct wpe_dmabuf_pool_entry* entry) {
  struct compositor_output* c = &r->output.compositor;
  struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
  g_autoptr(GError) error = NULL;
  if (!compositor_present(c->socket, fb->compositor_id, &error)) {
    fprintf(stderr, "glassdeck-local-web: %s\n", error->message);
    r->exit_code = 1;
    g_main_loop_quit(r->loop);
    return;
  }
  if (++c->outstanding <= MAX_PRESENTED) {
    request_next_frame(r);
  } else {
    c->awaiting_complete = TRUE;
  }
}

static void on_compositor_released(void* data, uint32_t id) {
  struct renderer* r = data;
  struct compositor_output* c = &r->output.compositor;
  for (guint i = 0; i < r->entries->len; i++) {
    struct wpe_dmabuf_pool_entry* entry = g_ptr_array_index(r->entries, i);
    struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
    if (fb->compositor_id != id) continue;
    wpe_view_backend_dmabuf_pool_fdo_dispatch_release_entry(r->pool, entry);
    c->outstanding--;
    if (c->awaiting_complete) {
      c->awaiting_complete = FALSE;
      request_next_frame(r);
    }
    return;
  }
  fprintf(stderr, "glassdeck-local-web: the compositor released buffer %u, which it never registered\n", id);
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
}

static gboolean on_compositor_readable(G_GNUC_UNUSED int fd, G_GNUC_UNUSED GIOCondition condition, gpointer data) {
  struct renderer* r = data;
  g_autoptr(GError) error = NULL;
  if (compositor_dispatch(r->output.compositor.socket, &error)) return G_SOURCE_CONTINUE;
  fprintf(stderr, "glassdeck-local-web: %s\n", error->message);
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
  return G_SOURCE_REMOVE;
}

// A connected output that can show the panel's size, preferring 60 Hz.
static gboolean find_output(int fd, struct display* d) {
  drmModeRes* resources = drmModeGetResources(fd);
  if (!resources) return FALSE;
  gboolean found = FALSE;
  for (int i = 0; i < resources->count_connectors && !found; i++) {
    drmModeConnector* connector = drmModeGetConnector(fd, resources->connectors[i]);
    if (!connector) continue;
    const drmModeModeInfo* best = NULL;
    for (int m = 0; connector->connection == DRM_MODE_CONNECTED && m < connector->count_modes; m++) {
      const drmModeModeInfo* mode = &connector->modes[m];
      if (mode->hdisplay != PANEL_WIDTH || mode->vdisplay != PANEL_HEIGHT) continue;
      if (!best || (mode->vrefresh == 60 && best->vrefresh != 60)) best = mode;
    }
    uint32_t encoder_id = connector->encoder_id ? connector->encoder_id : (connector->count_encoders ? connector->encoders[0] : 0);
    drmModeEncoder* encoder = best && encoder_id ? drmModeGetEncoder(fd, encoder_id) : NULL;
    if (encoder) {
      uint32_t crtc = encoder->crtc_id;
      for (int k = 0; !crtc && k < resources->count_crtcs; k++)
        if (encoder->possible_crtcs & (1u << k)) crtc = resources->crtcs[k];
      if (crtc) {
        *d = (struct display){.fd = fd, .crtc = crtc, .connector = connector->connector_id, .mode = *best};
        found = TRUE;
      }
      drmModeFreeEncoder(encoder);
    }
    drmModeFreeConnector(connector);
  }
  drmModeFreeResources(resources);
  return found;
}

static void on_commit_entry(void* data, struct wpe_dmabuf_pool_entry* entry) {
  struct renderer* r = data;
  if (r->frames++ == 0) {
    struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
    uint32_t format = gbm_bo_get_format(fb->bo);
    printf("first frame: %ux%u dma-buf, format %.4s, modifier 0x%016llx\n", gbm_bo_get_width(fb->bo),
           gbm_bo_get_height(fb->bo), (const char*)&format, (unsigned long long)gbm_bo_get_modifier(fb->bo));
    fflush(stdout);
  }
  switch (r->output.kind) {
    case OUTPUT_DISPLAY:
      return show(r, entry);
    case OUTPUT_COMPOSITOR:
      return present_to_compositor(r, entry);
    case OUTPUT_HEADLESS:
      wpe_view_backend_dmabuf_pool_fdo_dispatch_release_entry(r->pool, entry);
      wpe_view_backend_dmabuf_pool_fdo_dispatch_frame_complete(r->pool);
      return;
  }
}

// Buffers come from the display controller's node: Mesa (kmsro) allocates them there and renders into them on the
// Mali, so every frame is one the display can scan out.
static int open_display_device(void) {
  for (int minor = 0; minor < MAX_CARDS; minor++) {
    g_autofree char* path = g_strdup_printf("/dev/dri/card%d", minor);
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) continue;
    drmVersionPtr version = drmGetVersion(fd);
    gboolean match = version && strcmp(version->name, DISPLAY_DRIVER) == 0;
    drmFreeVersion(version);
    if (match) return fd;
    close(fd);
  }
  return -1;
}

static gboolean on_stop(gpointer data) {
  g_main_loop_quit(((struct renderer*)data)->loop);
  return G_SOURCE_REMOVE;
}

static gboolean on_load_failed(G_GNUC_UNUSED WebKitWebView* view, G_GNUC_UNUSED WebKitLoadEvent event, char* uri,
                               GError* error, gpointer data) {
  struct renderer* r = data;
  fprintf(stderr, "loading %s failed: %s\n", uri, error->message);
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
  return TRUE;
}

static void on_web_process_terminated(G_GNUC_UNUSED WebKitWebView* view, WebKitWebProcessTerminationReason reason,
                                      gpointer data) {
  struct renderer* r = data;
  if (r->stopping) return;
  fprintf(stderr, "the web process ended (reason %d)\n", reason);
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
}

static void on_title(WebKitWebView* view, G_GNUC_UNUSED GParamSpec* spec, G_GNUC_UNUSED gpointer data) {
  printf("title: %s\n", webkit_web_view_get_title(view));
  fflush(stdout);
}

static int fail(const char* message) {
  fprintf(stderr, "glassdeck-local-web: %s\n", message);
  return 1;
}

// Content types by extension, for what web wallpapers load (the same table as the PC's background:// handler).
static const char* mime_type(const char* path) {
  static const char* const TYPES[][2] = {
      {".html", "text/html"}, {".htm", "text/html"}, {".js", "text/javascript"}, {".mjs", "text/javascript"},
      {".css", "text/css"}, {".json", "application/json"}, {".map", "application/json"}, {".wasm", "application/wasm"},
      {".svg", "image/svg+xml"}, {".png", "image/png"}, {".jpg", "image/jpeg"}, {".jpeg", "image/jpeg"},
      {".gif", "image/gif"}, {".webp", "image/webp"}, {".ico", "image/x-icon"}, {".mp4", "video/mp4"},
      {".webm", "video/webm"}, {".mp3", "audio/mpeg"}, {".ogg", "audio/ogg"}, {".wav", "audio/wav"},
      {".woff", "font/woff"}, {".woff2", "font/woff2"}, {".ttf", "font/ttf"}, {".otf", "font/otf"},
      {".txt", "text/plain"}, {".xml", "application/xml"},
  };
  g_autofree char* lower = g_ascii_strdown(path, -1);
  for (size_t i = 0; i < G_N_ELEMENTS(TYPES); i++)
    if (g_str_has_suffix(lower, TYPES[i][0])) return TYPES[i][1];
  return "application/octet-stream";
}

// Stands in for Wallpaper Engine's JS hooks, as on the PC: user properties get their defaults, audio stays silent.
static char* wallpaper_init_script(const char* properties_json) {
  return g_strdup_printf(
      "(() => {\n"
      "  const userProperties = %s;\n"
      "  for (const name of [\"wallpaperRegisterAudioListener\", \"wallpaperRegisterMediaStatusListener\",\n"
      "      \"wallpaperRegisterMediaPropertiesListener\", \"wallpaperRegisterMediaThumbnailListener\",\n"
      "      \"wallpaperRegisterMediaPlaybackListener\", \"wallpaperRegisterMediaTimelineListener\"]) {\n"
      "    window[name] = () => {};\n"
      "  }\n"
      "  window.addEventListener(\"load\", () => setTimeout(() => {\n"
      "    const listener = window.wallpaperPropertyListener;\n"
      "    if (listener && typeof listener.applyUserProperties === \"function\") listener.applyUserProperties(userProperties);\n"
      "  }, 0));\n"
      "})();",
      properties_json);
}

// "bytes=a-b", "bytes=a-" or "bytes=-n" into [start, end); one range only.
static gboolean parse_range(const char* header, uint64_t len, uint64_t* start, uint64_t* end) {
  if (!g_str_has_prefix(header, "bytes=") || strchr(header, ',')) return FALSE;
  const char* spec = header + 6;
  const char* dash = strchr(spec, '-');
  if (!dash) return FALSE;
  char* rest;
  if (dash == spec) {
    uint64_t suffix = g_ascii_strtoull(dash + 1, &rest, 10);
    if (*rest || suffix == 0) return FALSE;
    *start = len - MIN(suffix, len);
    *end = len;
  } else {
    *start = g_ascii_strtoull(spec, &rest, 10);
    if (rest != dash) return FALSE;
    if (dash[1] == '\0') {
      *end = len;
    } else {
      uint64_t last = g_ascii_strtoull(dash + 1, &rest, 10);
      if (*rest) return FALSE;
      *end = MIN(last + 1, len);
    }
  }
  return *start < *end;
}

// Serves background://wallpaper/<entry> straight from the mapped background, honouring Range so media can seek.
static void on_background_request(WebKitURISchemeRequest* request, gpointer data) {
  const struct background* d = data;
  g_autofree char* path = g_uri_unescape_string(webkit_uri_scheme_request_get_path(request), NULL);
  const char* name = path && path[0] == '/' ? path + 1 : path;
  struct background_entry entry;
  if (!name || !background_find(d, name, &entry)) {
    fprintf(stderr, "glassdeck-local-web: the page asked for \"%s\", which the background does not hold\n", name ? name : "");
    GError* error = g_error_new(G_IO_ERROR, G_IO_ERROR_NOT_FOUND, "not in the background: %s", name ? name : "");
    webkit_uri_scheme_request_finish_error(request, error);
    g_error_free(error);
    return;
  }
  uint64_t start = 0, end = entry.len;
  SoupMessageHeaders* request_headers = webkit_uri_scheme_request_get_http_headers(request);
  const char* range = request_headers ? soup_message_headers_get_one(request_headers, "Range") : NULL;
  gboolean partial = range && parse_range(range, entry.len, &start, &end);
  if (range && !partial) {
    GError* error = g_error_new(G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT, "unsatisfiable range %s", range);
    webkit_uri_scheme_request_finish_error(request, error);
    g_error_free(error);
    return;
  }
  g_autoptr(GBytes) bytes = background_bytes(d, entry, start, end);
  g_autoptr(GInputStream) stream = g_memory_input_stream_new_from_bytes(bytes);
  g_autoptr(WebKitURISchemeResponse) response = webkit_uri_scheme_response_new(stream, (gint64)(end - start));
  webkit_uri_scheme_response_set_content_type(response, mime_type(name));
  SoupMessageHeaders* headers = soup_message_headers_new(SOUP_MESSAGE_HEADERS_RESPONSE);
  soup_message_headers_append(headers, "Accept-Ranges", "bytes");
  if (partial) {
    g_autofree char* content_range = g_strdup_printf("bytes %" G_GUINT64_FORMAT "-%" G_GUINT64_FORMAT "/%" G_GUINT64_FORMAT,
                                                     start, end - 1, entry.len);
    soup_message_headers_append(headers, "Content-Range", content_range);
    webkit_uri_scheme_response_set_status(response, 206, "Partial Content");
  }
  webkit_uri_scheme_response_set_http_headers(response, headers);
  webkit_uri_scheme_request_finish_with_response(request, response);
}

// The page's errors reach our stderr; WebKit's own console output never leaves the sandboxed web process.
static const char PAGE_ERRORS_SCRIPT[] =
    "(() => {\n"
    "  const post = (kind, text) => window.webkit.messageHandlers.glassdeck.postMessage(kind + ': ' + text);\n"
    "  window.addEventListener('error', e => post('error', `${e.message} at ${e.filename}:${e.lineno}`));\n"
    "  window.addEventListener('unhandledrejection', e => post('rejection', String(e.reason)));\n"
    "  for (const level of ['error', 'warn']) {\n"
    "    const original = console[level];\n"
    "    console[level] = (...args) => { post(level, args.map(String).join(' ')); original.apply(console, args); };\n"
    "  }\n"
    "})();";

static void on_page_message(G_GNUC_UNUSED WebKitUserContentManager* manager, JSCValue* value, G_GNUC_UNUSED gpointer data) {
  g_autofree char* text = jsc_value_to_string(value);
  fprintf(stderr, "page %s\n", text);
}

static void add_start_script(WebKitUserContentManager* scripts, const char* source) {
  WebKitUserScript* script = webkit_user_script_new(source, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
                                                    WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, NULL, NULL);
  webkit_user_content_manager_add_script(scripts, script);
  webkit_user_script_unref(script);
}

// Wallpapers are written for Wallpaper Engine's Chromium; WebKit has these too, but off by default.
static const char* const CHROMIUM_FEATURES[] = {"RequestIdleCallback"};

static gboolean enable_features(WebKitSettings* settings) {
  g_autoptr(WebKitFeatureList) features = webkit_settings_get_all_features();
  for (size_t i = 0; i < G_N_ELEMENTS(CHROMIUM_FEATURES); i++) {
    gboolean found = FALSE;
    for (gsize f = 0; f < webkit_feature_list_get_length(features) && !found; f++) {
      WebKitFeature* feature = webkit_feature_list_get(features, f);
      if (g_strcmp0(webkit_feature_get_identifier(feature), CHROMIUM_FEATURES[i]) != 0) continue;
      webkit_settings_set_feature_enabled(settings, feature, TRUE);
      found = TRUE;
    }
    if (!found) {
      fprintf(stderr, "glassdeck-local-web: this WebKit has no %s feature\n", CHROMIUM_FEATURES[i]);
      return FALSE;
    }
  }
  return TRUE;
}

static char* background_page_uri(const char* entry) {
  g_autofree char* escaped = g_uri_escape_string(entry, "/", FALSE);
  return g_strconcat(BACKGROUND_ORIGIN, escaped, NULL);
}

static int render(const struct options* o) {
  int card = open_display_device();
  if (card < 0) return fail("no " DISPLAY_DRIVER " display device");
  struct renderer r = {
      .loop = g_main_loop_new(NULL, FALSE),
      .gbm = gbm_create_device(card),
      .entries = g_ptr_array_new(),
      .output = {.kind = o->output},
  };
  if (!r.gbm) return fail("cannot open the GPU through GBM");
  switch (o->output) {
    case OUTPUT_DISPLAY: {
      struct display* d = &r.output.display;
      if (!find_output(card, d)) return fail("no connected output can show 1920x1080");
      printf("previewing on %ux%u@%u\n", d->mode.hdisplay, d->mode.vdisplay, d->mode.vrefresh);
      g_unix_fd_add(d->fd, G_IO_IN, on_display_event, NULL);
      break;
    }
    case OUTPUT_COMPOSITOR:
      r.output.compositor.socket = compositor_new(o->compositor_fd, on_compositor_released, &r);
      g_unix_fd_add(o->compositor_fd, G_IO_IN | G_IO_HUP | G_IO_ERR, on_compositor_readable, &r);
      break;
    case OUTPUT_HEADLESS:
      break;
  }
  if (!wpe_loader_init("libWPEBackend-fdo-1.0.so.1")) return fail("cannot load WPEBackend-fdo");
  if (!wpe_fdo_initialize_dmabuf()) return fail("cannot start WPEBackend-fdo in dma-buf pool mode");

  static const struct wpe_view_backend_dmabuf_pool_fdo_client client = {
      .create_entry = on_create_entry,
      .destroy_entry = on_destroy_entry,
      .commit_entry = on_commit_entry,
  };
  r.pool = wpe_view_backend_dmabuf_pool_fdo_create(&client, &r, PANEL_WIDTH, PANEL_HEIGHT);
  if (!r.pool) return fail("cannot create the view");
  // WebKit destroys the view backend itself; wpe_view_backend_dmabuf_pool_fdo_destroy segfaults in WPEBackend-fdo
  // 1.16 on teardown, so the pool's small wrapper is left for process exit.
  WebKitWebViewBackend* backend =
      webkit_web_view_backend_new(wpe_view_backend_dmabuf_pool_fdo_get_view_backend(r.pool), NULL, NULL);

  WebKitWebContext* context = webkit_web_context_new();
  WebKitUserContentManager* scripts = webkit_user_content_manager_new();
  webkit_user_content_manager_register_script_message_handler(scripts, "glassdeck", NULL);
  g_signal_connect(scripts, "script-message-received::glassdeck", G_CALLBACK(on_page_message), NULL);
  add_start_script(scripts, PAGE_ERRORS_SCRIPT);
  struct background* background = NULL;
  g_autofree char* uri = NULL;
  if (o->content == CONTENT_BACKGROUND) {
    g_autoptr(GError) error = NULL;
    struct background_web web = {0};
    background = background_open(o->location, &error);
    if (!background || !background_web_content(background, &web, &error)) return fail(error->message);
    webkit_web_context_register_uri_scheme(context, "background", on_background_request, background, NULL);
    WebKitSecurityManager* security = webkit_web_context_get_security_manager(context);
    webkit_security_manager_register_uri_scheme_as_secure(security, "background");
    webkit_security_manager_register_uri_scheme_as_cors_enabled(security, "background");
    g_autofree char* init = wallpaper_init_script(web.properties_json);
    add_start_script(scripts, init);
    uri = background_page_uri(web.entry);
    background_web_clear(&web);
  } else {
    uri = g_strdup(o->location);
  }
  WebKitNetworkSession* session = webkit_network_session_new_ephemeral();
  WebKitSettings* settings = webkit_settings_new_with_settings("media-playback-requires-user-gesture", FALSE, NULL);
  if (!enable_features(settings)) return 1;
  WebKitWebView* view = g_object_new(WEBKIT_TYPE_WEB_VIEW, "backend", backend, "web-context", context,
                                     "network-session", session, "settings", settings, "user-content-manager", scripts,
                                     NULL);
  webkit_web_view_set_is_muted(view, TRUE);
  // A view WebKit thinks is hidden gets requestAnimationFrame and timers throttled to a crawl.
  wpe_view_backend_add_activity_state(wpe_view_backend_dmabuf_pool_fdo_get_view_backend(r.pool),
                                      wpe_view_activity_state_visible | wpe_view_activity_state_focused |
                                          wpe_view_activity_state_in_window);

  g_signal_connect(view, "load-failed", G_CALLBACK(on_load_failed), &r);
  g_signal_connect(view, "web-process-terminated", G_CALLBACK(on_web_process_terminated), &r);
  g_signal_connect(view, "notify::title", G_CALLBACK(on_title), NULL);
  g_unix_signal_add(SIGINT, on_stop, &r);
  g_unix_signal_add(SIGTERM, on_stop, &r);
  if (o->seconds) g_timeout_add_seconds(o->seconds, on_stop, &r);

  webkit_web_view_load_uri(view, uri);
  gint64 started = g_get_monotonic_time();
  g_main_loop_run(r.loop);
  double seconds = (g_get_monotonic_time() - started) / 1e6;
  printf("Painted %u frames in %.1f s (%.1f fps) into %u buffers\n", r.frames, seconds, r.frames / seconds, r.entries->len);
  fflush(stdout);

  r.stopping = TRUE;
  webkit_web_view_terminate_web_process(view);
  g_object_unref(view);
  g_object_unref(settings);
  g_object_unref(session);
  g_object_unref(scripts);
  g_object_unref(context);
  if (background) background_free(background);
  g_main_loop_unref(r.loop);
  for (guint i = 0; i < r.entries->len; i++) free_entry(&r, g_ptr_array_index(r.entries, i));
  g_ptr_array_unref(r.entries);
  if (r.output.kind == OUTPUT_COMPOSITOR) {
    if (r.output.compositor.complete_timer) g_source_remove(r.output.compositor.complete_timer);
    compositor_free(r.output.compositor.socket);
  }
  gbm_device_destroy(r.gbm);
  close(card);
  return r.exit_code;
}

int main(int argc, char** argv) {
  struct options o = {0};
  if (!parse_options(argc, argv, &o)) return usage();
  return o.inside ? render(&o) : exec_in_sandbox(&o);
}
