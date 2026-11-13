/* Hardware H.264 encoding through VA-API, on Intel's and AMD's GPUs. libva is loaded at run time, so the
   binary needs it neither to build nor to start; where it is missing, or no GPU here encodes H.264 as this
   file needs, vaapi_open fails and the caller uses another encoder.

   VA-API encodes what it is told to: this file writes the stream's parameter sets itself, which the stream
   needs for its decoder configuration anyway, and gives the driver every frame's picture and slice. Each frame
   refers to the one before; the first, and one a client needs to begin with, is an IDR. Only 4:2:0.

   The bitrate is this file's to keep, around a constant quantiser: the drivers' own rate controls that aim
   for a quality under a cap fell well short of the quality (Intel's QVBR began at quantiser 48 where 20 was
   asked for), and left an unchanged picture as it was where it was to be refined. Each frame is given the
   quality target's quantiser unless the frames before it have used up what the cap allows; see `choose_qp`. */
#include "encoder.h"

#ifndef __linux__
struct encoder *vaapi_open(const struct encoder_setup *setup)
{
    (void)setup;
    return NULL;
}
#else

#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <va/va.h>
#include "nal.h"

/* The render nodes looked at for a GPU that encodes: /dev/dri/renderD128 and the ones after it. */
#define RENDER_FIRST 128
#define RENDER_NODES 8
#define PROFILE VAProfileH264High
#define ENTRYPOINT VAEntrypointEncSlice
/* H.264 level 5.2: pictures of up to 36864 macroblocks, 4096x2304. */
#define LEVEL 52
#define MOST_MACROBLOCKS 36864
#define MACROBLOCK 16
/* The frame number counts up to 2^16 before it wraps; frames have nothing to say with it but their order. */
#define LOG2_MAX_FRAME_NUM 16
/* No IDR comes unasked: the driver is given one as long a period as it takes. */
#define NO_PERIOD 0x7FFFFFFF
/* H.264's coarsest quantiser; and how much smaller a frame comes out a quantiser step coarser, about: six
   steps halve it, so 2^(1/6). */
#define MOST_QP 51
#define QP_STEP 1.12246
/* How many steps finer than the last frame's the next frame's quantiser may be. Refining a still picture, a
   frame's size at a finer quantiser depends on how much finer it is than what the picture has, which the last
   frame's size does not foresee; a large step would come out too large for the cap and be thrown away whole. */
#define MOST_QP_FALL 2
/* The driver's speed against quality, 1 the best and 7 the fastest. */
#define SPEED 4
/* The picture's quantiser before the rate control adjusts it, as PPS and pictures state it. */
#define INITIAL_QP 26
/* Slice types (H.264, table 7-6). */
#define SLICE_P 0
#define SLICE_I 2
/* Surfaces: the picture to encode, and two the driver reconstructs frames into, the one being made and the
   one it refers to. */
#define INPUT 0
#define SURFACES 3

#define FUNCTIONS(F) \
    F(vaInitialize) F(vaTerminate) F(vaSetErrorCallback) F(vaSetInfoCallback) F(vaGetConfigAttributes) \
    F(vaCreateConfig) F(vaDestroyConfig) F(vaCreateSurfaces) F(vaDestroySurfaces) F(vaCreateContext) \
    F(vaDestroyContext) F(vaCreateBuffer) F(vaDestroyBuffer) F(vaMapBuffer) F(vaUnmapBuffer) F(vaBeginPicture) \
    F(vaRenderPicture) F(vaEndPicture) F(vaSyncSurface) F(vaDeriveImage) F(vaDestroyImage)

static struct {
    VADisplay (*vaGetDisplayDRM)(int fd);
#define F(name) __typeof__(name) *name;
    FUNCTIONS(F)
#undef F
} va;

/* The display of the GPU that encodes, made once and kept. Calls into libva take turns: one display serves
   every session's encoder. */
static VADisplay display;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_mutex_t calls = PTHREAD_MUTEX_INITIALIZER;
static int loaded;

