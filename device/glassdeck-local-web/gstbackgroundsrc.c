// A GStreamer source for background:// URIs, so <video> and <audio> in a web background play from the background like the rest of
// the page. WebKit hands media URLs to GStreamer instead of its own loader, and GStreamer knows no background scheme.
// The web process runs inside glassdeck-local-web's sandbox, where GLASSDECK_BACKGROUND names the background file.
#include <gst/base/gstbasesrc.h>
#include <gst/gst.h>
#include <string.h>

#include "background.h"

#define BACKGROUND_URI_PREFIX "background://wallpaper/"

typedef struct {
  GstBaseSrc parent;
  char* uri;
  char* entry_name;
  struct background* background;
  struct background_entry entry;
} GstBackgroundSrc;

typedef struct {
  GstBaseSrcClass parent_class;
} GstBackgroundSrcClass;

static void gst_background_src_uri_handler_init(gpointer iface, gpointer data);

G_DEFINE_TYPE_WITH_CODE(GstBackgroundSrc, gst_background_src, GST_TYPE_BASE_SRC,
                        G_IMPLEMENT_INTERFACE(GST_TYPE_URI_HANDLER, gst_background_src_uri_handler_init))

static GstStaticPadTemplate src_template = GST_STATIC_PAD_TEMPLATE("src", GST_PAD_SRC, GST_PAD_ALWAYS, GST_STATIC_CAPS_ANY);

static gboolean set_uri(GstBackgroundSrc* self, const char* uri, GError** error) {
  if (!g_str_has_prefix(uri, BACKGROUND_URI_PREFIX)) {
    g_set_error(error, GST_URI_ERROR, GST_URI_ERROR_BAD_URI, "not a background URI: %s", uri);
    return FALSE;
  }
  g_autofree char* path = g_strdup(uri + strlen(BACKGROUND_URI_PREFIX));
  char* query = strpbrk(path, "?#");
  if (query) *query = '\0';
  g_free(self->entry_name);
  self->entry_name = g_uri_unescape_string(path, NULL);
  g_free(self->uri);
  self->uri = g_strdup(uri);
  return self->entry_name != NULL;
}

static gboolean gst_background_src_start(GstBaseSrc* base) {
  GstBackgroundSrc* self = (GstBackgroundSrc*)base;
  const char* path = g_getenv("GLASSDECK_BACKGROUND");
  if (!path || !self->entry_name) {
    GST_ELEMENT_ERROR(self, RESOURCE, NOT_FOUND, ("no background to read"), ("GLASSDECK_BACKGROUND is unset or the URI is missing"));
    return FALSE;
  }
  g_autoptr(GError) error = NULL;
  self->background = background_open(path, &error);
  if (!self->background) {
    GST_ELEMENT_ERROR(self, RESOURCE, OPEN_READ, ("cannot open the background"), ("%s", error->message));
    return FALSE;
  }
  if (!background_find(self->background, self->entry_name, &self->entry)) {
    GST_ELEMENT_ERROR(self, RESOURCE, NOT_FOUND, ("the background holds no %s", self->entry_name), (NULL));
    return FALSE;
  }
  return TRUE;
}

static gboolean gst_background_src_stop(GstBaseSrc* base) {
  GstBackgroundSrc* self = (GstBackgroundSrc*)base;
  g_clear_pointer(&self->background, background_free);
  return TRUE;
}

static gboolean gst_background_src_is_seekable(G_GNUC_UNUSED GstBaseSrc* base) {
  return TRUE;
}

static gboolean gst_background_src_get_size(GstBaseSrc* base, guint64* size) {
  GstBackgroundSrc* self = (GstBackgroundSrc*)base;
  if (!self->background) return FALSE;
  *size = self->entry.len;
  return TRUE;
}

static GstFlowReturn gst_background_src_create(GstBaseSrc* base, guint64 offset, guint length, GstBuffer** buffer) {
  GstBackgroundSrc* self = (GstBackgroundSrc*)base;
  if (offset >= self->entry.len) return GST_FLOW_EOS;
  guint64 end = MIN(offset + length, self->entry.len);
  g_autoptr(GBytes) bytes = background_bytes(self->background, self->entry, offset, end);
  gsize size;
  const void* data = g_bytes_get_data(bytes, &size);
  *buffer = gst_buffer_new_memdup(data, size);
  GST_BUFFER_OFFSET(*buffer) = offset;
  GST_BUFFER_OFFSET_END(*buffer) = end;
  return GST_FLOW_OK;
}

static void gst_background_src_finalize(GObject* object) {
  GstBackgroundSrc* self = (GstBackgroundSrc*)object;
  g_free(self->uri);
  g_free(self->entry_name);
  G_OBJECT_CLASS(gst_background_src_parent_class)->finalize(object);
}

static void gst_background_src_class_init(GstBackgroundSrcClass* klass) {
  GObjectClass* object_class = G_OBJECT_CLASS(klass);
  GstElementClass* element_class = GST_ELEMENT_CLASS(klass);
  GstBaseSrcClass* base_class = GST_BASE_SRC_CLASS(klass);
  object_class->finalize = gst_background_src_finalize;
  gst_element_class_add_static_pad_template(element_class, &src_template);
  gst_element_class_set_static_metadata(element_class, "Glassdeck background source", "Source/File",
                                        "Reads an entry of a .deckbg in place", "Glassdeck");
  base_class->start = gst_background_src_start;
  base_class->stop = gst_background_src_stop;
  base_class->is_seekable = gst_background_src_is_seekable;
  base_class->get_size = gst_background_src_get_size;
  base_class->create = gst_background_src_create;
}

static void gst_background_src_init(GstBackgroundSrc* self) {
  gst_base_src_set_format(GST_BASE_SRC(self), GST_FORMAT_BYTES);
}

static GstURIType uri_get_type(G_GNUC_UNUSED GType type) {
  return GST_URI_SRC;
}

static const gchar* const* uri_get_protocols(G_GNUC_UNUSED GType type) {
  static const gchar* const protocols[] = {"background", NULL};
  return protocols;
}

static gchar* uri_get_uri(GstURIHandler* handler) {
  return g_strdup(((GstBackgroundSrc*)handler)->uri);
}

static gboolean uri_set_uri(GstURIHandler* handler, const gchar* uri, GError** error) {
  return set_uri((GstBackgroundSrc*)handler, uri, error);
}

static void gst_background_src_uri_handler_init(gpointer iface, G_GNUC_UNUSED gpointer data) {
  GstURIHandlerInterface* uri = iface;
  uri->get_type = uri_get_type;
  uri->get_protocols = uri_get_protocols;
  uri->get_uri = uri_get_uri;
  uri->set_uri = uri_set_uri;
}

static gboolean plugin_init(GstPlugin* plugin) {
  return gst_element_register(plugin, "backgroundsrc", GST_RANK_PRIMARY, gst_background_src_get_type());
}

#define PACKAGE "glassdeck-local-web"
GST_PLUGIN_DEFINE(GST_VERSION_MAJOR, GST_VERSION_MINOR, background, "background:// sources for Glassdeck web backgrounds", plugin_init, "1.0",
                  "MIT/X11", PACKAGE, "Glassdeck")
