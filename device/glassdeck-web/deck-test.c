// Reads a deck written by the Rust converter (glassdeck-deck), so both sides agree on the format.
#include <gio/gio.h>
#include <glib/gstdio.h>
#include <string.h>

#include "deck.h"

static const char* fixture;

static void reads_the_converters_web_deck(void) {
  g_autoptr(GError) error = NULL;
  struct deck* d = deck_open(fixture, &error);
  g_assert_no_error(error);

  struct deck_web web = {0};
  g_assert_true(deck_web_content(d, &web, &error));
  g_assert_cmpstr(web.entry, ==, "web/index.html");
  g_assert_cmpstr(web.properties_json, ==, "{\"speed\":{\"type\":\"slider\",\"value\":7}}");

  struct deck_entry script;
  g_assert_true(deck_find(d, "web/js/app.js", &script));
  g_assert_cmpuint(script.offset % 4096, ==, 0);
  g_autoptr(GBytes) bytes = deck_bytes(d, script, 0, script.len);
  g_assert_true(g_str_has_prefix(g_bytes_get_data(bytes, NULL), "export const answer = 42;"));
  g_autoptr(GBytes) tail = deck_bytes(d, script, 7, 12);
  g_assert_cmpmem(g_bytes_get_data(tail, NULL), 5, "const", 5);

  struct deck_entry missing;
  g_assert_false(deck_find(d, "web/nope.js", &missing));
  deck_web_clear(&web);
  deck_free(d);
}

static void refuses_a_compressed_entry_by_name(void) {
  gsize len;
  g_autofree char* bytes = NULL;
  g_assert_true(g_file_get_contents(fixture, &bytes, &len, NULL));
  const uint8_t central[] = {0x50, 0x4b, 0x01, 0x02};
  char* last = NULL;
  for (char* p = bytes; (p = memmem(p, len - (gsize)(p - bytes), central, 4)); p++) last = p;
  g_assert_nonnull(last);
  last[10] = 8;
  g_autofree char* path = g_build_filename(g_get_tmp_dir(), "glassdeck-deflated.deck", NULL);
  g_assert_true(g_file_set_contents(path, bytes, (gssize)len, NULL));

  g_autoptr(GError) error = NULL;
  g_assert_null(deck_open(path, &error));
  g_assert_nonnull(strstr(error->message, "compressed"));
  g_assert_nonnull(strstr(error->message, "\""));
  g_unlink(path);
}

int main(int argc, char** argv) {
  g_test_init(&argc, &argv, NULL);
  fixture = g_test_build_filename(G_TEST_DIST, "tests", "web.deck", NULL);
  g_test_add_func("/deck/reads", reads_the_converters_web_deck);
  g_test_add_func("/deck/refuses-compressed", refuses_a_compressed_entry_by_name);
  return g_test_run();
}
