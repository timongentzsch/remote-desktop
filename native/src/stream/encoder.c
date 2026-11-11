/* The encoders behind one interface, so that the Rust side depends neither on which one is in use nor on the
   layout of their structs: x264 in x264.c and the GPU's encoder in nvenc.c. */
#include "encoder.h"

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
