/* What the Rust side sees of an encoder, whichever implementation is behind it. */
#include <stdint.h>

struct encoder;

struct encoder_ops {
    uint8_t *(*plane)(struct encoder *encoder, int index, int *stride);
    int (*headers)(struct encoder *encoder, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size);
    int (*encode)(struct encoder *encoder, int fresh, uint8_t **data, int *keyframe, int *coarse);
    /* The bitrate at the frame rate the encoder was opened with, the most one frame may take, and the quality
       target. */
    int (*set_rate)(struct encoder *encoder, int kbps, int buffer_kbits, int quality);
    void (*close)(struct encoder *encoder);
};

/* Each implementation's own struct starts with this. */
struct encoder {
    const struct encoder_ops *ops;
    int fps; /* as it was opened; it goes on being told a rate for this many frames a second */
};

/* How much one frame may take at most: what the bitrate cap allows in a fifth of a second. */
#define BUFFER_KBITS(kbps) ((kbps) / 5)

/* What an encoder is opened with, as stream/encode.rs makes it (`Setup`): the picture's size, the frame rate it
   is told, the bitrate cap in kbit/s, the constant-quality target on H.264's scale of quantisers (lower is
   better; a picture is given the bits this quality needs and no more, however high the cap), 4:4:4 or 4:2:0,
   and for x264 its effort. */
struct encoder_setup {
    int width, height, fps, kbps, quality, full_chroma, effort;
};

/* How the picture's colours are told to the decoder, in H.264's codes: full range, BT.709 primaries, transfer
   and matrix (as convert.rs makes them), and 4:2:0 chroma sited at each 2x2 block's centre. */
#define BT709 1
#define CHROMA_AT_CENTRE 1

/* How much work the software encoder can put into a frame: 0 is the least, EFFORTS - 1 the most. More makes a
   frame of the same size look better and takes longer. The GPU's encoder has the one level. */
#define EFFORTS 4

struct encoder *software_open(const struct encoder_setup *setup);
/* NULL when the machine has no usable hardware encoder. */
struct encoder *nvenc_open(const struct encoder_setup *setup);