struct vaapi {
    struct encoder base;
    VAConfigID config;
    VAContextID context;
    VASurfaceID surfaces[SURFACES];
    VABufferID coded;
    int width, height, width_mbs, height_mbs;
    int kbps, buffer_kbits, quality;
    /* What the cap allows to be sent now, in bits: it fills by the cap's share of each frame up to the size of
       the buffer, and each frame empties it by its size. And the quantiser and size of the last frame. */
    double allowance;
    int last_qp, last_bits;
    /* Counted from the last IDR, which is numbered by idr_id; reference is the surface the last frame was
       reconstructed into, 0 before the first. */
    uint32_t frame_num, frames, idr_id;
    int reference;
    /* The picture as the caller fills it, 4:2:0 in three planes; it is copied into the input surface. */
    uint8_t *planes[3];
    struct nal_units unit;
    /* The parameter sets as NAL units after start codes, as the driver writes them into the stream. */
    uint8_t sps[BITS_ROOM * 2], pps[BITS_ROOM * 2];
    int sps_size, pps_size;
};

static const struct encoder_ops vaapi_ops;

/* libva's messages, which say for instance which driver it tries, are not this program's to show. */
static void quiet(void *context, const char *message)
{
    (void)context;
    (void)message;
}

/* Whether the GPU behind `display` encodes as this file needs. */
static int suits(VADisplay candidate)
{
    VAConfigAttrib wanted[] = {
        { .type = VAConfigAttribRTFormat },
        { .type = VAConfigAttribRateControl },
        { .type = VAConfigAttribEncPackedHeaders },
    };
    if (va.vaGetConfigAttributes(candidate, PROFILE, ENTRYPOINT, wanted, 3) != VA_STATUS_SUCCESS)
        return 0;
    const unsigned packed = VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_PICTURE;
    return wanted[0].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[0].value & VA_RT_FORMAT_YUV420)
        && wanted[1].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[1].value & VA_RC_CQP)
        && wanted[2].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[2].value & packed) == packed;
}

static void load(void)
{
    void *library = dlopen("libva.so.2", RTLD_NOW), *drm = dlopen("libva-drm.so.2", RTLD_NOW);
    if (!library || !drm || !(va.vaGetDisplayDRM = dlsym(drm, "vaGetDisplayDRM")))
        return;
#define F(name) \
    if (!(va.name = dlsym(library, #name))) \
        return;
    FUNCTIONS(F)
#undef F
    for (int node = RENDER_FIRST; node < RENDER_FIRST + RENDER_NODES; node++) {
        char path[32];
        snprintf(path, sizeof path, "/dev/dri/renderD%d", node);
        int fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd < 0)
            continue;
        VADisplay candidate = va.vaGetDisplayDRM(fd);
        int major, minor;
        if (candidate) {
            va.vaSetErrorCallback(candidate, quiet, NULL);
            va.vaSetInfoCallback(candidate, quiet, NULL);
        }
        if (candidate && va.vaInitialize(candidate, &major, &minor) == VA_STATUS_SUCCESS) {
            if (suits(candidate)) {
                display = candidate; /* kept open with its file for the process's lifetime */
                loaded = 1;
                return;
            }
            va.vaTerminate(candidate);
        }
        close(fd);
    }
}

/* The sequence parameter set: High profile, 4:2:0, frames referring to one before them, the picture's size
   and the part of its macroblocks that is shown, and how its colours are meant (as encoder.h says). Its VUI
   says that frames come in the order they are shown, so that a decoder shows each one at once. */
static void write_sps(struct vaapi *encoder, int fps)
{
    struct bits sps = { 0 };
    bits_put(&sps, 100, 8); /* profile_idc: High */
    bits_put(&sps, 0, 8);   /* constraint flags */
    bits_put(&sps, LEVEL, 8);
    bits_put_ue(&sps, 0); /* seq_parameter_set_id */
    bits_put_ue(&sps, 1); /* chroma_format_idc: 4:2:0 */
    bits_put_ue(&sps, 0); /* bit_depth_luma_minus8 */
    bits_put_ue(&sps, 0); /* bit_depth_chroma_minus8 */
    bits_put(&sps, 0, 1); /* qpprime_y_zero_transform_bypass_flag */
    bits_put(&sps, 0, 1); /* seq_scaling_matrix_present_flag */
    bits_put_ue(&sps, LOG2_MAX_FRAME_NUM - 4);
    bits_put_ue(&sps, 2); /* pic_order_cnt_type: in the order of frame_num */
    bits_put_ue(&sps, 1); /* max_num_ref_frames */
    bits_put(&sps, 0, 1); /* gaps_in_frame_num_value_allowed_flag */
    bits_put_ue(&sps, encoder->width_mbs - 1);
    bits_put_ue(&sps, encoder->height_mbs - 1);
    bits_put(&sps, 1, 1); /* frame_mbs_only_flag */
    bits_put(&sps, 1, 1); /* direct_8x8_inference_flag */
    /* Cropping is counted in pairs of pixels for 4:2:0. */
    int right = (encoder->width_mbs * MACROBLOCK - encoder->width) / 2;
    int bottom = (encoder->height_mbs * MACROBLOCK - encoder->height) / 2;
    bits_put(&sps, right || bottom, 1);
    if (right || bottom) {
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, right);
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, bottom);
    }
    bits_put(&sps, 1, 1); /* vui_parameters_present_flag */
    bits_put(&sps, 0, 1); /* aspect_ratio_info_present_flag */
    bits_put(&sps, 0, 1); /* overscan_info_present_flag */
    bits_put(&sps, 1, 1); /* video_signal_type_present_flag */
    bits_put(&sps, 5, 3); /* video_format: unspecified */
    bits_put(&sps, 1, 1); /* video_full_range_flag */
    bits_put(&sps, 1, 1); /* colour_description_present_flag */
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, 1, 1); /* chroma_loc_info_present_flag */
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put(&sps, 1, 1);       /* timing_info_present_flag */
    bits_put(&sps, 1, 32);      /* num_units_in_tick */
    bits_put(&sps, 2 * fps, 32); /* time_scale: two ticks a frame */
    bits_put(&sps, 0, 1);       /* fixed_frame_rate_flag */
    bits_put(&sps, 0, 1);       /* nal_hrd_parameters_present_flag */
    bits_put(&sps, 0, 1);       /* vcl_hrd_parameters_present_flag */
    bits_put(&sps, 0, 1);       /* pic_struct_present_flag */
    bits_put(&sps, 1, 1);       /* bitstream_restriction_flag */
    bits_put(&sps, 1, 1);       /* motion_vectors_over_pic_boundaries_flag */
    bits_put_ue(&sps, 0);       /* max_bytes_per_pic_denom */
    bits_put_ue(&sps, 0);       /* max_bits_per_mb_denom */
    bits_put_ue(&sps, 15);      /* log2_max_mv_length_horizontal */
    bits_put_ue(&sps, 15);      /* log2_max_mv_length_vertical */
    bits_put_ue(&sps, 0);       /* max_num_reorder_frames */
    bits_put_ue(&sps, 1);       /* max_dec_frame_buffering */
    bits_trailing(&sps);
    encoder->sps_size = nal_write(encoder->sps, sizeof encoder->sps, NAL_SPS, &sps);
}

