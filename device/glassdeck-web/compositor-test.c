// The same bytes as glassdeck-proto's wire_layout_is_fixed test, from the C side.
#include <string.h>

#include "compositor.h"

static const uint8_t REGISTER[] = {
    1, 1, 0, 0,                                     // header
    0x80, 0x07, 0, 0, 0x38, 0x04, 0, 0,             // 1920, 1080
    'X', 'R', '2', '4', 0x00, 0x1e, 0, 0,           // XR24, stride 7680
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,             // offset 0, modifier 0
};
static const uint8_t PRESENT_258[] = {1, 2, 0, 0, 2, 1, 0, 0};
static const uint8_t REGISTERED_258[] = {1, 3, 0, 0, 2, 1, 0, 0};
static const uint8_t REFUSED_NO[] = {1, 4, 0, 0, 'n', 'o'};
static const uint8_t RELEASED_258[] = {1, 5, 0, 0, 2, 1, 0, 0};

static void encodes_register_and_present(void) {
  uint8_t out[COMPOSITOR_MAX_MESSAGE_LEN];
  struct buffer_layout layout = {1920, 1080, 0x34325258, 7680, 0, 0};
  g_assert_cmpmem(out, compositor_encode_register(out, &layout), REGISTER, sizeof REGISTER);
  g_assert_cmpmem(out, compositor_encode_present(out, 258), PRESENT_258, sizeof PRESENT_258);
}

static void decodes_replies(void) {
  struct compositor_reply reply;
  g_assert_true(compositor_decode_reply(REGISTERED_258, sizeof REGISTERED_258, &reply, NULL));
  g_assert_cmpint(reply.kind, ==, COMPOSITOR_REGISTERED);
  g_assert_cmpuint(reply.id, ==, 258);
  g_assert_true(compositor_decode_reply(RELEASED_258, sizeof RELEASED_258, &reply, NULL));
  g_assert_cmpint(reply.kind, ==, COMPOSITOR_RELEASED);
  g_assert_true(compositor_decode_reply(REFUSED_NO, sizeof REFUSED_NO, &reply, NULL));
  g_assert_cmpint(reply.kind, ==, COMPOSITOR_REFUSED);
  g_assert_cmpstr(reply.reason, ==, "no");
}

static void refuses_bad_replies(void) {
  struct compositor_reply reply;
  const uint8_t short_header[] = {1, 3};
  const uint8_t wrong_version[] = {2, 3, 0, 0, 1, 0, 0, 0};
  const uint8_t unknown_kind[] = {1, 9, 0, 0};
  const uint8_t truncated_id[] = {1, 5, 0, 0, 1};
  const uint8_t bad_utf8[] = {1, 4, 0, 0, 0xff};
  const uint8_t* bad[] = {short_header, wrong_version, unknown_kind, truncated_id, bad_utf8};
  const size_t lens[] = {sizeof short_header, sizeof wrong_version, sizeof unknown_kind, sizeof truncated_id, sizeof bad_utf8};
  for (size_t i = 0; i < G_N_ELEMENTS(bad); i++) {
    GError* error = NULL;
    g_assert_false(compositor_decode_reply(bad[i], lens[i], &reply, &error));
    g_assert_nonnull(error);
    g_error_free(error);
  }
}

int main(int argc, char** argv) {
  g_test_init(&argc, &argv, NULL);
  g_test_add_func("/compositor/encodes", encodes_register_and_present);
  g_test_add_func("/compositor/decodes", decodes_replies);
  g_test_add_func("/compositor/refuses", refuses_bad_replies);
  return g_test_run();
}
