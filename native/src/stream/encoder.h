/* What the Rust side sees of an encoder, whichever implementation is behind it. */
#include <stdint.h>

struct encoder;

struct encoder_ops {
    uint8_t *(*plane)(struct encoder *encoder, int index, int *stride);
    int (*headers)(struct encoder *encoder, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size);
    int (*encode)(struct encoder *encoder, uint8_t **data, int *keyframe, int *coarse);
    /* The bitrate at the frame rate the encoder was opened with, and the most one frame may take. */
    int (*set_rate)(struct encoder *encoder, int kbps, int buffer_kbits);
    void (*close)(struct encoder *encoder);
};

/* Each implementation's own struct starts with this. */
struct encoder {
    const struct encoder_ops *ops;
    int fps; /* as it was opened; it goes on being told a rate for this many frames a second */
};

/* How much one frame may take at most: what the bitrate cap allows in a fifth of a second. */
#define BUFFER_KBITS(kbps) ((kbps) / 5)

/* The constant-quality target both encoders aim for, on H.264's scale of quantisers (lower is better). A
   picture is given the bits this quality needs and no more, however high the bitrate cap. */
#define QUALITY 20

/* How much work the software encoder can put into a frame: 0 is the least, EFFORTS - 1 the most. More makes a
   frame of the same size look better and takes longer. The GPU's encoder has the one level. */
#define EFFORTS 3

struct encoder *software_open(int width, int height, int fps, int kbps, int full_chroma, int effort);
/* NULL when the machine has no usable hardware encoder. */
struct encoder *nvenc_open(int width, int height, int fps, int kbps, int full_chroma);
