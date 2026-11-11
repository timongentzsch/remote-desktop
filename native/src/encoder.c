/* Thin wrapper around x264 so the Rust side does not depend on the layout of x264's structs. */
#include <stdint.h>
#include <stdlib.h>
#include <x264.h>

/* The constant-quality target (x264's "rate factor"; lower is better). */
#define QUALITY 20

struct encoder {
    x264_t *x264;
    x264_picture_t picture;
};

struct encoder *encoder_open(int width, int height, int fps, int kbps, int full_chroma)
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

    struct encoder *encoder = calloc(1, sizeof *encoder);
    if (!encoder)
        return NULL;
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
    return encoder;
}

uint8_t *encoder_plane(struct encoder *encoder, int index, int *stride)
{
    *stride = encoder->picture.img.i_stride[index];
    return encoder->picture.img.plane[index];
}

/* Points sps/pps at the raw parameter sets (without their length prefix). */
int encoder_headers(struct encoder *encoder, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size)
{
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

/* Encodes the picture whose planes the caller filled. Returns the size of the access unit at *data. */
int encoder_encode(struct encoder *encoder, uint8_t **data, int *keyframe)
{
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

/* Changes the bitrate cap of a running encoder. Returns 0 on success. */
int encoder_set_bitrate(struct encoder *encoder, int kbps)
{
    x264_param_t param;
    x264_encoder_parameters(encoder->x264, &param);
    param.rc.i_vbv_max_bitrate = kbps;
    param.rc.i_vbv_buffer_size = kbps / 5;
    return x264_encoder_reconfig(encoder->x264, &param);
}

void encoder_close(struct encoder *encoder)
{
    x264_encoder_close(encoder->x264);
    x264_picture_clean(&encoder->picture);
    free(encoder);
}