/* The picture parameter set: CABAC, one reference, 8x8 transforms, and deblocking the slices may adjust. */
static void write_pps(struct vaapi *encoder)
{
    struct bits pps = { 0 };
    bits_put_ue(&pps, 0); /* pic_parameter_set_id */
    bits_put_ue(&pps, 0); /* seq_parameter_set_id */
    bits_put(&pps, 1, 1); /* entropy_coding_mode_flag: CABAC */
    bits_put(&pps, 0, 1); /* bottom_field_pic_order_in_frame_present_flag */
    bits_put_ue(&pps, 0); /* num_slice_groups_minus1 */
    bits_put_ue(&pps, 0); /* num_ref_idx_l0_default_active_minus1 */
    bits_put_ue(&pps, 0); /* num_ref_idx_l1_default_active_minus1 */
    bits_put(&pps, 0, 1); /* weighted_pred_flag */
    bits_put(&pps, 0, 2); /* weighted_bipred_idc */
    bits_put_se(&pps, INITIAL_QP - 26);
    bits_put_se(&pps, 0); /* pic_init_qs_minus26 */
    bits_put_se(&pps, 0); /* chroma_qp_index_offset */
    bits_put(&pps, 1, 1); /* deblocking_filter_control_present_flag */
    bits_put(&pps, 0, 1); /* constrained_intra_pred_flag */
    bits_put(&pps, 0, 1); /* redundant_pic_cnt_present_flag */
    bits_put(&pps, 1, 1); /* transform_8x8_mode_flag */
    bits_put(&pps, 0, 1); /* pic_scaling_matrix_present_flag */
    bits_put_se(&pps, 0); /* second_chroma_qp_index_offset */
    bits_trailing(&pps);
    encoder->pps_size = nal_write(encoder->pps, sizeof encoder->pps, NAL_PPS, &pps);
}

