/* The encoders behind one interface, so the Rust side does not depend on the layout of their structs:
   x264 here, and the GPU's encoder in nvenc.c. */
#include <stdint.h>
#include <stdlib.h>
#include <x264.h>
#include "encoder.h"

/* The constant-quality target (x264's "rate factor"; lower is better). */
#define QUALITY 20

struct software {
    struct encoder base;
    x264_t *x264;
    x264_picture_t picture;
};

static const struct encoder_ops software_ops;

static struct encoder *software_open(int width, int height, int fps, int kbps, int full_chroma)
{
    x264_param_t param;
    if (x264_param_default_preset(&param, "ultrafast", "zerolatency") < 0)
        return NULL;
    param.i_csp = full_chroma ? X264_CSP_I444 : X264_CSP_I420;
    param.i_width = width;
    param.i_height = height;
    /* Frames arrive only when the screen changes, but x264 is told a constant rate: its variable-rate mode
       holds each frame back until the next one arrives. The bitrate cap is then an upper bound. */
    param.i_fps_num = fps;
    param.i_fps_den = 1;
    param.b_vfr_input = 0;
    param.i_keyint_max = X264_KEYINT_MAX_INFINITE;
    param.b_repeat_headers = 0;
    param.b_annexb = 0; /* length-prefixed NAL units, as FLV carries them */
    /* Constant quality, capped: a still screen costs almost nothing and motion cannot outrun the link. */
    param.rc.i_rc_method = X264_RC_CRF;
    param.rc.f_rf_constant = QUALITY;
    param.rc.i_vbv_max_bitrate = kbps;
    param.rc.i_vbv_buffer_size = kbps / 5;
    param.vui.b_fullrange = 1;
    param.vui.i_colorprim = 1; /* BT.709 */
    param.vui.i_transfer = 1;
    param.vui.i_colmatrix = 1;
    param.vui.i_chroma_loc = 1; /* 4:2:0 chroma is the average of each 2x2 block, i.e. sited at its centre */
    param.i_log_level = X264_LOG_WARNING;

    struct software *encoder = calloc(1, sizeof *encoder);
    if (!encoder)
        return NULL;
    encoder->base.ops = &software_ops;
    if (x264_picture_alloc(&encoder->picture, param.i_csp, width, height) < 0) {
        free(encoder);
        return NULL;
    }
    encoder->x264 = x264_encoder_open(&param);
    if (!encoder->x264) {
        x264_picture_clean(&encoder->picture);
        free(encoder);
        return NULL;
    }
    return &encoder->base;
}

static uint8_t *software_plane(struct encoder *base, int index, int *stride)
{
    struct software *encoder = (struct software *)base;
    *stride = encoder->picture.img.i_stride[index];
    return encoder->picture.img.plane[index];
}

static int software_headers(struct encoder *base, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size)
{
    struct software *encoder = (struct software *)base;
    x264_nal_t *nal;
    int count;
    if (x264_encoder_headers(encoder->x264, &nal, &count) < 0)
        return -1;
    for (int i = 0; i < count; i++) {
        if (nal[i].i_type == NAL_SPS) {
            *sps = nal[i].p_payload + 4;
            *sps_size = nal[i].i_payload - 4;
        } else if (nal[i].i_type == NAL_PPS) {
            *pps = nal[i].p_payload + 4;
            *pps_size = nal[i].i_payload - 4;
        }
    }
    return 0;
}

static int software_encode(struct encoder *base, uint8_t **data, int *keyframe)
{
    struct software *encoder = (struct software *)base;
    x264_nal_t *nal;
    int count;
    x264_picture_t out;
    encoder->picture.i_pts++;
    int size = x264_encoder_encode(encoder->x264, &nal, &count, &encoder->picture, &out);
    if (size > 0) {
        *data = nal[0].p_payload;
        *keyframe = out.b_keyframe;
    }
    return size;
}

static int software_set_bitrate(struct encoder *base, int kbps)
{
    struct software *encoder = (struct software *)base;
    x264_param_t param;
    x264_encoder_parameters(encoder->x264, &param);
    param.rc.i_vbv_max_bitrate = kbps;
    param.rc.i_vbv_buffer_size = kbps / 5;
    return x264_encoder_reconfig(encoder->x264, &param);
}

static void software_close(struct encoder *base)
{
    struct software *encoder = (struct software *)base;
    x264_encoder_close(encoder->x264);
    x264_picture_clean(&encoder->picture);
    free(encoder);
}

static const struct encoder_ops software_ops = { software_plane, software_headers, software_encode, software_set_bitrate, software_close };

/* Opens the GPU's encoder if `gpu` is set, x264 otherwise. NULL if that one cannot be used. */
struct encoder *encoder_open(int width, int height, int fps, int kbps, int full_chroma, int gpu)
{
    return (gpu ? nvenc_open : software_open)(width, height, fps, kbps, full_chroma);
}

/* A plane of the picture to encode, for the caller to fill; it stays valid while the encoder is open. */
uint8_t *encoder_plane(struct encoder *encoder, int index, int *stride)
{
    return encoder->ops->plane(encoder, index, stride);
}

/* Points sps/pps at the raw parameter sets (without start code or length prefix). Returns 0 on success. */
int encoder_headers(struct encoder *encoder, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size)
{
    return encoder->ops->headers(encoder, sps, sps_size, pps, pps_size);
}

/* Encodes the picture in the planes. Returns the size of the access unit at *data (length-prefixed NAL
   units, valid until the next call), or 0 or less if there is none. */
int encoder_encode(struct encoder *encoder, uint8_t **data, int *keyframe)
{
    return encoder->ops->encode(encoder, data, keyframe);
}

/* Changes the bitrate cap of a running encoder. Returns 0 on success. */
int encoder_set_bitrate(struct encoder *encoder, int kbps)
{
    return encoder->ops->set_bitrate(encoder, kbps);
}

void encoder_close(struct encoder *encoder)
{
    encoder->ops->close(encoder);
}
