#include "deck.h"

#include <errno.h>
#include <fcntl.h>
#include <gio/gio.h>
#include <json-glib/json-glib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define LOCAL_SIGNATURE 0x04034b50u
#define CENTRAL_SIGNATURE 0x02014b50u
#define END_SIGNATURE 0x06054b50u
#define LOCAL_HEADER_LEN 30
#define CENTRAL_HEADER_LEN 46
#define END_RECORD_LEN 22
#define MAX_COMMENT_LEN 0xffff
#define STORED 0
#define FLAG_ENCRYPTED 1
#define MANIFEST "manifest.json"

struct deck {
  const uint8_t* map;
  size_t size;
  GHashTable* entries;
};

static uint16_t u16_at(const uint8_t* p) {
  return (uint16_t)(p[0] | p[1] << 8);
}

static uint32_t u32_at(const uint8_t* p) {
  return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static gboolean fail(GError** error, const char* path, const char* format, ...) G_GNUC_PRINTF(3, 4);
static gboolean fail(GError** error, const char* path, const char* format, ...) {
  va_list args;
  va_start(args, format);
  g_autofree char* message = g_strdup_vprintf(format, args);
  va_end(args);
  g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "%s: %s", path, message);
  return FALSE;
}

static gboolean read_directory(struct deck* d, const char* path, GError** error) {
  if (d->size < END_RECORD_LEN) return fail(error, path, "not a zip file");
  size_t lowest = d->size > END_RECORD_LEN + MAX_COMMENT_LEN ? d->size - END_RECORD_LEN - MAX_COMMENT_LEN : 0;
  size_t end = d->size - END_RECORD_LEN + 1;
  do {
    end--;
  } while (end > lowest && u32_at(d->map + end) != END_SIGNATURE);
  if (u32_at(d->map + end) != END_SIGNATURE) return fail(error, path, "not a zip file (no end of central directory)");
  uint16_t count = u16_at(d->map + end + 10);
  uint64_t directory_len = u32_at(d->map + end + 12);
  uint64_t at = u32_at(d->map + end + 16);
  if (at + directory_len > d->size) return fail(error, path, "the central directory runs past the end");

  for (uint16_t i = 0; i < count; i++) {
    const uint8_t* h = d->map + at;
    if (at + CENTRAL_HEADER_LEN > d->size || u32_at(h) != CENTRAL_SIGNATURE) return fail(error, path, "corrupt central directory");
    uint16_t flags = u16_at(h + 8), method = u16_at(h + 10);
    uint64_t compressed = u32_at(h + 20), size = u32_at(h + 24), local = u32_at(h + 42);
    uint16_t name_len = u16_at(h + 28), extra_len = u16_at(h + 30), comment_len = u16_at(h + 32);
    if (at + CENTRAL_HEADER_LEN + name_len > d->size) return fail(error, path, "corrupt central directory");
    g_autofree char* name = g_strndup((const char*)h + CENTRAL_HEADER_LEN, name_len);
    at += CENTRAL_HEADER_LEN + name_len + extra_len + comment_len;

    if (method != STORED) return fail(error, path, "entry \"%s\" is compressed (method %u); a deck holds stored entries only", name, method);
    if (flags & FLAG_ENCRYPTED) return fail(error, path, "entry \"%s\" is encrypted", name);
    if (compressed != size) return fail(error, path, "entry \"%s\" has different stored and real sizes", name);
    if (g_str_has_suffix(name, "/")) continue;
    if (local + LOCAL_HEADER_LEN > d->size || u32_at(d->map + local) != LOCAL_SIGNATURE)
      return fail(error, path, "entry \"%s\" has no local header", name);
    uint64_t offset = local + LOCAL_HEADER_LEN + u16_at(d->map + local + 26) + u16_at(d->map + local + 28);
    if (offset + size > d->size) return fail(error, path, "entry \"%s\" runs past the end", name);
    if (g_hash_table_contains(d->entries, name)) return fail(error, path, "entry \"%s\" appears twice", name);
    struct deck_entry* entry = g_new(struct deck_entry, 1);
    *entry = (struct deck_entry){.offset = offset, .len = size};
    g_hash_table_insert(d->entries, g_steal_pointer(&name), entry);
  }
  return TRUE;
}