/* Makes a parameter buffer of `type` with a copy of `data` and adds it to `buffers`. */
static int add(struct vaapi *encoder, VABufferType type, const void *data, unsigned size, VABufferID *buffers, int *count)
{
    VABufferID buffer;
    if (va.vaCreateBuffer(display, encoder->context, type, size, 1, (void *)data, &buffer) != VA_STATUS_SUCCESS)
        return -1;
    buffers[(*count)++] = buffer;
    return 0;
}

/* A parameter set the driver writes into the stream as it is. */
static int add_packed(struct vaapi *encoder, unsigned kind, const uint8_t *nal, int size, VABufferID *buffers, int *count)
{
    VAEncPackedHeaderParameterBuffer header = { .type = kind, .bit_length = size * 8, .has_emulation_bytes = 1 };
    if (add(encoder, VAEncPackedHeaderParameterBufferType, &header, sizeof header, buffers, count) < 0)
        return -1;
    return add(encoder, VAEncPackedHeaderDataBufferType, nal, size, buffers, count);
}

/* One of the encoder's settings, as libva takes them: a type and its value in one buffer. */
static int add_misc(struct vaapi *encoder, VAEncMiscParameterType type, const void *value, unsigned size, VABufferID *buffers, int *count)
{
    uint8_t misc[sizeof(VAEncMiscParameterBuffer) + 64] = { 0 };
    ((VAEncMiscParameterBuffer *)misc)->type = type;
    memcpy(((VAEncMiscParameterBuffer *)misc)->data, value, size);
    return add(encoder, VAEncMiscParameterBufferType, misc, sizeof(VAEncMiscParameterBuffer) + size, buffers, count);
}

/* The sequence, and the driver's speed against quality. They go with each IDR. */
static int add_sequence(struct vaapi *encoder, VABufferID *buffers, int *count)
{
    VAEncSequenceParameterBufferH264 sequence = {
        .level_idc = LEVEL,
        .intra_period = NO_PERIOD,
        .intra_idr_period = NO_PERIOD,
        .ip_period = 1,
        .max_num_ref_frames = 1,
        .picture_width_in_mbs = encoder->width_mbs,
        .picture_height_in_mbs = encoder->height_mbs,
        .seq_fields.bits = {
            .chroma_format_idc = 1,
            .frame_mbs_only_flag = 1,
            .direct_8x8_inference_flag = 1,
            .log2_max_frame_num_minus4 = LOG2_MAX_FRAME_NUM - 4,
            .pic_order_cnt_type = 2,
        },
        .frame_cropping_flag = encoder->width % MACROBLOCK || encoder->height % MACROBLOCK,
        .frame_crop_right_offset = (encoder->width_mbs * MACROBLOCK - encoder->width) / 2,
        .frame_crop_bottom_offset = (encoder->height_mbs * MACROBLOCK - encoder->height) / 2,
        .vui_parameters_present_flag = 1,
        .vui_fields.bits = {
            .timing_info_present_flag = 1,
            .bitstream_restriction_flag = 1,
            .log2_max_mv_length_horizontal = 15,
            .log2_max_mv_length_vertical = 15,
            .motion_vectors_over_pic_boundaries_flag = 1,
        },
        .num_units_in_tick = 1,
        .time_scale = 2 * encoder->base.fps,
    };
    VAEncMiscParameterBufferQualityLevel speed = { .quality_level = SPEED };
    if (add(encoder, VAEncSequenceParameterBufferType, &sequence, sizeof sequence, buffers, count) < 0
        || add_misc(encoder, VAEncMiscParameterTypeQualityLevel, &speed, sizeof speed, buffers, count) < 0)
        return -1;
    return 0;
}

/* The quantiser for the next frame: the quality target's, unless the last frame's size says that at it the
   frame would take more than the cap allows now; then the finest that fits. A first frame has nothing to go
   by, and is held to the cap after the fact (see vaapi_encode). */
static int choose_qp(const struct vaapi *encoder)
{
    if (!encoder->last_bits)
        return encoder->quality;
    int qp = encoder->quality;
    double predicted = encoder->last_bits;
    /* The last frame's size at this quantiser. */
    for (int step = qp; step < encoder->last_qp; step++)
        predicted *= QP_STEP;
    for (int step = encoder->last_qp; step < qp; step++)
        predicted /= QP_STEP;
    while (qp < MOST_QP && predicted > encoder->allowance) {
        qp++;
        predicted /= QP_STEP;
    }
    return qp < encoder->last_qp - MOST_QP_FALL ? encoder->last_qp - MOST_QP_FALL : qp;
}

