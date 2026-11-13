/* Hardware encoding through VA-API, on Intel's and AMD's GPUs: H.264 (vaapi_h264.c), and HEVC (vaapi_hevc.c)
   for a client that decodes it. libva is loaded at run time, so the binary needs it neither to build nor to
   start; where it is missing, or no GPU here encodes as these files need, vaapi_open fails and the caller uses
   another encoder.

   VA-API encodes what it is told to: the codecs' files write the stream's parameter sets themselves, which the
   stream needs for its decoder configuration anyway, and give the driver every frame's picture and slice. Each
   frame refers to the one before; the first, and one a client needs to begin with, is an IDR. Only 4:2:0.

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
#define START_CODE_LENGTH 4
#define MOST_BUFFERS 16

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

int vaapi_add_misc(struct vaapi *encoder, VAEncMiscParameterType type, const void *value, unsigned size, VABufferID *buffers, int *count)
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

int vaapi_add_packed(struct vaapi *encoder, unsigned kind, const uint8_t *nal, int size, VABufferID *buffers, int *count)
{
    VAEncPackedHeaderParameterBuffer header = { .type = kind, .bit_length = size * 8, .has_emulation_bytes = 1 };
    if (vaapi_add(encoder, VAEncPackedHeaderParameterBufferType, &header, sizeof header, buffers, count) < 0)
        return -1;
    return vaapi_add(encoder, VAEncPackedHeaderDataBufferType, nal, size, buffers, count);
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
        || vaapi_add_packed(encoder, VAEncPackedHeaderSequence, joined, size, buffers, count) < 0)
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

static int vaapi_encode(struct encoder *base, int fresh, uint8_t **data, int *keyframe, int *flags)
{
    struct vaapi *encoder = (struct vaapi *)base;
    int idr = fresh || encoder->reference == 0, size = -1;
    if (idr) {
        encoder->frames = 0;
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
    /* The cap, not the quality target, set the quantiser, as the budget this file keeps said. */
    *flags = qp > encoder->quality + COARSE_MARGIN ? FRAME_COARSE | FRAME_HELD_BACK : 0;
    encoder->last_qp = qp;
    encoder->last_bits = size * 8;
    encoder->allowance -= size * 8.0;
    encoder->reference = current;
    encoder->frames++;
    return size;
}

static uint8_t *vaapi_plane(struct encoder *base, int index, int *stride)
{
    struct vaapi *encoder = (struct vaapi *)base;
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
    for (int at = 0; at < SURFACES; at++)
        encoder->surfaces[at] = VA_INVALID_SURFACE;
    encoder->width = setup->width;
    encoder->height = setup->height;
    encoder->width_blocks = (setup->width + kind->block - 1) / kind->block;
    encoder->height_blocks = (setup->height + kind->block - 1) / kind->block;
    encoder->kbps = setup->kbps;
    encoder->buffer_kbits = BUFFER_KBITS(setup->kbps);
    encoder->quality = setup->quality;
    encoder->allowance = BUFFER_KBITS(setup->kbps) * 1000.0; /* the buffer begins full */
    size_t luma = (size_t)setup->width * setup->height;
    for (int plane = 0; plane < 3; plane++)
        if (!(encoder->planes[plane] = malloc(plane ? luma / 4 : luma)))
            goto failed;
    kind->write_headers(encoder);
    for (int at = 0; at < encoder->header_count; at++)
        if (encoder->header_sizes[at] < 0)
            goto failed;

    VAConfigAttrib attributes[] = {
        { .type = VAConfigAttribRTFormat, .value = VA_RT_FORMAT_YUV420 },
        { .type = VAConfigAttribRateControl, .value = VA_RC_CQP },
        { .type = VAConfigAttribEncPackedHeaders, .value = kind->packed_headers },
    };
    int surface_width = encoder->width_blocks * kind->block, surface_height = encoder->height_blocks * kind->block;
    pthread_mutex_lock(&calls);
    int made = va.vaCreateConfig(display, kind->profile, ENTRYPOINT, attributes, 3, &encoder->config) == VA_STATUS_SUCCESS
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
