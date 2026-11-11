/* What the Rust side sees of an encoder, whichever implementation is behind it. */
#include <stdint.h>

struct encoder;

struct encoder_ops {
    uint8_t *(*plane)(struct encoder *encoder, int index, int *stride);
    int (*headers)(struct encoder *encoder, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size);
    int (*encode)(struct encoder *encoder, uint8_t **data, int *keyframe);
    int (*set_bitrate)(struct encoder *encoder, int kbps);
    void (*close)(struct encoder *encoder);
};

/* Each implementation's own struct starts with this. */
struct encoder {
    const struct encoder_ops *ops;
};

/* NULL when the machine has no usable hardware encoder. */
struct encoder *nvenc_open(int width, int height, int fps, int kbps, int full_chroma);