/* Copies the caller's planes into the input surface, the colour planes interleaved as NV12 has them. */
static int upload(struct vaapi *encoder)
{
    VAImage image;
    uint8_t *mapped;
    if (va.vaDeriveImage(display, encoder->surfaces[INPUT], &image) != VA_STATUS_SUCCESS)
        return -1;
    int mapped_ok = va.vaMapBuffer(display, image.buf, (void **)&mapped) == VA_STATUS_SUCCESS;
    if (mapped_ok) {
        for (int row = 0; row < encoder->height; row++)
            memcpy(mapped + image.offsets[0] + (size_t)row * image.pitches[0], encoder->planes[0] + (size_t)row * encoder->width, encoder->width);
        int half = encoder->width / 2;
        for (int row = 0; row < encoder->height / 2; row++) {
            uint8_t *to = mapped + image.offsets[1] + (size_t)row * image.pitches[1];
            const uint8_t *u = encoder->planes[1] + (size_t)row * half, *v = encoder->planes[2] + (size_t)row * half;
            for (int x = 0; x < half; x++) {
                to[2 * x] = u[x];
                to[2 * x + 1] = v[x];
            }
        }
        va.vaUnmapBuffer(display, image.buf);
    }
    va.vaDestroyImage(display, image.image_id);
    return mapped_ok ? 0 : -1;
}

/* A picture entry that refers to nothing. */
static VAPictureH264 nothing(void)
{
    return (VAPictureH264){ .picture_id = VA_INVALID_SURFACE, .flags = VA_PICTURE_H264_INVALID };
}

/* Encodes the picture in the input surface at quantiser `qp` into the surface `current`, as an IDR or as a
   frame that refers to the last one, and leaves its length-prefixed NAL units in encoder->unit. Returns their
   size, or -1. */
static int encode_at(struct vaapi *encoder, int idr, int current, int qp)
{
    int size = -1, count = 0;
    VABufferID buffers[8];
    VAEncPictureParameterBufferH264 picture = {
        .CurrPic = {
            .picture_id = encoder->surfaces[current],
            .frame_idx = encoder->frame_num,
            .TopFieldOrderCnt = 2 * encoder->frames,
            .BottomFieldOrderCnt = 2 * encoder->frames,
        },
        .coded_buf = encoder->coded,
        .frame_num = encoder->frame_num,
        .pic_init_qp = INITIAL_QP,
        .pic_fields.bits = {
            .idr_pic_flag = idr,
            .reference_pic_flag = 1,
            .entropy_coding_mode_flag = 1,
            .transform_8x8_mode_flag = 1,
            .deblocking_filter_control_present_flag = 1,
        },
    };
    VAEncSliceParameterBufferH264 slice = {
        .num_macroblocks = encoder->width_mbs * encoder->height_mbs,
        .slice_type = idr ? SLICE_I : SLICE_P,
        .idr_pic_id = encoder->idr_id,
        .direct_spatial_mv_pred_flag = 1,
        .slice_qp_delta = qp - INITIAL_QP,
    };
    for (int at = 0; at < 16; at++)
        picture.ReferenceFrames[at] = nothing();
    for (int at = 0; at < 32; at++)
        slice.RefPicList0[at] = slice.RefPicList1[at] = nothing();
    if (!idr) {
        VAPictureH264 before = {
            .picture_id = encoder->surfaces[encoder->reference],
            .frame_idx = encoder->frame_num - 1,
            .flags = VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            .TopFieldOrderCnt = 2 * (encoder->frames - 1),
            .BottomFieldOrderCnt = 2 * (encoder->frames - 1),
        };
        picture.ReferenceFrames[0] = slice.RefPicList0[0] = before;
    }
    if ((idr && (add_sequence(encoder, buffers, &count) < 0
                 || add_packed(encoder, VAEncPackedHeaderSequence, encoder->sps, encoder->sps_size, buffers, &count) < 0
                 || add_packed(encoder, VAEncPackedHeaderPicture, encoder->pps, encoder->pps_size, buffers, &count) < 0))
        || add(encoder, VAEncPictureParameterBufferType, &picture, sizeof picture, buffers, &count) < 0
        || add(encoder, VAEncSliceParameterBufferType, &slice, sizeof slice, buffers, &count) < 0)
        goto done;
    if (va.vaBeginPicture(display, encoder->context, encoder->surfaces[INPUT]) != VA_STATUS_SUCCESS)
        goto done;
    int rendered = va.vaRenderPicture(display, encoder->context, buffers, count) == VA_STATUS_SUCCESS;
    if (va.vaEndPicture(display, encoder->context) != VA_STATUS_SUCCESS || !rendered)
        goto done;
    VACodedBufferSegment *segment;
    if (va.vaSyncSurface(display, encoder->surfaces[INPUT]) != VA_STATUS_SUCCESS
        || va.vaMapBuffer(display, encoder->coded, (void **)&segment) != VA_STATUS_SUCCESS)
        goto done;
    /* The coded frame may come in several segments; one after another they are the byte stream. */
    uint8_t *joined = NULL;
    int length = 0;
    for (VACodedBufferSegment *part = segment; part; part = part->next) {
        uint8_t *grown = realloc(joined, length + part->size);
        if (!grown) {
            length = -1;
            break;
        }
        joined = grown;
        memcpy(joined + length, part->buf, part->size);
        length += part->size;
    }
    va.vaUnmapBuffer(display, encoder->coded);
    if (length >= 0)
        size = nal_length_prefixed(&encoder->unit, joined, length);
    free(joined);
done:
    for (int at = 0; at < count; at++)
        va.vaDestroyBuffer(display, buffers[at]);
    return size;
}

