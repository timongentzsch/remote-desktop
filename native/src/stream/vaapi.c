/* Hardware encoding through VA-API, on Intel's and AMD's GPUs: H.264 (vaapi_h264.c), and HEVC (vaapi_hevc.c)
   for a client that decodes it. libva is loaded at run time, so the binary needs it neither to build nor to
   start; where it is missing, or no GPU here encodes as these files need, vaapi_open fails and the caller uses
   another encoder.

   VA-API encodes what it is told to: the codecs' files write the stream's parameter sets themselves, which the
   stream needs for its decoder configuration anyway, and give the driver every frame's picture and slice. Each
   frame refers to the one before; the first, and one a client needs to begin with, is an IDR. A codec may keep
   older frames besides (vaapi_codec.kept), which a frame for a client that lost those after them refers to
   instead. Only 4:2:0.

   The bitrate is this file's to keep, around a constant quantiser: the drivers' own rate controls that aim for
   a quality under a cap fell well short of the quality (Intel's QVBR began at quantiser 48 where 20 was asked
   for), and left an unchanged picture as it was where it was to be refined. Each frame is given the quality
   target's quantiser unless the frames before it have used up what the cap allows; see `choose_qp`. */
#include <stddef.h>
#include "encoder.h"

#ifndef __linux__
struct encoder *vaapi_open(const struct encoder_setup *setup, int codec)
{
    (void)setup;
    (void)codec;
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
#include "vaapi.h"

/* The render nodes looked at for a GPU that encodes: /dev/dri/renderD128 and the ones after it. */
#define RENDER_FIRST 128
#define RENDER_NODES 8
#define ENTRYPOINT VAEntrypointEncSlice
/* The coarsest quantiser; and how much smaller a frame comes out a quantiser step coarser, about: six steps
   halve it, so 2^(1/6). */
#define MOST_QP 51
#define QP_STEP 1.12246
/* How many steps finer than the last frame's the next frame's quantiser may be. Refining a still picture, a
   frame's size at a finer quantiser depends on how much finer it is than what the picture has, which the last
   frame's size does not foresee; a large step would come out too large for the cap and be thrown away whole. */
#define MOST_QP_FALL 2
#define MOST_BUFFERS 16
/* Frames are kept for at most this many frames: HEVC's picture order counts in the decoder's buffer must lie
   within 2048 of each other. */
#define MOST_AGE 1024

#define FUNCTIONS(F) \
    F(vaInitialize) F(vaTerminate) F(vaSetErrorCallback) F(vaSetInfoCallback) F(vaGetConfigAttributes) \
    F(vaCreateConfig) F(vaDestroyConfig) F(vaCreateSurfaces) F(vaDestroySurfaces) F(vaCreateContext) \
    F(vaDestroyContext) F(vaCreateBuffer) F(vaDestroyBuffer) F(vaMapBuffer) F(vaUnmapBuffer) F(vaBeginPicture) \
    F(vaRenderPicture) F(vaEndPicture) F(vaSyncSurface) F(vaDeriveImage) F(vaDestroyImage) \
    F(vaQuerySurfaceAttributes)

static struct {
    VADisplay (*vaGetDisplayDRM)(int fd);
    /* Since libva 2.1; without it the pictures are copied into the input (see vaapi_gpu_buffer). */
    __typeof__(vaExportSurfaceHandle) *vaExportSurfaceHandle;
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

static const struct encoder_ops vaapi_ops;

/* libva's messages, which say for instance which driver it tries, are not this program's to show. */
static void quiet(void *context, const char *message)
{
    (void)context;
    (void)message;
}

/* Whether the GPU behind `candidate` encodes `codec` as these files need: 4:2:0 at a constant quantiser, with
   the headers the codec's file writes. */
static int encodes(VADisplay candidate, const struct vaapi_codec *codec)
{
    VAConfigAttrib wanted[] = {
        { .type = VAConfigAttribRTFormat },
        { .type = VAConfigAttribRateControl },
        { .type = VAConfigAttribEncPackedHeaders },
    };
    if (va.vaGetConfigAttributes(candidate, codec->profile, ENTRYPOINT, wanted, 3) != VA_STATUS_SUCCESS)
        return 0;
    const unsigned packed = codec->packed_headers;
    return wanted[0].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[0].value & VA_RT_FORMAT_YUV420)
        && wanted[1].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[1].value & VA_RC_CQP)
        && wanted[2].value != VA_ATTRIB_NOT_SUPPORTED && (wanted[2].value & packed) == packed;
}

