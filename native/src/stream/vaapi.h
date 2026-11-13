/* What vaapi.c, which talks to the GPU and keeps the bitrate, shares with the codecs' files, vaapi_h264.c and
   vaapi_hevc.c, which write their parameter sets and fill in their parameter buffers. Linux only. */
#ifndef VAAPI_H
#define VAAPI_H

#include <va/va.h>
#include "encoder.h"
#include "nal.h"

/* Surfaces: the picture to encode, and two the driver reconstructs frames into, the one being made and the one
   it refers to. */
#define INPUT 0
#define SURFACES 3
/* The quantiser parameter sets and pictures state before each slice adjusts it. */
#define INITIAL_QP 26
/* No IDR comes unasked: the driver is given one as long a period as it takes. */
#define NO_PERIOD 0x7FFFFFFF
/* The driver's speed against quality, 1 the best and 7 the fastest. */
#define SPEED 4

struct vaapi;

/* A codec's part. */
struct vaapi_codec {
    int codec; /* as nal.h names it */
    VAProfile profile;
    /* Surfaces are a multiple of this many pixels each way: H.264's macroblock, HEVC's coding tree block. */
    int block;
    /* The most luma samples a picture may have at the level the parameter sets state. */
    int most_samples;
    /* Which headers the codec's file writes for the driver (VA_ENC_PACKED_HEADER_...): the parameter sets
       always, the slice headers where the driver does not write them itself. */
    unsigned packed_headers;
    /* Writes the parameter sets into encoder->headers. */
    void (*write_headers)(struct vaapi *encoder);
    /* Adds the buffers that go with an IDR, and those of a frame: one that refers to nothing if `idr`, else
       to the surface encoder->reference; made into the surface `current` at quantiser `qp`. */
    int (*add_sequence)(struct vaapi *encoder, VABufferID *buffers, int *count);
    int (*add_frame)(struct vaapi *encoder, int idr, int current, int qp, VABufferID *buffers, int *count);
};

extern const struct vaapi_codec vaapi_h264, vaapi_hevc;

struct vaapi {
    struct encoder base;
    const struct vaapi_codec *codec;
    VAConfigID config;
    VAContextID context;
    VASurfaceID surfaces[SURFACES];
    VABufferID coded;
    /* The picture's size, and in blocks. */
    int width, height, width_blocks, height_blocks;
    int kbps, buffer_kbits, quality;
    /* What the cap allows to be sent now, in bits: it fills by the cap's share of each frame up to the size of
       the buffer, and each frame empties it by its size. And the quantiser and size of the last frame. */
    double allowance;
    int last_qp, last_bits;
    /* Frames since the last IDR, which is numbered by idr_id; reference is the surface the last frame was
       reconstructed into, 0 before the first. */
    uint32_t frames, idr_id;
    int reference;
    /* The picture as the caller fills it, 4:2:0 in three planes; it is copied into the input surface. */
    uint8_t *planes[3];
    struct nal_units unit;
    /* The parameter sets as NAL units after start codes, as the driver writes them into the stream. */
    uint8_t headers[MOST_PARAMETER_SETS][BITS_ROOM * 2];
    int header_sizes[MOST_PARAMETER_SETS], header_count;
};

/* Makes a parameter buffer of `type` with a copy of `data` and adds it to `buffers`. */
int vaapi_add(struct vaapi *encoder, VABufferType type, const void *data, unsigned size, VABufferID *buffers, int *count);
/* One of the encoder's settings, as libva takes them: a type and its value in one buffer. */
int vaapi_add_misc(struct vaapi *encoder, VAEncMiscParameterType type, const void *value, unsigned size, VABufferID *buffers, int *count);
/* A header the driver writes into the stream as it is, NAL units after start codes, heading the part of it
   `kind` (VAEncPackedHeader...) says. */
int vaapi_add_packed(struct vaapi *encoder, unsigned kind, const uint8_t *nal, int size, VABufferID *buffers, int *count);
/* Adds `rbsp` to encoder->headers as a parameter set of `type`. */
void vaapi_header(struct vaapi *encoder, int type, const struct bits *rbsp);

#endif