static int vaapi_encode(struct encoder *base, int fresh, uint8_t **data, int *keyframe, int *coarse)
{
    struct vaapi *encoder = (struct vaapi *)base;
    int idr = fresh || encoder->reference == 0, size = -1;
    if (idr) {
        encoder->frame_num = encoder->frames = 0;
        encoder->idr_id++;
    }
    /* What the cap allows fills by its share of a frame, up to the buffer's size. */
    double buffer = encoder->buffer_kbits * 1000.0, share = encoder->kbps * 1000.0 / encoder->base.fps;
    encoder->allowance = encoder->allowance + share < buffer ? encoder->allowance + share : buffer;
    /* The new frame is reconstructed into whichever of the two surfaces the last one is not in. */
    int current = encoder->reference == 1 ? 2 : 1, qp = choose_qp(encoder);
    pthread_mutex_lock(&calls);
    if (upload(encoder) == 0)
        size = encode_at(encoder, idr, current, qp);
    /* A frame that took more than twice what the cap allows now, which the last frame's size could not
       foresee, is encoded once more, as much coarser as it needs to fit. */
    double allowed = encoder->allowance > share ? encoder->allowance : share;
    if (size > 0 && size * 8.0 > 2 * allowed && qp < MOST_QP) {
        for (double over = size * 8.0 / allowed; over > 1 && qp < MOST_QP; over /= QP_STEP)
            qp++;
        size = encode_at(encoder, idr, current, qp);
    }
    pthread_mutex_unlock(&calls);
    if (size <= 0)
        return size;
    *data = encoder->unit.bytes;
    *keyframe = idr;
    *coarse = qp > encoder->quality + COARSE_MARGIN; /* the cap, not the quality target, set the quantiser */
    encoder->last_qp = qp;
    encoder->last_bits = size * 8;
    encoder->allowance -= size * 8.0;
    encoder->reference = current;
    encoder->frame_num = (encoder->frame_num + 1) % (1u << LOG2_MAX_FRAME_NUM);
    encoder->frames++;
    return size;
}

static uint8_t *vaapi_plane(struct encoder *base, int index, int *stride)
{
    struct vaapi *encoder = (struct vaapi *)base;
    *stride = index == 0 ? encoder->width : encoder->width / 2;
    return encoder->planes[index];
}

static int vaapi_headers(struct encoder *base, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size)
{
    struct vaapi *encoder = (struct vaapi *)base;
    /* Without the start code in front, as the decoder configuration carries them. */
    const int start = 4;
    *sps = encoder->sps + start;
    *sps_size = encoder->sps_size - start;
    *pps = encoder->pps + start;
    *pps_size = encoder->pps_size - start;
    return 0;
}

