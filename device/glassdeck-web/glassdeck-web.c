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

#define PANEL_WIDTH 1920
#define PANEL_HEIGHT 1080
#define INSIDE_FLAG "--inside-sandbox"
#define SANDBOXED_SELF "/app/glassdeck-web"
#define DISPLAY_DRIVER "rockchip"
#define MAX_CARDS 16

static const char* const DEVICES[] = {"/dev/mpp_service", "/dev/dma_heap", "/dev/dri"};
// MPP identifies the chip from the device tree (/proc/device-tree links here); libdrm finds GPUs through /sys.
static const char* const READ_ONLY_SYSTEM[] = {
    "/usr", "/etc/ld.so.cache", "/etc/fonts", "/sys/firmware/devicetree/base", "/sys/dev/char", "/sys/devices/platform",
    "/sys/devices/system/cpu",
};

struct options {
  const char* uri;
  guint seconds;
  gboolean inside;
  // Drive the screen directly, for previews while no receiver holds it
  gboolean display;
};

static int usage(void) {
  fprintf(stderr, "usage: glassdeck-web [--seconds=N] [--display] <uri>\n");
  return 2;
}

static gboolean parse_options(int argc, char** argv, struct options* o) {
  for (int i = 1; i < argc; i++) {
    if (strcmp(argv[i], INSIDE_FLAG) == 0) {
      o->inside = TRUE;
    } else if (strcmp(argv[i], "--display") == 0) {
      o->display = TRUE;
    } else if (strncmp(argv[i], "--seconds=", 10) == 0) {
      o->seconds = (guint)strtoul(argv[i] + 10, NULL, 10);
    } else if (argv[i][0] == '-' || o->uri) {
      return FALSE;
    } else {
      o->uri = argv[i];
    }
  }
  return o->uri != NULL;
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
    fprintf(stderr, "glassdeck-web: refusing to run pages as root; run as the glassdeck-web user\n");
    return 1;
  }
  for (size_t i = 0; i < G_N_ELEMENTS(DEVICES); i++) {
    int needed = g_file_test(DEVICES[i], G_FILE_TEST_IS_DIR) ? R_OK | X_OK : R_OK | W_OK;
    if (access(DEVICES[i], needed) != 0) {
      fprintf(stderr, "glassdeck-web: cannot open %s (%s); is this user in the video group?\n", DEVICES[i], strerror(errno));
      return 1;
    }
  }
  g_autofree char* self = g_file_read_link("/proc/self/exe", NULL);
  if (!self) return fprintf(stderr, "glassdeck-web: cannot find its own executable\n"), 1;

  g_autoptr(GPtrArray) args = g_ptr_array_new_with_free_func(g_free);
  add(args, "bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL", NULL);
  for (size_t i = 0; i < G_N_ELEMENTS(READ_ONLY_SYSTEM); i++) add(args, "--ro-bind-try", READ_ONLY_SYSTEM[i], READ_ONLY_SYSTEM[i], NULL);
  add(args, "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/sbin", "/sbin", NULL);
  add(args, "--proc", "/proc", "--dev", "/dev", NULL);
  for (size_t i = 0; i < G_N_ELEMENTS(DEVICES); i++) add(args, "--dev-bind", DEVICES[i], DEVICES[i], NULL);
  add(args, "--tmpfs", "/tmp", "--tmpfs", "/run", "--dir", "/tmp/runtime", NULL);
  g_autofree char* content = content_dir(o->uri);
  if (content) add(args, "--ro-bind", content, content, NULL);
  add(args, "--ro-bind", self, SANDBOXED_SELF, NULL);
  add(args, "--setenv", "HOME", "/tmp", "--setenv", "XDG_RUNTIME_DIR", "/tmp/runtime", NULL);
  add(args, "--unsetenv", "DBUS_SESSION_BUS_ADDRESS", "--unsetenv", "WAYLAND_DISPLAY", "--unsetenv", "DISPLAY", NULL);
  // WebKit's own sandbox cannot nest in ours or pass the decoder through; ours stands in for it.
  add(args, "--setenv", "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS", "1", NULL);
  add(args, "--", SANDBOXED_SELF, INSIDE_FLAG, NULL);
  if (o->display) add(args, "--display", NULL);
  if (o->seconds) {
    g_autofree char* seconds = g_strdup_printf("--seconds=%u", o->seconds);
    add(args, seconds, NULL);
  }
  add(args, o->uri, NULL);
  g_ptr_array_add(args, NULL);

  execvp("bwrap", (char**)args->pdata);
  fprintf(stderr, "glassdeck-web: cannot start bwrap: %s\n", strerror(errno));
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

