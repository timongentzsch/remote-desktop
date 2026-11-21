/* What vaapi.c, which talks to the GPU and keeps the bitrate, shares with the codecs' files, vaapi_h264.c and
   vaapi_hevc.c, which write their parameter sets and fill in their parameter buffers. Linux only. */
#ifndef VAAPI_H
#define VAAPI_H

#include <va/va.h>
#include "encoder.h"
#include "nal.h"

/* How many frames besides the last one a codec may keep for a client that lost the frames after them. */
#define MOST_KEPT 3
/* Surfaces: the picture to encode, and those the driver reconstructs frames into: the one being made, the one
   it refers to, and those kept. */
#define INPUT 0
#define SURFACES (3 + MOST_KEPT)
/* The quantiser parameter sets and pictures state before each slice adjusts it. */
#define INITIAL_QP 26
/* No IDR comes unasked: the driver is given one as long a period as it takes. */
#define NO_PERIOD 0x7FFFFFFF
/* The driver's speed against quality, 1 the best and 7 the fastest. On Intel's Coffee Lake, HEVC at
   3456x1994 with code scrolling by: 7 took 9.4 ms a frame where 4 took 26.5, for the same picture (51.6 dB)
   in 12% more bits; at 4 such a screen got no more than 38 frames a second. */
#define SPEED 7

struct vaapi;

/* A codec's part. */
struct vaapi_codec {
    int codec; /* as nal.h names it */
    VAProfile profile;
    /* Surfaces are a multiple of this many pixels each way: H.264's macroblock, HEVC's coding tree block, unless
       `configure` chooses another. */
    int block;
    /* The most luma samples a picture may have at the level the parameter sets state. */
    int most_samples;
    /* How many frames besides the last one it keeps, up to MOST_KEPT, for a client that lost those after them
       (ENCODE_RECOVER): a frame for it refers to the latest of them that the client has. */
    int kept;
    /* Which headers the codec's file writes for the driver (VA_ENC_PACKED_HEADER_...): the parameter sets
       always, the slice headers where the driver does not write them itself. */
    unsigned packed_headers;
    /* Where set, chooses what the driver allows once its configuration is made (vaapi_attribute): the block
       sizes, encoder->block and the size in blocks with them, and whatever else the parameter sets need. */
    void (*configure)(struct vaapi *encoder);
    /* Writes the parameter sets into encoder->headers. */
    void (*write_headers)(struct vaapi *encoder);
    /* Adds the buffers that go with an IDR, and those of a frame: one that refers to nothing if `idr`, else
       to encoder->references[0], keeping the others; made into the surface `current` at quantiser `qp`. */
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
    /* The picture's size, and in blocks of `block` pixels. */
    int width, height, block, width_blocks, height_blocks;
    /* What the driver asks the surfaces' size be a multiple of, each way; 1 where it says nothing. */
    int align_width, align_height;
    /* HEVC's coding blocks and tools (vaapi_hevc.c), in log2 of pixels, and its coded picture's size. */
    struct hevc_blocks {
        int log2_ctb, log2_min_cb, log2_min_tb, log2_max_tb, depth_inter, depth_intra, amp, width, height;
    } hevc;
    int kbps, buffer_kbits, quality;
    /* What the cap allows to be sent now, in bits: it fills by the cap's share of each frame up to the size of
       the buffer, and each frame empties it by its size. And the quantiser and size of the last frame. */
    double allowance;
    int last_qp, last_bits;
    /* Frames since the last IDR, which is numbered by idr_id. */
    uint32_t frames, idr_id;
    /* The frames the next may refer to, newest first: the one it refers to, then those kept besides (see
       vaapi_codec.kept). Each is the surface it was reconstructed into, its id (encode_request.id) and its
       order since the IDR. None before the first frame. */
    struct reference {
        int surface;
        int64_t id;
        uint32_t order;
    } references[1 + MOST_KEPT];
    int reference_count;
    /* The picture as the caller fills it, 4:2:0 in three planes; it is copied into the input surface. */
    uint8_t *planes[3];
    struct nal_units unit;
    /* The parameter sets as NAL units after start codes, as the driver writes them into the stream. */
    uint8_t headers[MOST_PARAMETER_SETS][BITS_ROOM * 2];
    int header_sizes[MOST_PARAMETER_SETS], header_count;
};

/* The value of the encoder's configuration attribute `type`, VA_ATTRIB_NOT_SUPPORTED where the driver does not
   say. */
unsigned vaapi_attribute(const struct vaapi *encoder, VAConfigAttribType type);
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