/* Finds the first GPU that encodes H.264, which every one that encodes at all does. */
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
    va.vaExportSurfaceHandle = dlsym(library, "vaExportSurfaceHandle");
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
            if (encodes(candidate, &vaapi_h264)) {
                display = candidate; /* kept open with its file for the process's lifetime */
                loaded = 1;
                return;
            }
            va.vaTerminate(candidate);
        }
        close(fd);
    }
}

int vaapi_add(struct vaapi *encoder, VABufferType type, const void *data, unsigned size, VABufferID *buffers, int *count)
{
    VABufferID buffer;
    if (*count >= MOST_BUFFERS || va.vaCreateBuffer(display, encoder->context, type, size, 1, (void *)data, &buffer) != VA_STATUS_SUCCESS)
        return -1;
    buffers[(*count)++] = buffer;
    return 0;
}

/* One of the encoder's settings, as libva takes them: a type and its value in one buffer. */
static int vaapi_add_misc(struct vaapi *encoder, VAEncMiscParameterType type, const void *value, unsigned size, VABufferID *buffers, int *count)
{
    uint8_t misc[sizeof(VAEncMiscParameterBuffer) + 64] = { 0 };
    ((VAEncMiscParameterBuffer *)misc)->type = type;
    memcpy(((VAEncMiscParameterBuffer *)misc)->data, value, size);
    return vaapi_add(encoder, VAEncMiscParameterBufferType, misc, sizeof(VAEncMiscParameterBuffer) + size, buffers, count);
}

void vaapi_header(struct vaapi *encoder, int type, const struct bits *rbsp)
{
    int at = encoder->header_count++;
    encoder->header_sizes[at] = nal_write(encoder->headers[at], sizeof encoder->headers[at], encoder->codec->codec, type, rbsp);
}

void vaapi_vui_colours(struct bits *sps)
{
    bits_put(sps, 1, 1); /* vui_parameters_present_flag */
    bits_put(sps, 0, 1); /* aspect_ratio_info_present_flag */
    bits_put(sps, 0, 1); /* overscan_info_present_flag */
    bits_put(sps, 1, 1); /* video_signal_type_present_flag */
    bits_put(sps, 5, 3); /* video_format: unspecified */
    bits_put(sps, 1, 1); /* video_full_range_flag */
    bits_put(sps, 1, 1); /* colour_description_present_flag */
    bits_put(sps, BT709, 8);
    bits_put(sps, BT709, 8);
    bits_put(sps, BT709, 8);
    bits_put(sps, 1, 1); /* chroma_loc_info_present_flag */
    bits_put_ue(sps, CHROMA_AT_CENTRE);
    bits_put_ue(sps, CHROMA_AT_CENTRE);
}

int vaapi_add_packed(struct vaapi *encoder, unsigned kind, const uint8_t *nal, int bits, VABufferID *buffers, int *count)
{
    VAEncPackedHeaderParameterBuffer header = { .type = kind, .bit_length = bits, .has_emulation_bytes = 1 };
    if (vaapi_add(encoder, VAEncPackedHeaderParameterBufferType, &header, sizeof header, buffers, count) < 0)
        return -1;
    return vaapi_add(encoder, VAEncPackedHeaderDataBufferType, nal, (bits + 7) / 8, buffers, count);
}

/* What goes with an IDR: the sequence, the driver's speed, and the parameter sets, all of which head the
   sequence, as ffmpeg's encoders give them too. */