struct renderer {
  struct gbm_device* gbm;
  struct wpe_view_backend_dmabuf_pool_fdo* pool;
  GMainLoop* loop;
  // WPEBackend-fdo 1.16 never calls destroy_entry, so the renderer frees what it allocated.
  GPtrArray* entries;
  struct display display;
  guint frames;
  gboolean stopping;
  int exit_code;
};

struct frame_buffer {
  struct gbm_bo* bo;
  int fd;
  uint32_t fb_id;
};

static void free_entry(struct renderer* r, struct wpe_dmabuf_pool_entry* entry) {
  struct frame_buffer* fb = wpe_dmabuf_pool_entry_get_user_data(entry);
  wpe_dmabuf_pool_entry_destroy(entry);
  if (fb->fb_id) drmModeRmFB(r->display.fd, fb->fb_id);
  close(fb->fd);
  gbm_bo_destroy(fb->bo);
  g_free(fb);
}

static void stop_with_error(struct renderer* r, const char* message) {
  fprintf(stderr, "glassdeck-web: %s: %s\n", message, strerror(errno));
  r->exit_code = 1;
  g_main_loop_quit(r->loop);
}

// WebKit renders into dma-bufs we allocate; this is what the receiver's compositor will put on a plane.
static struct wpe_dmabuf_pool_entry* on_create_entry(void* data) {
  struct renderer* r = data;
  struct gbm_bo* bo = gbm_bo_create(r->gbm, PANEL_WIDTH, PANEL_HEIGHT, GBM_FORMAT_XRGB8888,
                                    GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
  if (!bo) {
    fprintf(stderr, "glassdeck-web: cannot allocate a frame buffer\n");
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
  struct display* d = &r->display;
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
  struct display* d = &r->display;
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
  if (r->display.fd >= 0) return show(r, entry);
  wpe_view_backend_dmabuf_pool_fdo_dispatch_release_entry(r->pool, entry);
  wpe_view_backend_dmabuf_pool_fdo_dispatch_frame_complete(r->pool);
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
  fprintf(stderr, "glassdeck-web: %s\n", message);
  return 1;
}

static int render(const struct options* o) {
  int card = open_display_device();
  if (card < 0) return fail("no " DISPLAY_DRIVER " display device");
  struct renderer r = {
      .loop = g_main_loop_new(NULL, FALSE),
      .gbm = gbm_create_device(card),
      .entries = g_ptr_array_new(),
      .display = {.fd = -1},
  };
  if (!r.gbm) return fail("cannot open the GPU through GBM");
  if (o->display) {
    if (!find_output(card, &r.display)) return fail("no connected output can show 1920x1080");
    printf("previewing on %ux%u@%u\n", r.display.mode.hdisplay, r.display.mode.vdisplay, r.display.mode.vrefresh);
    g_unix_fd_add(r.display.fd, G_IO_IN, on_display_event, NULL);
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
  WebKitNetworkSession* session = webkit_network_session_new_ephemeral();
  WebKitSettings* settings = webkit_settings_new_with_settings(
      "enable-write-console-messages-to-stdout", TRUE, "media-playback-requires-user-gesture", FALSE, NULL);
  WebKitWebView* view = g_object_new(WEBKIT_TYPE_WEB_VIEW, "backend", backend, "web-context", context,
                                     "network-session", session, "settings", settings, NULL);
  webkit_web_view_set_is_muted(view, TRUE);

  g_signal_connect(view, "load-failed", G_CALLBACK(on_load_failed), &r);
  g_signal_connect(view, "web-process-terminated", G_CALLBACK(on_web_process_terminated), &r);
  g_signal_connect(view, "notify::title", G_CALLBACK(on_title), NULL);
  g_unix_signal_add(SIGINT, on_stop, &r);
  g_unix_signal_add(SIGTERM, on_stop, &r);
  if (o->seconds) g_timeout_add_seconds(o->seconds, on_stop, &r);

  webkit_web_view_load_uri(view, o->uri);
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
  g_object_unref(context);
  g_main_loop_unref(r.loop);
  for (guint i = 0; i < r.entries->len; i++) free_entry(&r, g_ptr_array_index(r.entries, i));
  g_ptr_array_unref(r.entries);
  gbm_device_destroy(r.gbm);
  close(card);
  return r.exit_code;
}

int main(int argc, char** argv) {
  struct options o = {0};
  if (!parse_options(argc, argv, &o)) return usage();
  return o.inside ? render(&o) : exec_in_sandbox(&o);
}
