/* See nal.h. */
#include <stdlib.h>
#include <string.h>
#include "nal.h"

#define START_CODE_LENGTH 4
#define LENGTH_PREFIX 4
/* nal_ref_idc of a NAL unit that is referred to, in the header byte's bits 5 and 6. */
#define REFERENCED (3 << 5)

int nal_start_code(const uint8_t *bytes, int size, int from)
{
    for (int at = from; at + 3 <= size; at++)
        if (bytes[at] == 0 && bytes[at + 1] == 0 && bytes[at + 2] == 1)
            return at;
    return size;
}

/* Zero bytes in front of the next start code belong to that start code; a NAL unit never ends in one. */
int nal_unit(const uint8_t *bytes, int size, int code, int *begin, int *next)
{
    *begin = code + 3;
    *next = nal_start_code(bytes, size, *begin);
    int end = *next;
    while (end > *begin && bytes[end - 1] == 0)
        end--;
    return end - *begin;
}

int nal_length_prefixed(struct nal_units *units, const uint8_t *bytes, int size)
{
    /* A three-byte start code becomes a four-byte length, so each unit can grow by one byte. */
    int needed = size + size / 3 + LENGTH_PREFIX;
    if (units->capacity < needed) {
        uint8_t *grown = realloc(units->bytes, needed);
        if (!grown)
            return -1;
        units->bytes = grown;
        units->capacity = needed;
    }
    int written = 0;
    for (int code = nal_start_code(bytes, size, 0), begin, next; code < size; code = next) {
        int length = nal_unit(bytes, size, code, &begin, &next);
        int type = length ? bytes[begin] & 0x1F : 0;
        if (!length || type == NAL_SPS || type == NAL_PPS || type == NAL_AUD)
            continue;
        uint8_t *out = units->bytes + written;
        out[0] = length >> 24;
        out[1] = length >> 16;
        out[2] = length >> 8;
        out[3] = length;
        memcpy(out + LENGTH_PREFIX, bytes + begin, length);
        written += LENGTH_PREFIX + length;
    }
    return written;
}

void nal_units_free(struct nal_units *units)
{
    free(units->bytes);
    units->bytes = NULL;
    units->capacity = 0;
}

void bits_put(struct bits *bits, uint32_t value, int count)
{
    for (int bit = count - 1; bit >= 0 && bits->length < BITS_ROOM * 8; bit--, bits->length++) {
        uint8_t *byte = &bits->bytes[bits->length / 8];
        if (bits->length % 8 == 0)
            *byte = 0;
        *byte |= ((value >> bit) & 1) << (7 - bits->length % 8);
    }
}

void bits_put_ue(struct bits *bits, uint32_t value)
{
    uint64_t coded = (uint64_t)value + 1;
    int width = 0;
    while (coded >> (width + 1))
        width++;
    bits_put(bits, 0, width);
    bits_put(bits, (uint32_t)coded, width + 1);
}

void bits_put_se(struct bits *bits, int32_t value)
{
    bits_put_ue(bits, value > 0 ? 2 * (uint32_t)value - 1 : 2 * (uint32_t)-value);
}

void bits_trailing(struct bits *bits)
{
    bits_put(bits, 1, 1);
    while (bits->length % 8)
        bits_put(bits, 0, 1);
}

int nal_write(uint8_t *out, int capacity, int type, const struct bits *rbsp)
{
    int size = rbsp->length / 8, written = 0;
    /* Each RBSP byte may need an emulation prevention byte in front. */
    if (capacity < START_CODE_LENGTH + 1 + 2 * size)
        return -1;
    static const uint8_t start_code[START_CODE_LENGTH] = { 0, 0, 0, 1 };
    memcpy(out, start_code, START_CODE_LENGTH);
    written = START_CODE_LENGTH;
    out[written++] = REFERENCED | type;
    /* Two zero bytes may not be followed by 00 to 03 inside a NAL unit: a 03 goes between. */
    for (int at = 0, zeros = 0; at < size; at++) {
        if (zeros >= 2 && rbsp->bytes[at] <= 3) {
            out[written++] = 3;
            zeros = 0;
        }
        out[written++] = rbsp->bytes[at];
        zeros = rbsp->bytes[at] ? 0 : zeros + 1;
    }
    return written;
}
