/* Software H.264 encoding with x264, which is built into the binary. */
#include <stdint.h>
#include <stdlib.h>
#include <x264.h>
#include "encoder.h"

struct software {
    struct encoder base;
    x264_t *x264;
    x264_picture_t picture;
    int quality; /* the target now, which tells a coarse frame */
};

static const struct encoder_ops software_ops;

/* x264's presets by effort. On text scrolling by at 3 Mbit/s each step up was worth 3 to 6 dB, for about half
   as much time again per frame. On code scrolling by at 1920x1200, superfast took 17% fewer bits than ultrafast
   for the same quality, for 0.75 ms more a frame. */
static const char *const PRESETS[EFFORTS] = { "ultrafast", "superfast", "veryfast", "faster" };

static void set_rate(x264_param_t *param, int kbps, int buffer_kbits, int quality)
{
    param->rc.i_vbv_max_bitrate = kbps;
    param->rc.i_vbv_buffer_size = buffer_kbits;
    param->rc.f_rf_constant = quality; /* x264's "rate factor" */
}

struct encoder *software_open(const struct encoder_setup *setup)
{
    x264_param_t param;
    if (x264_param_default_preset(&param, PRESETS[setup->effort], "zerolatency") < 0)
        return NULL;
    param.i_csp = setup->full_chroma ? X264_CSP_I444 : X264_CSP_I420;
    param.i_width = setup->width;
    param.i_height = setup->height;
    /* Frames arrive only when the screen changes, but x264 is told a constant rate: its variable-rate mode
       holds each frame back until the next one arrives. The bitrate cap is then an upper bound. */
    param.i_fps_num = setup->fps;
    param.i_fps_den = 1;
    param.b_vfr_input = 0;
    param.i_keyint_max = X264_KEYINT_MAX_INFINITE;
    param.b_repeat_headers = 0;
    param.b_annexb = 0; /* length-prefixed NAL units, as FLV carries them */
    /* Constant quality, capped: a still screen costs almost nothing and motion cannot outrun the link. */
    param.rc.i_rc_method = X264_RC_CRF;
    /* No adaptive quantisation: it takes bits from flat areas for busy ones, which on a screen are the plain
       backgrounds that text sits on. Without it the same rate factor gave the same quality at every effort,
       and superfast 0.4 dB more for its 17% fewer bits. */
    param.rc.i_aq_mode = X264_AQ_NONE;
    set_rate(&param, setup->kbps, BUFFER_KBITS(setup->kbps, setup->buffer), setup->quality);
    param.vui.b_fullrange = 1;
    param.vui.i_colorprim = param.vui.i_transfer = param.vui.i_colmatrix = BT709;
    param.vui.i_chroma_loc = CHROMA_AT_CENTRE; /* 4:2:0 chroma is the average of each 2x2 block */
    param.i_log_level = X264_LOG_WARNING;

    struct software *encoder = calloc(1, sizeof *encoder);
    if (!encoder)
        return NULL;
    encoder->base.ops = &software_ops;
    encoder->quality = setup->quality;
    if (x264_picture_alloc(&encoder->picture, param.i_csp, setup->width, setup->height) < 0) {
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

static int software_headers(struct encoder *base, uint8_t **sets, int *sizes)
{
    struct software *encoder = (struct software *)base;
    x264_nal_t *nal;
    int count;
    if (x264_encoder_headers(encoder->x264, &nal, &count) < 0)
        return -1;
    for (int i = 0; i < count; i++) {
        /* After the four-byte start code; SPS first, then PPS. */
        int at = nal[i].i_type == NAL_SPS ? 0 : nal[i].i_type == NAL_PPS ? 1 : -1;
        if (at >= 0) {
            sets[at] = nal[i].p_payload + 4;
            sizes[at] = nal[i].i_payload - 4;
        }
    }
    return 2;
}

static int software_encode(struct encoder *base, int fresh, uint8_t **data, int *keyframe, int *flags)
{
    struct software *encoder = (struct software *)base;
    x264_nal_t *nal;
    int count;
    x264_picture_t out;
    encoder->picture.i_pts++;
    encoder->picture.i_type = fresh ? X264_TYPE_IDR : X264_TYPE_AUTO;
    int size = x264_encoder_encode(encoder->x264, &nal, &count, &encoder->picture, &out);
    if (size > 0) {
        *data = nal[0].p_payload;
        *keyframe = out.b_keyframe;
        /* The rate factor the cap left it with. That x264 does not hold back by the budget alone: on two cores it
           reported a still picture's empty refinements as coarse. */
        *flags = out.prop.f_crf_avg > encoder->quality + COARSE_MARGIN ? FRAME_COARSE : 0;
    }
    return size;
}

static int software_set_rate(struct encoder *base, int kbps, int buffer_kbits, int quality)
{
    struct software *encoder = (struct software *)base;
    x264_param_t param;
    x264_encoder_parameters(encoder->x264, &param);
    set_rate(&param, kbps, buffer_kbits, quality);
    int failed = x264_encoder_reconfig(encoder->x264, &param);
    if (!failed)
        encoder->quality = quality;
    return failed;
}

static void software_close(struct encoder *base)
{
    struct software *encoder = (struct software *)base;
    x264_encoder_close(encoder->x264);
    x264_picture_clean(&encoder->picture);
    free(encoder);
}

static const struct encoder_ops software_ops = { "x264", software_plane, software_headers, software_encode, software_set_rate, software_close };
