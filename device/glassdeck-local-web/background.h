// A .deck read in place: the slice of ZIP glassdeck-package writes (stored entries, no ZIP64), mapped once.
#pragma once

#include <glib.h>
#include <stdint.h>

#define PACKAGE_FORMAT_VERSION 4

struct background_entry {
  uint64_t offset;
  uint64_t len;
};

// What a web background's manifest says to load.
struct background_web {
  char* entry;
  // Wallpaper Engine user properties with their defaults, as JSON text
  char* properties_json;
};

struct background;

struct background* background_open(const char* path, GError** error);
void background_free(struct background* d);
gboolean background_find(const struct background* d, const char* name, struct background_entry* entry);
// The entry's bytes, straight from the mapping; valid while the background is open.
GBytes* background_bytes(const struct background* d, struct background_entry entry, uint64_t start, uint64_t end);
gboolean background_web_content(const struct background* d, struct background_web* web, GError** error);
void background_web_clear(struct background_web* web);
