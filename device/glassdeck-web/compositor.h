// The receiver's compositor socket (glassdeck-proto's compositor module), renderer side.
#pragma once

#include <glib.h>
#include <stddef.h>
#include <stdint.h>

#define COMPOSITOR_VERSION 1
#define COMPOSITOR_HEADER_LEN 4
#define COMPOSITOR_MAX_MESSAGE_LEN 512

struct buffer_layout {
  uint32_t width;
  uint32_t height;
  uint32_t fourcc;
  uint32_t stride;
  uint32_t offset;
  uint64_t modifier;
};

enum compositor_reply_kind {
  COMPOSITOR_REGISTERED,
  COMPOSITOR_REFUSED,
  COMPOSITOR_RELEASED,
};

struct compositor_reply {
  enum compositor_reply_kind kind;
  uint32_t id;
  char reason[COMPOSITOR_MAX_MESSAGE_LEN];
};

size_t compositor_encode_register(uint8_t* out, const struct buffer_layout* layout);
size_t compositor_encode_present(uint8_t* out, uint32_t id);
gboolean compositor_decode_reply(const uint8_t* message, size_t len, struct compositor_reply* reply, GError** error);

typedef void (*compositor_released_fn)(void* data, uint32_t id);

struct compositor;

struct compositor* compositor_new(int fd, compositor_released_fn released, void* data);
void compositor_free(struct compositor* c);
int compositor_fd(const struct compositor* c);
// Sends the buffer and waits for its answer, handing any Released that arrives first to the callback.
gboolean compositor_register(struct compositor* c, int dmabuf_fd, const struct buffer_layout* layout, uint32_t* id,
                             GError** error);
gboolean compositor_present(struct compositor* c, uint32_t id, GError** error);
// Reads whatever has arrived; FALSE once the receiver has gone or sent something unexpected.
gboolean compositor_dispatch(struct compositor* c, GError** error);