static int add_idr(struct vaapi *encoder, VABufferID *buffers, int *count)
{
    VAEncMiscParameterBufferQualityLevel speed = { .quality_level = SPEED };
    uint8_t joined[MOST_PARAMETER_SETS * BITS_ROOM * 2];
    int size = 0;
    for (int at = 0; at < encoder->header_count; at++) {
        memcpy(joined + size, encoder->headers[at], encoder->header_sizes[at]);
        size += encoder->header_sizes[at];
    }
    if (encoder->codec->add_sequence(encoder, buffers, count) < 0
        || vaapi_add_misc(encoder, VAEncMiscParameterTypeQualityLevel, &speed, sizeof speed, buffers, count) < 0
        || vaapi_add_packed(encoder, VAEncPackedHeaderSequence, joined, size * 8, buffers, count) < 0)
        return -1;
    return 0;
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

/* The quantiser for the next frame: the quality target's, unless the last frame's size says that at it the
   frame would take more than the cap allows now; then the finest that fits. A first frame has nothing to go
   by, and is held to the cap after the fact (see vaapi_encode). */
static int choose_qp(const struct vaapi *encoder)
{
    if (!encoder->last_bits)
        return encoder->quality;
    int qp = encoder->quality;
    double predicted = encoder->last_bits; /* the last frame's size at this quantiser */
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

/* Encodes the picture in the input surface at quantiser `qp` into the surface `current`, as an IDR or as a
   frame that refers to the last one, and leaves its length-prefixed NAL units in encoder->unit. Returns their
   size, or -1. */
static int encode_at(struct vaapi *encoder, int idr, int current, int qp)
{
    int size = -1, count = 0;
    VABufferID buffers[MOST_BUFFERS];
    if ((idr && add_idr(encoder, buffers, &count) < 0) || encoder->codec->add_frame(encoder, idr, current, qp, buffers, &count) < 0)
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
        size = nal_length_prefixed(&encoder->unit, joined, length, encoder->codec->codec);
    free(joined);
done:
    for (int at = 0; at < count; at++)
        va.vaDestroyBuffer(display, buffers[at]);
    return size;
}

static void drop(struct vaapi *encoder, int at)
{
    struct reference *list = encoder->references;
    memmove(list + at, list + at + 1, (size_t)(encoder->reference_count - at - 1) * sizeof *list);
    encoder->reference_count--;
}

/* Leaves out the frames after the one `confirmed`, so that the next refers to that one or one before it.
   Returns -1 where none is left. */
static int go_back(struct vaapi *encoder, int64_t confirmed)
{
    while (encoder->reference_count && encoder->references[0].id > confirmed)
        drop(encoder, 0);
    return encoder->reference_count ? 0 : -1;
}

/* A surface to reconstruct the next frame into: one that holds none of the frames referred to. */
static int free_surface(const struct vaapi *encoder)
{
    for (int surface = INPUT + 1;; surface++) {
        int taken = 0;
        for (int at = 0; at < encoder->reference_count; at++)
            taken |= encoder->references[at].surface == surface;
        if (!taken)
            return surface;
    }
}

/* Makes the frame just made the one the next refers to, and keeps of those before it what the codec allows,
   newest first: the latest one the client has (`confirmed`), and none older, which it will not need; the last
   one only where it is at least a share of the frames the client lags behind newer than the next kept, so
   that those kept reach back to what the client has; and of the others the newest. */
static void keep(struct vaapi *encoder, struct reference made, int64_t confirmed)
{
    struct reference *list = encoder->references;
    int kept = encoder->codec->kept;
    if (!kept)
        encoder->reference_count = 0;
    int64_t has = -1;
    for (int at = 0; at < encoder->reference_count && has < 0; at++)
        if (list[at].id <= confirmed) {
            has = list[at].id;
            encoder->reference_count = at + 1;
        }
    int64_t spacing = confirmed >= 0 ? (made.id - confirmed) / (kept > 1 ? kept - 1 : 1) : 0;
    if (encoder->reference_count >= 2 && list[0].id != has && list[0].id - list[1].id < spacing)
        drop(encoder, 0);
    for (int at = encoder->reference_count - 1; at >= 0; at--)
        if (made.order - list[at].order >= MOST_AGE)
            drop(encoder, at);
    while (encoder->reference_count > kept) {
        int oldest = encoder->reference_count - 1;
        drop(encoder, list[oldest].id == has ? oldest - 1 : oldest);
    }
    memmove(list + 1, list, (size_t)encoder->reference_count * sizeof *list);
    list[0] = made;
    encoder->reference_count++;
}

static int vaapi_encode(struct encoder *base, const struct encode_request *request, uint8_t **data, int *keyframe, int *flags)
{
    struct vaapi *encoder = (struct vaapi *)base;
    int idr = request->kind == ENCODE_KEYFRAME || encoder->reference_count == 0, size = -1;
    int recovered = !idr && request->kind == ENCODE_RECOVER;
    if (recovered && go_back(encoder, request->confirmed) < 0)
        idr = 1, recovered = 0;
    if (idr) {
        encoder->reference_count = 0;
        encoder->frames = 0;
        encoder->idr_id++;
    }
    /* What the cap allows fills by its share of a frame, up to the buffer's size. */
    double buffer = encoder->buffer_kbits * 1000.0, share = encoder->kbps * 1000.0 / encoder->base.fps;
    encoder->allowance = encoder->allowance + share < buffer ? encoder->allowance + share : buffer;
    int current = free_surface(encoder), qp = choose_qp(encoder);
    pthread_mutex_lock(&calls);
    if (encoder->on_gpu || upload(encoder) == 0)
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
    /* The cap, not the quality target, set the quantiser, as the budget this file keeps said. */
    *flags = qp > encoder->quality + COARSE_MARGIN ? FRAME_COARSE | FRAME_HELD_BACK : 0;
    *flags |= qp > POOR_QP ? FRAME_POOR : 0;
    *flags |= recovered ? FRAME_RECOVERED : 0;
    encoder->last_qp = qp;
    encoder->last_bits = size * 8;
    encoder->allowance -= size * 8.0;
    keep(encoder, (struct reference){ current, request->id, encoder->frames }, request->confirmed);
    encoder->frames++;
    return size;
}

static uint8_t *vaapi_plane(struct encoder *base, int index, int *stride)
{
    struct vaapi *encoder = (struct vaapi *)base;
    encoder->on_gpu = 0;
    *stride = index == 0 ? encoder->width : encoder->width / 2;
    return encoder->planes[index];
}

/* Without the start code in front, as the decoder configuration carries them. */
static int vaapi_headers(struct encoder *base, uint8_t **sets, int *sizes)
{
    struct vaapi *encoder = (struct vaapi *)base;
    for (int at = 0; at < encoder->header_count; at++) {
        sets[at] = encoder->headers[at] + START_CODE_LENGTH;
        sizes[at] = encoder->header_sizes[at] - START_CODE_LENGTH;
    }
    return encoder->header_count;
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

unsigned vaapi_attribute(const struct vaapi *encoder, VAConfigAttribType type)
{
    VAConfigAttrib attribute = { .type = type };
    pthread_mutex_lock(&calls);
    int told = va.vaGetConfigAttributes(display, encoder->codec->profile, ENTRYPOINT, &attribute, 1) == VA_STATUS_SUCCESS;
    pthread_mutex_unlock(&calls);
    return told ? attribute.value : VA_ATTRIB_NOT_SUPPORTED;
}

/* Takes what the driver asks the surfaces' size be a multiple of, where it says (AMD's does, for HEVC). */
static void take_alignment(struct vaapi *encoder)
{
    unsigned count = 0;
    if (va.vaQuerySurfaceAttributes(display, encoder->config, NULL, &count) != VA_STATUS_SUCCESS || !count)
        return;
    VASurfaceAttrib *attributes = calloc(count, sizeof *attributes);
    if (attributes && va.vaQuerySurfaceAttributes(display, encoder->config, attributes, &count) == VA_STATUS_SUCCESS)
        for (unsigned at = 0; at < count; at++)
            if (attributes[at].type == VASurfaceAttribAlignmentSize) {
                VASurfaceAttribAlignmentStruct alignment = { .value = (uint32_t)attributes[at].value.value.i };
                encoder->align_width = 1 << alignment.bits.log2_width_alignment;
                encoder->align_height = 1 << alignment.bits.log2_height_alignment;
            }
    free(attributes);
}

/* Makes the screen's surface, BGRX at the screen's size, and the video processor, if there are none yet or the
   size changed, and hands the surface out as a DMA-BUF. */
static int vaapi_gpu_buffer(struct encoder *base, int width, int height, struct gpu_buffer *buffer)
{
    struct vaapi *encoder = (struct vaapi *)base;
    VASurfaceAttrib bgrx = {
        .type = VASurfaceAttribPixelFormat,
        .flags = VA_SURFACE_ATTRIB_SETTABLE,
        .value = { .type = VAGenericValueTypeInteger, .value.i = VA_FOURCC_BGRX },
    };
    VADRMPRIMESurfaceDescriptor shared;
    if (!va.vaExportSurfaceHandle)
        return -1;
    pthread_mutex_lock(&calls);
    int ready = encoder->processor != VA_INVALID_ID
        || (va.vaCreateConfig(display, VAProfileNone, VAEntrypointVideoProc, NULL, 0, &encoder->processor_config) == VA_STATUS_SUCCESS
            && va.vaCreateContext(display, encoder->processor_config, encoder->width, encoder->height, VA_PROGRESSIVE, NULL, 0, &encoder->processor) == VA_STATUS_SUCCESS);
    if (ready && encoder->screen != VA_INVALID_SURFACE && (encoder->screen_width != width || encoder->screen_height != height)) {
        va.vaDestroySurfaces(display, &encoder->screen, 1);
        encoder->screen = VA_INVALID_SURFACE;
    }
    if (ready && encoder->screen == VA_INVALID_SURFACE) {
        ready = va.vaCreateSurfaces(display, VA_RT_FORMAT_RGB32, width, height, &encoder->screen, 1, &bgrx, 1) == VA_STATUS_SUCCESS;
        encoder->screen_width = width;
        encoder->screen_height = height;
    }
    unsigned flags = VA_EXPORT_SURFACE_WRITE_ONLY | VA_EXPORT_SURFACE_COMPOSED_LAYERS;
    ready = ready && va.vaExportSurfaceHandle(display, encoder->screen, VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, flags, &shared) == VA_STATUS_SUCCESS;
    pthread_mutex_unlock(&calls);
    if (!ready)
        return -1;
    /* The screen is one plane in one object: anything else is not what was made. */
    if (shared.num_objects != 1 || shared.num_layers != 1 || shared.layers[0].num_planes != 1) {
        for (unsigned at = 0; at < shared.num_objects; at++)
            close(shared.objects[at].fd);
        return -1;
    }
    *buffer = (struct gpu_buffer){
        .fd = shared.objects[0].fd,
        .format = shared.layers[0].drm_format,
        .offset = shared.layers[0].offset[0],
        .pitch = shared.layers[0].pitch[0],
        .modifier = shared.objects[0].drm_format_modifier,
    };
    return 0;
}

/* Turns the screen's surface into the input, made the picture's size: BT.709 at full range, as the planes are
   (see convert.rs). */
static int vaapi_take_gpu_picture(struct encoder *base)
{
    struct vaapi *encoder = (struct vaapi *)base;
    VARectangle picture = { 0, 0, (uint16_t)encoder->width, (uint16_t)encoder->height };
    VAProcPipelineParameterBuffer pipeline = {
        .surface = encoder->screen,
        .surface_color_standard = VAProcColorStandardBT709,
        .output_region = &picture,
        .output_color_standard = VAProcColorStandardBT709,
        .input_color_properties.color_range = VA_SOURCE_RANGE_FULL,
        .output_color_properties.color_range = VA_SOURCE_RANGE_FULL,
    };
    VABufferID buffer;
    pthread_mutex_lock(&calls);
    int done = encoder->screen != VA_INVALID_SURFACE
        && va.vaCreateBuffer(display, encoder->processor, VAProcPipelineParameterBufferType, sizeof pipeline, 1, &pipeline, &buffer) == VA_STATUS_SUCCESS;
    if (done) {
        done = va.vaBeginPicture(display, encoder->processor, encoder->surfaces[INPUT]) == VA_STATUS_SUCCESS;
        done = done && va.vaRenderPicture(display, encoder->processor, &buffer, 1) == VA_STATUS_SUCCESS;
        done = va.vaEndPicture(display, encoder->processor) == VA_STATUS_SUCCESS && done;
        va.vaDestroyBuffer(display, buffer);
    }
    pthread_mutex_unlock(&calls);
    encoder->on_gpu = done;
    return done ? 0 : -1;
}

static void vaapi_close(struct encoder *base)
{
    struct vaapi *encoder = (struct vaapi *)base;
    pthread_mutex_lock(&calls);
    if (encoder->processor != VA_INVALID_ID)
        va.vaDestroyContext(display, encoder->processor);
    if (encoder->processor_config != VA_INVALID_ID)
        va.vaDestroyConfig(display, encoder->processor_config);
    if (encoder->screen != VA_INVALID_SURFACE)
        va.vaDestroySurfaces(display, &encoder->screen, 1);
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

struct encoder *vaapi_open(const struct encoder_setup *setup, int codec)
{
    pthread_once(&once, load);
    const struct vaapi_codec *kind = codec == CODEC_HEVC ? &vaapi_hevc : &vaapi_h264;
    if (!loaded || setup->full_chroma || setup->width * setup->height > kind->most_samples || !encodes(display, kind))
        return NULL;
    struct vaapi *encoder = calloc(1, sizeof *encoder);
    if (!encoder)
        return NULL;
    encoder->base.ops = &vaapi_ops;
    encoder->base.codec = kind->codec;
    encoder->base.fps = setup->fps; /* set again by encoder_open; the parameter sets need it before */
    encoder->codec = kind;
    encoder->config = encoder->context = encoder->coded = VA_INVALID_ID;
    encoder->processor_config = encoder->processor = VA_INVALID_ID;
    encoder->screen = VA_INVALID_SURFACE;
    for (int at = 0; at < SURFACES; at++)
        encoder->surfaces[at] = VA_INVALID_SURFACE;
    encoder->width = setup->width;
    encoder->height = setup->height;
    encoder->block = kind->block;
    encoder->width_blocks = (setup->width + kind->block - 1) / kind->block;
    encoder->height_blocks = (setup->height + kind->block - 1) / kind->block;
    encoder->align_width = encoder->align_height = 1;
    encoder->kbps = setup->kbps;
    encoder->buffer_kbits = BUFFER_KBITS(setup->kbps, setup->buffer);
    encoder->quality = setup->quality;
    encoder->allowance = encoder->buffer_kbits * 1000.0; /* the buffer begins full */
    size_t luma = (size_t)setup->width * setup->height;
    for (int plane = 0; plane < 3; plane++)
        if (!(encoder->planes[plane] = malloc(plane ? luma / 4 : luma)))
            goto failed;
    unsigned taken = vaapi_attribute(encoder, VAConfigAttribEncPackedHeaders);
    encoder->packed = kind->packed_headers | (taken == VA_ATTRIB_NOT_SUPPORTED ? 0 : taken & kind->packed_if_taken);
    VAConfigAttrib attributes[] = {
        { .type = VAConfigAttribRTFormat, .value = VA_RT_FORMAT_YUV420 },
        { .type = VAConfigAttribRateControl, .value = VA_RC_CQP },
        { .type = VAConfigAttribEncPackedHeaders, .value = encoder->packed },
    };
    pthread_mutex_lock(&calls);
    int configured = va.vaCreateConfig(display, kind->profile, ENTRYPOINT, attributes, 3, &encoder->config) == VA_STATUS_SUCCESS;
    if (configured)
        take_alignment(encoder);
    pthread_mutex_unlock(&calls);
    if (!configured)
        goto failed;
    if (kind->configure)
        kind->configure(encoder);
    kind->write_headers(encoder);
    for (int at = 0; at < encoder->header_count; at++)
        if (encoder->header_sizes[at] < 0)
            goto failed;

    int surface_width = encoder->width_blocks * encoder->block, surface_height = encoder->height_blocks * encoder->block;
    pthread_mutex_lock(&calls);
    int made = va.vaCreateSurfaces(display, VA_RT_FORMAT_YUV420, surface_width, surface_height, encoder->surfaces, SURFACES, NULL, 0) == VA_STATUS_SUCCESS
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

static const struct encoder_ops vaapi_ops = {
    "vaapi", vaapi_plane, vaapi_headers, vaapi_encode, vaapi_set_rate, vaapi_close, vaapi_gpu_buffer, vaapi_take_gpu_picture,
};

#endif
