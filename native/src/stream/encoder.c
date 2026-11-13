/* The encoders behind one interface, so that the Rust side depends neither on which one is in use nor on the
   layout of their structs: x264 in x264.c, and the GPU's encoder in nvenc.c on NVIDIA's and vaapi.c on
   Intel's and AMD's. */
#include "encoder.h"

/* Opens the GPU's encoder if `gpu` is set, x264 otherwise. NULL if that one cannot be used. */
struct encoder *encoder_open(const struct encoder_setup *setup, int gpu)
{
    struct encoder *encoder = !gpu ? software_open(setup) : nvenc_open(setup);
    if (gpu && !encoder)
        encoder = vaapi_open(setup);
    if (encoder)
        encoder->fps = setup->fps;
    return encoder;
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
   units, valid until the next call), or 0 or less if there is none. *coarse is set where the encoder can tell
   that the bitrate cap, not the quality target, decided how good the frame is. With `fresh` the frame is one
   that needs none before it, for a client that lost its place in the stream. */
int encoder_encode(struct encoder *encoder, int fresh, uint8_t **data, int *keyframe, int *coarse)
{
    *coarse = 0;
    return encoder->ops->encode(encoder, fresh, data, keyframe, coarse);
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