struct deck* deck_open(const char* path, GError** error) {
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  struct stat st;
  if (fd < 0 || fstat(fd, &st) != 0) {
    g_set_error(error, G_IO_ERROR, g_io_error_from_errno(errno), "opening %s: %s", path, g_strerror(errno));
    if (fd >= 0) close(fd);
    return NULL;
  }
  void* map = st.st_size > 0 ? mmap(NULL, (size_t)st.st_size, PROT_READ, MAP_PRIVATE, fd, 0) : MAP_FAILED;
  int mapped_errno = errno;
  close(fd);
  if (map == MAP_FAILED) {
    g_set_error(error, G_IO_ERROR, g_io_error_from_errno(mapped_errno), "mapping %s: %s", path, g_strerror(mapped_errno));
    return NULL;
  }
  struct deck* d = g_new0(struct deck, 1);
  *d = (struct deck){.map = map, .size = (size_t)st.st_size, .entries = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, g_free)};
  if (!read_directory(d, path, error)) {
    deck_free(d);
    return NULL;
  }
  return d;
}

void deck_free(struct deck* d) {
  munmap((void*)d->map, d->size);
  g_hash_table_unref(d->entries);
  g_free(d);
}

gboolean deck_find(const struct deck* d, const char* name, struct deck_entry* entry) {
  const struct deck_entry* found = g_hash_table_lookup(d->entries, name);
  if (found) *entry = *found;
  return found != NULL;
}

GBytes* deck_bytes(const struct deck* d, struct deck_entry entry, uint64_t start, uint64_t end) {
  g_assert(start <= end && end <= entry.len);
  return g_bytes_new_static(d->map + entry.offset + start, end - start);
}

gboolean deck_web_content(const struct deck* d, struct deck_web* web, GError** error) {
  struct deck_entry manifest;
  if (!deck_find(d, MANIFEST, &manifest)) return fail(error, "deck", "no " MANIFEST);
  g_autoptr(JsonParser) parser = json_parser_new();
  if (!json_parser_load_from_data(parser, (const char*)d->map + manifest.offset, (gssize)manifest.len, error)) return FALSE;
  JsonNode* root = json_parser_get_root(parser);
  JsonObject* top = JSON_NODE_HOLDS_OBJECT(root) ? json_node_get_object(root) : NULL;
  if (!top || json_object_get_int_member_with_default(top, "format", 0) != DECK_FORMAT_VERSION)
    return fail(error, MANIFEST, "not deck format %d", DECK_FORMAT_VERSION);
  JsonObject* content = json_object_get_object_member(top, "content");
  const char* kind = content ? json_object_get_string_member_with_default(content, "kind", NULL) : NULL;
  if (g_strcmp0(kind, "web") != 0) return fail(error, MANIFEST, "content is \"%s\", not a web page", kind ? kind : "missing");
  const char* entry = json_object_get_string_member_with_default(content, "entry", NULL);
  JsonNode* properties = json_object_get_member(content, "properties");
  if (!entry || !properties || !JSON_NODE_HOLDS_OBJECT(properties)) return fail(error, MANIFEST, "web content needs an entry and properties");
  struct deck_entry page;
  if (!deck_find(d, entry, &page)) return fail(error, MANIFEST, "names \"%s\", which the deck does not hold", entry);
  web->entry = g_strdup(entry);
  web->properties_json = json_to_string(properties, FALSE);
  return TRUE;
}

void deck_web_clear(struct deck_web* web) {
  g_clear_pointer(&web->entry, g_free);
  g_clear_pointer(&web->properties_json, g_free);
}
