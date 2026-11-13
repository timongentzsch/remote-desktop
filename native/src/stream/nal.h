/* H.264's NAL units: finding them where an encoder writes them after start codes, rewriting them with their
   length in front as the stream carries them (see flv.rs), and writing the parameter sets an encoder that
   does not make its own needs from its caller. */
#ifndef NAL_H
#define NAL_H

#include <stdint.h>

/* NAL unit types (H.264, table 7-1). */
#define NAL_SPS 7
#define NAL_PPS 8
#define NAL_AUD 9

/* Where the next start code (00 00 01) at or after `from` begins, or `size` if there is none. */
int nal_start_code(const uint8_t *bytes, int size, int from);
/* Finds the NAL unit that follows the start code at `code`: sets *begin and *next, the next start code, and
   returns its length. */
int nal_unit(const uint8_t *bytes, int size, int code, int *begin, int *next);

/* The length-prefixed NAL units of one access unit, in a buffer that grows as needed. */
struct nal_units {
    uint8_t *bytes;
    int capacity;
};
/* Rewrites `bytes`, NAL units after start codes, into `units` with each one's length in front, leaving out
   parameter sets and access unit delimiters: the stream carries the former in its decoder configuration and
   needs none of the latter. Returns the size, or -1 without memory. */
int nal_length_prefixed(struct nal_units *units, const uint8_t *bytes, int size);
void nal_units_free(struct nal_units *units);

/* An RBSP being written, most significant bit first; large enough for parameter sets. */
#define BITS_ROOM 256
struct bits {
    uint8_t bytes[BITS_ROOM];
    int length; /* in bits */
};
void bits_put(struct bits *bits, uint32_t value, int count);
/* Exp-Golomb codes, unsigned and signed. */
void bits_put_ue(struct bits *bits, uint32_t value);
void bits_put_se(struct bits *bits, int32_t value);
/* Ends the RBSP: a one, then zeros to the end of the byte. */
void bits_trailing(struct bits *bits);

/* Writes `rbsp` into `out` as a NAL unit of `type` that is referred to: a four-byte start code, the header
   byte and the RBSP with emulation prevention. Returns the length, or -1 if `capacity` is too small. */
int nal_write(uint8_t *out, int capacity, int type, const struct bits *rbsp);

#endif