/* The new rate counts from the next frame on. */
static int vaapi_set_rate(struct encoder *base, int kbps, int buffer_kbits, int quality)
{
    struct vaapi *encoder = (struct vaapi *)base;
    encoder->kbps = kbps;
    encoder->buffer_kbits = buffer_kbits;
    encoder->quality = quality;
    return 0;
}

static void vaapi_close(struct encoder *base)
{
    struct vaapi *encoder = (struct vaapi *)base;
    pthread_mutex_lock(&calls);
    if (encoder->coded != VA_INVALID_ID)
        va.vaDestroyBuffer(display, encoder->coded);
    if (encoder->context != VA_INVALID_ID)
        va.vaDestroyContext(display, encoder->context);
    if (encoder->surfaces[INPUT] != VA_INVALID_SURFACE)
        va.vaDestroySurfaces(display, encoder->surfaces, SURFACES);
    if (encoder->config != VA_INVALID_ID)
        va.vaDestroyConfig(display, encoder->config);
    pthread_mutex_unlock(&calls);
    for (int plane = 0; plane < 3; plane++)
        free(encoder->planes[plane]);
    nal_units_free(&encoder->unit);
    free(encoder);
}

struct encoder *vaapi_open(const struct encoder_setup *setup)
{
    pthread_once(&once, load);
    int width_mbs = (setup->width + MACROBLOCK - 1) / MACROBLOCK, height_mbs = (setup->height + MACROBLOCK - 1) / MACROBLOCK;
    if (!loaded || setup->full_chroma || width_mbs * height_mbs > MOST_MACROBLOCKS)
        return NULL;
    struct vaapi *encoder = calloc(1, sizeof *encoder);
    if (!encoder)
        return NULL;
    encoder->base.ops = &vaapi_ops;
    encoder->base.fps = setup->fps; /* set again by encoder_open; the sequence needs it before */
    encoder->config = encoder->context = encoder->coded = VA_INVALID_ID;
    for (int at = 0; at < SURFACES; at++)
        encoder->surfaces[at] = VA_INVALID_SURFACE;
    encoder->width = setup->width;
    encoder->height = setup->height;
    encoder->width_mbs = width_mbs;
    encoder->height_mbs = height_mbs;
    encoder->kbps = setup->kbps;
    encoder->buffer_kbits = BUFFER_KBITS(setup->kbps);
    encoder->quality = setup->quality;
    encoder->allowance = BUFFER_KBITS(setup->kbps) * 1000.0; /* the buffer begins full */
    size_t luma = (size_t)setup->width * setup->height;
    for (int plane = 0; plane < 3; plane++)
        if (!(encoder->planes[plane] = malloc(plane ? luma / 4 : luma)))
            goto failed;
    write_sps(encoder, setup->fps);
    write_pps(encoder);
    if (encoder->sps_size < 0 || encoder->pps_size < 0)
        goto failed;

    VAConfigAttrib attributes[] = {
        { .type = VAConfigAttribRTFormat, .value = VA_RT_FORMAT_YUV420 },
        { .type = VAConfigAttribRateControl, .value = VA_RC_CQP },
        { .type = VAConfigAttribEncPackedHeaders, .value = VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_PICTURE },
    };
    int surface_width = width_mbs * MACROBLOCK, surface_height = height_mbs * MACROBLOCK;
    pthread_mutex_lock(&calls);
    int made = va.vaCreateConfig(display, PROFILE, ENTRYPOINT, attributes, 3, &encoder->config) == VA_STATUS_SUCCESS
        && va.vaCreateSurfaces(display, VA_RT_FORMAT_YUV420, surface_width, surface_height, encoder->surfaces, SURFACES, NULL, 0) == VA_STATUS_SUCCESS
        && va.vaCreateContext(display, encoder->config, surface_width, surface_height, VA_PROGRESSIVE, encoder->surfaces, SURFACES, &encoder->context) == VA_STATUS_SUCCESS
        /* A coded frame is never larger than the picture's raw size. */
        && va.vaCreateBuffer(display, encoder->context, VAEncCodedBufferType, surface_width * surface_height * 3 / 2, 1, NULL, &encoder->coded) == VA_STATUS_SUCCESS;
    pthread_mutex_unlock(&calls);
    if (made)
        return &encoder->base;
failed:
    vaapi_close(&encoder->base);
    return NULL;
}

static const struct encoder_ops vaapi_ops = { "vaapi", vaapi_plane, vaapi_headers, vaapi_encode, vaapi_set_rate, vaapi_close };

#endif
