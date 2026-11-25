#include "compositor.h"

#include <errno.h>
#include <gio/gio.h>
#include <string.h>
#include <sys/socket.h>

#define KIND_REGISTER 1
#define KIND_PRESENT 2
#define KIND_REGISTERED 3
#define KIND_REFUSED 4
#define KIND_RELEASED 5
#define REGISTER_BODY_LEN 28
#define ID_LEN 4

struct compositor {
  int fd;
  compositor_released_fn released;
  void* data;
};

static void put_u32(uint8_t* out, uint32_t value) {
  for (int i = 0; i < 4; i++) out[i] = (uint8_t)(value >> (8 * i));
}

static uint32_t get_u32(const uint8_t* in) {
  return (uint32_t)in[0] | (uint32_t)in[1] << 8 | (uint32_t)in[2] << 16 | (uint32_t)in[3] << 24;
}

static size_t put_header(uint8_t* out, uint8_t kind) {
  out[0] = COMPOSITOR_VERSION;
  out[1] = kind;
  out[2] = 0;
  out[3] = 0;
  return COMPOSITOR_HEADER_LEN;
}

size_t compositor_encode_register(uint8_t* out, const struct buffer_layout* layout) {
  size_t at = put_header(out, KIND_REGISTER);
  const uint32_t fields[] = {layout->width, layout->height, layout->fourcc, layout->stride, layout->offset};
  for (size_t i = 0; i < G_N_ELEMENTS(fields); i++, at += 4) put_u32(out + at, fields[i]);
  put_u32(out + at, (uint32_t)layout->modifier);
  put_u32(out + at + 4, (uint32_t)(layout->modifier >> 32));
  return at + 8;
}

size_t compositor_encode_present(uint8_t* out, uint32_t id) {
  size_t at = put_header(out, KIND_PRESENT);
  put_u32(out + at, id);
  return at + ID_LEN;
}

gboolean compositor_decode_reply(const uint8_t* message, size_t len, struct compositor_reply* reply, GError** error) {
  if (len < COMPOSITOR_HEADER_LEN) {
    g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "compositor message of %zu bytes is shorter than its header", len);
    return FALSE;
  }
  if (message[0] != COMPOSITOR_VERSION) {
    g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "compositor protocol version %u, expected %u", message[0],
                COMPOSITOR_VERSION);
    return FALSE;
  }
  const uint8_t* body = message + COMPOSITOR_HEADER_LEN;
  size_t body_len = len - COMPOSITOR_HEADER_LEN;
  switch (message[1]) {
    case KIND_REGISTERED:
    case KIND_RELEASED:
      if (body_len != ID_LEN) break;
      reply->kind = message[1] == KIND_REGISTERED ? COMPOSITOR_REGISTERED : COMPOSITOR_RELEASED;
      reply->id = get_u32(body);
      return TRUE;
    case KIND_REFUSED:
      if (!g_utf8_validate((const char*)body, (gssize)body_len, NULL)) break;
      reply->kind = COMPOSITOR_REFUSED;
      memcpy(reply->reason, body, body_len);
      reply->reason[body_len] = '\0';
      return TRUE;
    default:
      g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "unknown compositor message kind %u", message[1]);
      return FALSE;
  }
  g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "malformed compositor message kind %u (%zu-byte body)",
              message[1], body_len);
  return FALSE;
}

struct compositor* compositor_new(int fd, compositor_released_fn released, void* data) {
  struct compositor* c = g_new0(struct compositor, 1);
  *c = (struct compositor){.fd = fd, .released = released, .data = data};
  return c;
}

void compositor_free(struct compositor* c) {
  g_free(c);
}

int compositor_fd(const struct compositor* c) {
  return c->fd;
}

static gboolean send_message(struct compositor* c, const uint8_t* message, size_t len, int fd_to_pass, GError** error) {
  struct iovec iov = {.iov_base = (void*)message, .iov_len = len};
  union {
    char buffer[CMSG_SPACE(sizeof(int))];
    struct cmsghdr align;
  } control;
  struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1};
  if (fd_to_pass >= 0) {
    msg.msg_control = control.buffer;
    msg.msg_controllen = sizeof control.buffer;
    struct cmsghdr* header = CMSG_FIRSTHDR(&msg);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &fd_to_pass, sizeof(int));
  }
  if (sendmsg(c->fd, &msg, MSG_NOSIGNAL) != (ssize_t)len) {
    g_set_error(error, G_IO_ERROR, g_io_error_from_errno(errno), "sending to the compositor: %s", g_strerror(errno));
    return FALSE;
  }
  return TRUE;
}

static gboolean receive(struct compositor* c, int flags, struct compositor_reply* reply, gboolean* got, GError** error) {
  uint8_t message[COMPOSITOR_MAX_MESSAGE_LEN];
  ssize_t n = recv(c->fd, message, sizeof message, flags);
  *got = FALSE;
  if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) && (flags & MSG_DONTWAIT)) return TRUE;
  if (n < 0) {
    g_set_error(error, G_IO_ERROR, g_io_error_from_errno(errno), "reading from the compositor: %s", g_strerror(errno));
    return FALSE;
  }
  if (n == 0) {
    g_set_error(error, G_IO_ERROR, G_IO_ERROR_CLOSED, "the compositor closed the socket");
    return FALSE;
  }
  *got = TRUE;
  return compositor_decode_reply(message, (size_t)n, reply, error);
}

gboolean compositor_register(struct compositor* c, int dmabuf_fd, const struct buffer_layout* layout, uint32_t* id,
                             GError** error) {
  uint8_t message[COMPOSITOR_MAX_MESSAGE_LEN];
  if (!send_message(c, message, compositor_encode_register(message, layout), dmabuf_fd, error)) return FALSE;
  for (;;) {
    struct compositor_reply reply;
    gboolean got;
    if (!receive(c, 0, &reply, &got, error)) return FALSE;
    switch (reply.kind) {
      case COMPOSITOR_RELEASED:
        c->released(c->data, reply.id);
        continue;
      case COMPOSITOR_REGISTERED:
        *id = reply.id;
        return TRUE;
      case COMPOSITOR_REFUSED:
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED, "the compositor refused the buffer: %s", reply.reason);
        return FALSE;
    }
  }
}

gboolean compositor_present(struct compositor* c, uint32_t id, GError** error) {
  uint8_t message[COMPOSITOR_MAX_MESSAGE_LEN];
  return send_message(c, message, compositor_encode_present(message, id), -1, error);
}

gboolean compositor_dispatch(struct compositor* c, GError** error) {
  for (;;) {
    struct compositor_reply reply;
    gboolean got;
    if (!receive(c, MSG_DONTWAIT, &reply, &got, error)) return FALSE;
    if (!got) return TRUE;
    if (reply.kind != COMPOSITOR_RELEASED) {
      g_set_error(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "the compositor answered a registration nobody asked for");
      return FALSE;
    }
    c->released(c->data, reply.id);
  }
}
