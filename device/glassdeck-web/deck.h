// A .deck read in place: the slice of ZIP glassdeck-deck writes (stored entries, no ZIP64), mapped once.
#pragma once

#include <glib.h>
#include <stdint.h>

#define DECK_FORMAT_VERSION 2

struct deck_entry {
  uint64_t offset;
  uint64_t len;
};

// What a web deck's manifest says to load.
struct deck_web {
  char* entry;
  // Wallpaper Engine user properties with their defaults, as JSON text
  char* properties_json;
};

struct deck;

struct deck* deck_open(const char* path, GError** error);
void deck_free(struct deck* d);
gboolean deck_find(const struct deck* d, const char* name, struct deck_entry* entry);
// The entry's bytes, straight from the mapping; valid while the deck is open.
GBytes* deck_bytes(const struct deck* d, struct deck_entry entry, uint64_t start, uint64_t end);
gboolean deck_web_content(const struct deck* d, struct deck_web* web, GError** error);
void deck_web_clear(struct deck_web* web);
