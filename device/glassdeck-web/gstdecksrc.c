// A GStreamer source for deck:// URIs, so <video> and <audio> in a web deck play from the deck like the rest of
// the page. WebKit hands media URLs to GStreamer instead of its own loader, and GStreamer knows no deck scheme.
// The web process runs inside glassdeck-web's sandbox, where GLASSDECK_DECK names the deck file.
#include <gst/base/gstbasesrc.h>
#include <gst/gst.h>
#include <string.h>

#include "deck.h"

#define DECK_URI_PREFIX "deck://wallpaper/"

typedef struct {
  GstBaseSrc parent;
  char* uri;
  char* entry_name;
  struct deck* deck;
  struct deck_entry entry;
} GstDeckSrc;

typedef struct {
  GstBaseSrcClass parent_class;
} GstDeckSrcClass;

static void gst_deck_src_uri_handler_init(gpointer iface, gpointer data);

G_DEFINE_TYPE_WITH_CODE(GstDeckSrc, gst_deck_src, GST_TYPE_BASE_SRC,
                        G_IMPLEMENT_INTERFACE(GST_TYPE_URI_HANDLER, gst_deck_src_uri_handler_init))

static GstStaticPadTemplate src_template = GST_STATIC_PAD_TEMPLATE("src", GST_PAD_SRC, GST_PAD_ALWAYS, GST_STATIC_CAPS_ANY);

static gboolean set_uri(GstDeckSrc* self, const char* uri, GError** error) {
  if (!g_str_has_prefix(uri, DECK_URI_PREFIX)) {
    g_set_error(error, GST_URI_ERROR, GST_URI_ERROR_BAD_URI, "not a deck URI: %s", uri);
    return FALSE;
  }
  g_autofree char* path = g_strdup(uri + strlen(DECK_URI_PREFIX));
  char* query = strpbrk(path, "?#");
  if (query) *query = '\0';
  g_free(self->entry_name);
  self->entry_name = g_uri_unescape_string(path, NULL);
  g_free(self->uri);
  self->uri = g_strdup(uri);
  return self->entry_name != NULL;
}

static gboolean gst_deck_src_start(GstBaseSrc* base) {
  GstDeckSrc* self = (GstDeckSrc*)base;
  const char* path = g_getenv("GLASSDECK_DECK");
  if (!path || !self->entry_name) {
    GST_ELEMENT_ERROR(self, RESOURCE, NOT_FOUND, ("no deck to read"), ("GLASSDECK_DECK is unset or the URI is missing"));
    return FALSE;
  }
  g_autoptr(GError) error = NULL;
  self->deck = deck_open(path, &error);
  if (!self->deck) {
    GST_ELEMENT_ERROR(self, RESOURCE, OPEN_READ, ("cannot open the deck"), ("%s", error->message));
    return FALSE;
  }
  if (!deck_find(self->deck, self->entry_name, &self->entry)) {
    GST_ELEMENT_ERROR(self, RESOURCE, NOT_FOUND, ("the deck holds no %s", self->entry_name), (NULL));
    return FALSE;
  }
  return TRUE;
}

static gboolean gst_deck_src_stop(GstBaseSrc* base) {
  GstDeckSrc* self = (GstDeckSrc*)base;
  g_clear_pointer(&self->deck, deck_free);
  return TRUE;
}

static gboolean gst_deck_src_is_seekable(G_GNUC_UNUSED GstBaseSrc* base) {
  return TRUE;
}

static gboolean gst_deck_src_get_size(GstBaseSrc* base, guint64* size) {
  GstDeckSrc* self = (GstDeckSrc*)base;
  if (!self->deck) return FALSE;
  *size = self->entry.len;
  return TRUE;
}

static GstFlowReturn gst_deck_src_create(GstBaseSrc* base, guint64 offset, guint length, GstBuffer** buffer) {
  GstDeckSrc* self = (GstDeckSrc*)base;
  if (offset >= self->entry.len) return GST_FLOW_EOS;
  guint64 end = MIN(offset + length, self->entry.len);
  g_autoptr(GBytes) bytes = deck_bytes(self->deck, self->entry, offset, end);
  gsize size;
  const void* data = g_bytes_get_data(bytes, &size);
  *buffer = gst_buffer_new_memdup(data, size);
  GST_BUFFER_OFFSET(*buffer) = offset;
  GST_BUFFER_OFFSET_END(*buffer) = end;
  return GST_FLOW_OK;
}

static void gst_deck_src_finalize(GObject* object) {
  GstDeckSrc* self = (GstDeckSrc*)object;
  g_free(self->uri);
  g_free(self->entry_name);
  G_OBJECT_CLASS(gst_deck_src_parent_class)->finalize(object);
}

static void gst_deck_src_class_init(GstDeckSrcClass* klass) {
  GObjectClass* object_class = G_OBJECT_CLASS(klass);
  GstElementClass* element_class = GST_ELEMENT_CLASS(klass);
  GstBaseSrcClass* base_class = GST_BASE_SRC_CLASS(klass);
  object_class->finalize = gst_deck_src_finalize;
  gst_element_class_add_static_pad_template(element_class, &src_template);
  gst_element_class_set_static_metadata(element_class, "Glassdeck deck source", "Source/File",
                                        "Reads an entry of a .deck in place", "Glassdeck");
  base_class->start = gst_deck_src_start;
  base_class->stop = gst_deck_src_stop;
  base_class->is_seekable = gst_deck_src_is_seekable;
  base_class->get_size = gst_deck_src_get_size;
  base_class->create = gst_deck_src_create;
}

static void gst_deck_src_init(GstDeckSrc* self) {
  gst_base_src_set_format(GST_BASE_SRC(self), GST_FORMAT_BYTES);
}

static GstURIType uri_get_type(G_GNUC_UNUSED GType type) {
  return GST_URI_SRC;
}

static const gchar* const* uri_get_protocols(G_GNUC_UNUSED GType type) {
  static const gchar* const protocols[] = {"deck", NULL};
  return protocols;
}

static gchar* uri_get_uri(GstURIHandler* handler) {
  return g_strdup(((GstDeckSrc*)handler)->uri);
}

static gboolean uri_set_uri(GstURIHandler* handler, const gchar* uri, GError** error) {
  return set_uri((GstDeckSrc*)handler, uri, error);
}

static void gst_deck_src_uri_handler_init(gpointer iface, G_GNUC_UNUSED gpointer data) {
  GstURIHandlerInterface* uri = iface;
  uri->get_type = uri_get_type;
  uri->get_protocols = uri_get_protocols;
  uri->get_uri = uri_get_uri;
  uri->set_uri = uri_set_uri;
}

static gboolean plugin_init(GstPlugin* plugin) {
  return gst_element_register(plugin, "decksrc", GST_RANK_PRIMARY, gst_deck_src_get_type());
}

#define PACKAGE "glassdeck-web"
GST_PLUGIN_DEFINE(GST_VERSION_MAJOR, GST_VERSION_MINOR, deck, "deck:// sources for Glassdeck web decks", plugin_init, "1.0",
                  "MIT/X11", PACKAGE, "Glassdeck")
