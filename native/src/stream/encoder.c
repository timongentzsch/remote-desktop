/* The encoders behind one interface, so that the Rust side depends neither on which one is in use nor on the
   layout of their structs: x264 in x264.c, and the GPU's encoder in nvenc.c on NVIDIA's and vaapi.c on
   Intel's and AMD's. */
#include <stddef.h>
#include "encoder.h"
#include "nal.h"

/* Opens the GPU's encoder if `gpu` is set, x264 otherwise. NULL if that one cannot be used. On the GPU, HEVC
   where the client decodes it and the GPU has it, H.264 otherwise. */
struct encoder *encoder_open(const struct encoder_setup *setup, int gpu)
{
    struct encoder *encoder = NULL;
    if (!gpu)
        encoder = software_open(setup);
    if (gpu && setup->hevc)
        encoder = vaapi_open(setup, CODEC_HEVC);
    if (gpu && !encoder)
        encoder = nvenc_open(setup);
    if (gpu && !encoder)
        encoder = vaapi_open(setup, CODEC_H264);
    if (encoder)
        encoder->fps = setup->fps;
    return encoder;
}

/* A plane of the picture to encode, for the caller to fill; it stays valid while the encoder is open. */
uint8_t *encoder_plane(struct encoder *encoder, int index, int *stride)
{
    return encoder->ops->plane(encoder, index, stride);
}

/* Points `sets` at the raw parameter sets (see encoder_ops.headers), of which there are room for
   MOST_PARAMETER_SETS. Returns how many there are, or -1. */
int encoder_headers(struct encoder *encoder, uint8_t **sets, int *sizes)
{
    return encoder->ops->headers(encoder, sets, sizes);
}

/* Which codec it encodes: CODEC_H264 or CODEC_HEVC. */
int encoder_codec(struct encoder *encoder)
{
    return encoder->codec;
}

/* Encodes the picture in the planes. Returns the size of the access unit at *data (length-prefixed NAL
   units, valid until the next call), or 0 or less if there is none. *flags says what the encoder can tell of the
   frame (FRAME_COARSE, FRAME_HELD_BACK). With `fresh` the frame is one
   that needs none before it, for a client that lost its place in the stream. */
int encoder_encode(struct encoder *encoder, int fresh, uint8_t **data, int *keyframe, int *flags)
{
    *flags = 0;
    return encoder->ops->encode(encoder, fresh, data, keyframe, flags);
}

/* Tells a running encoder the bitrate cap and how many frames a second it will be given from now on, which
   together say how much each frame may take, and the quality to aim for. The encoders are told a frame rate
   only when opened, so fewer frames a second are expressed as a higher bitrate at that rate. Returns 0 on
   success. */
int encoder_set_rate(struct encoder *encoder, int kbps, int fps, int quality)
{
    return encoder->ops->set_rate(encoder, (int)((int64_t)kbps * encoder->fps / fps), BUFFER_KBITS(kbps), quality);
}

/* Which encoder it is: "x264", "nvenc" or "vaapi". */
const char *encoder_name(struct encoder *encoder)
{
    return encoder->ops->name;
}

void encoder_close(struct encoder *encoder)
{
    encoder->ops->close(encoder);
}
