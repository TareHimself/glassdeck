#include <stdint.h>
#include <stdlib.h>

#include <rockchip/rk_mpi.h>

enum { GD_FRAME_NONE = 0, GD_FRAME_READY = 1, GD_FRAME_DROPPED = 2 };
enum { GD_PUT_DONE = 0, GD_PUT_FULL = 1 };

typedef struct {
    MppCtx ctx;
    MppApi *mpi;
    RK_S64 timeout_ms;
} GdDecoder;

typedef struct {
    MppFrame frame;
    int32_t fd;
    uint32_t width;
    uint32_t height;
    uint32_t hor_stride;
    uint32_t ver_stride;
    int64_t pts;
    uint32_t format;
    uint32_t is_nv12;
    uint32_t color_range;
    uint32_t colorspace;
} GdFrame;

int gd_mpp_open(int hevc, int immediate_out, GdDecoder **out) {
    GdDecoder *dec = calloc(1, sizeof(*dec));
    if (!dec) return MPP_ERR_MALLOC;
    MPP_RET ret = mpp_create(&dec->ctx, &dec->mpi);
    if (ret != MPP_OK) goto fail;
    if (immediate_out) {
        RK_U32 on = 1;
        ret = dec->mpi->control(dec->ctx, MPP_DEC_SET_IMMEDIATE_OUT, &on);
        if (ret != MPP_OK) goto fail;
    }
    ret = mpp_init(dec->ctx, MPP_CTX_DEC, hevc ? MPP_VIDEO_CodingHEVC : MPP_VIDEO_CodingAVC);
    if (ret != MPP_OK) goto fail;
    *out = dec;
    return MPP_OK;
fail:
    if (dec->ctx) mpp_destroy(dec->ctx);
    free(dec);
    return ret;
}

/* MPP copies the packet data, so the caller's buffer is free again on return. */
int gd_mpp_put(GdDecoder *dec, const uint8_t *data, size_t len, int64_t pts) {
    MppPacket packet = NULL;
    MPP_RET ret = mpp_packet_init(&packet, (void *)data, len);
    if (ret != MPP_OK) return ret;
    mpp_packet_set_pts(packet, pts);
    ret = dec->mpi->decode_put_packet(dec->ctx, packet);
    mpp_packet_deinit(&packet);
    if (ret == MPP_ERR_BUFFER_FULL) return GD_PUT_FULL;
    return ret == MPP_OK ? GD_PUT_DONE : ret;
}

int gd_mpp_get(GdDecoder *dec, int64_t timeout_ms, GdFrame *out) {
    if (timeout_ms != dec->timeout_ms) {
        RK_S64 timeout = timeout_ms;
        MPP_RET ret = dec->mpi->control(dec->ctx, MPP_SET_OUTPUT_TIMEOUT, &timeout);
        if (ret != MPP_OK) return ret;
        dec->timeout_ms = timeout_ms;
    }
    for (;;) {
        MppFrame frame = NULL;
        MPP_RET ret = dec->mpi->decode_get_frame(dec->ctx, &frame);
        if (ret == MPP_ERR_TIMEOUT) return GD_FRAME_NONE;
        if (ret != MPP_OK) return ret;
        if (!frame) return GD_FRAME_NONE;

        if (mpp_frame_get_info_change(frame)) {
            mpp_frame_deinit(&frame);
            ret = dec->mpi->control(dec->ctx, MPP_DEC_SET_INFO_CHANGE_READY, NULL);
            if (ret != MPP_OK) return ret;
            continue;
        }
        MppBuffer buffer = mpp_frame_get_buffer(frame);
        if (!buffer) {
            mpp_frame_deinit(&frame);
            return GD_FRAME_NONE;
        }
        if (mpp_frame_get_errinfo(frame) || mpp_frame_get_discard(frame)) {
            mpp_frame_deinit(&frame);
            return GD_FRAME_DROPPED;
        }
        out->frame = frame;
        out->fd = mpp_buffer_get_fd(buffer);
        out->width = mpp_frame_get_width(frame);
        out->height = mpp_frame_get_height(frame);
        out->hor_stride = mpp_frame_get_hor_stride(frame);
        out->ver_stride = mpp_frame_get_ver_stride(frame);
        out->pts = mpp_frame_get_pts(frame);
        MppFrameFormat format = mpp_frame_get_fmt(frame);
        out->format = format;
        out->is_nv12 = (format & MPP_FRAME_FMT_MASK) == MPP_FMT_YUV420SP && !MPP_FRAME_FMT_IS_FBC(format);
        out->color_range = mpp_frame_get_color_range(frame);
        out->colorspace = mpp_frame_get_colorspace(frame);
        return GD_FRAME_READY;
    }
}

void gd_mpp_frame_release(MppFrame frame) {
    mpp_frame_deinit(&frame);
}

void gd_mpp_close(GdDecoder *dec) {
    dec->mpi->reset(dec->ctx);
    mpp_destroy(dec->ctx);
    free(dec);
}
