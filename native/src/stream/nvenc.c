/* Hardware H.264 encoding on NVIDIA GPUs. The driver's libraries are loaded at run time, so the binary needs
   them neither to build nor to start; where they are missing or the GPU has no encoder, nvenc_open fails
   and the caller uses x264. */
/* The loader reports what it cannot find; here a missing library is an ordinary case and needs no message. */
#define FFNV_LOG_FUNC(logctx, msg, ...) ((void)(logctx))
#define FFNV_DEBUG_LOG_FUNC(logctx, msg, ...) ((void)(logctx))
#include <ffnvcodec/dynlink_loader.h>
#include <pthread.h>
#include <string.h>
#include <time.h>
#include "encoder.h"

/* P1 is the fastest preset and P7 the best; P3 costs about a millisecond per frame more than P1. */
#define PRESET NV_ENC_PRESET_P3_GUID
#define TUNING NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY
#define NAL_SPS 7
#define NAL_PPS 8

struct nvenc {
    struct encoder base;
    void *session;
    NV_ENC_INITIALIZE_PARAMS setup;
    NV_ENC_CONFIG config;
    NV_ENC_INPUT_PTR input;
    NV_ENC_OUTPUT_PTR output;
    int width, height, full_chroma;
    uint64_t frames;
    /* The picture as the caller fills it; it is copied into the driver's buffer for each frame. */
    uint8_t *planes[3];
    /* The last access unit, converted to length-prefixed NAL units. */
    uint8_t *unit;
    int capacity;
    uint8_t headers[1024];
};

static const struct encoder_ops nvenc_ops;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static CudaFunctions *cuda;
static NV_ENCODE_API_FUNCTION_LIST api;
/* The GPU context all encoders share. It is made once and kept: making one per encoder costs a tenth of a
   second each time and leaves memory behind in the driver's library. */
static CUcontext context;
static int loaded;

/* Every call into the driver's encoder happens between enter() and leave(), which make the shared context
   current on the calling thread for that long. */
static void enter(void)
{
    cuda->cuCtxPushCurrent(context);
}

static void leave(void)
{
    CUcontext unused;
    cuda->cuCtxPopCurrent(&unused);
}

static void load(void)
{
    NvencFunctions *library = NULL; /* the loader frees whatever a non-null pointer holds */
    uint32_t newest;
    if (cuda_load_functions(&cuda, NULL) < 0 || nvenc_load_functions(&library, NULL) < 0)
        return;
    /* A driver older than the API these headers describe cannot be used. */
    if (library->NvEncodeAPIGetMaxSupportedVersion(&newest) != NV_ENC_SUCCESS || newest < (NVENCAPI_MAJOR_VERSION << 4 | NVENCAPI_MINOR_VERSION))
        return;
    api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    CUdevice device;
    /* Blocking rather than spinning while the GPU works keeps the processor free. */
    if (library->NvEncodeAPICreateInstance(&api) != NV_ENC_SUCCESS
        || cuda->cuInit(0) != CUDA_SUCCESS
        || cuda->cuDeviceGet(&device, 0) != CUDA_SUCCESS
        || cuda->cuCtxCreate(&context, CU_CTX_SCHED_BLOCKING_SYNC, device) != CUDA_SUCCESS)
        return;
    leave(); /* creating the context made it current on this thread */
    loaded = 1;
}

static void set_rate(NV_ENC_RC_PARAMS *rate, int kbps, int buffer_kbits)
{
    rate->averageBitRate = rate->maxBitRate = (uint32_t)kbps * 1000;
    rate->vbvBufferSize = rate->vbvInitialDelay = (uint32_t)buffer_kbits * 1000;
}

static int width_of(const struct nvenc *encoder, int plane)
{
    return plane == 0 || encoder->full_chroma ? encoder->width : encoder->width / 2;
}

static int rows_of(const struct nvenc *encoder, int plane)
{
    return plane == 0 || encoder->full_chroma ? encoder->height : encoder->height / 2;
}

/* Takes an encoder apart and frees it. */
static void destroy(struct nvenc *encoder)
{
    if (encoder->session) {
        enter();
        if (encoder->frames) {
            /* The driver must be told that the stream has ended before its encoder is taken apart. */
            NV_ENC_PIC_PARAMS end = { .version = NV_ENC_PIC_PARAMS_VER, .encodePicFlags = NV_ENC_PIC_FLAG_EOS };
            api.nvEncEncodePicture(encoder->session, &end);
        }
        if (encoder->output)
            api.nvEncDestroyBitstreamBuffer(encoder->session, encoder->output);
        if (encoder->input)
            api.nvEncDestroyInputBuffer(encoder->session, encoder->input);
        api.nvEncDestroyEncoder(encoder->session);
        leave();
    }
    for (int plane = 0; plane < 3; plane++)
        free(encoder->planes[plane]);
    free(encoder->unit);
    free(encoder);
}

/* The driver (seen with version 580) crashes, a moment later and on a thread of its own, when one of several
   encoders is destroyed while another is still in use. So this process has one encoder at a time. A second
   one is asked for when a client reconnects before its old session has wound down, and then the wait is
   short. If the encoder stays taken, because someone else is watching, the newcomer gets none and the caller
   encodes with x264. */
static pthread_mutex_t turn = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t released = PTHREAD_COND_INITIALIZER;
static int taken;
#define TURN_WAIT_SECONDS 1

static int take_turn(void)
{
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += TURN_WAIT_SECONDS;
    pthread_mutex_lock(&turn);
    while (taken && pthread_cond_timedwait(&released, &turn, &deadline) == 0)
        ;
    int mine = !taken;
    if (mine)
        taken = 1;
    pthread_mutex_unlock(&turn);
    return mine;
}

static void give_turn(void)
{
    pthread_mutex_lock(&turn);
    taken = 0;
    pthread_cond_signal(&released);
    pthread_mutex_unlock(&turn);
}

static void nvenc_close(struct encoder *base)
{
    destroy((struct nvenc *)base);
    give_turn();
}

struct encoder *nvenc_open(int width, int height, int fps, int kbps, int full_chroma)
{
    pthread_once(&once, load);
    if (!loaded || !take_turn())
        return NULL;
    struct nvenc *encoder = calloc(1, sizeof *encoder);
    if (!encoder) {
        give_turn();
        return NULL;
    }
    encoder->base.ops = &nvenc_ops;
    encoder->width = width;
    encoder->height = height;
    encoder->full_chroma = full_chroma;
    for (int plane = 0; plane < 3; plane++)
        if (!(encoder->planes[plane] = malloc((size_t)width_of(encoder, plane) * rows_of(encoder, plane))))
            goto failed;

    enter();
    NV_ENC_PRESET_CONFIG preset = { .version = NV_ENC_PRESET_CONFIG_VER, .presetCfg.version = NV_ENC_CONFIG_VER };
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS session = {
        .version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
        .deviceType = NV_ENC_DEVICE_TYPE_CUDA,
        .device = context,
        .apiVersion = NVENCAPI_VERSION,
    };
    int opened = api.nvEncOpenEncodeSessionEx(&session, &encoder->session) == NV_ENC_SUCCESS
        && api.nvEncGetEncodePresetConfigEx(encoder->session, NV_ENC_CODEC_H264_GUID, PRESET, TUNING, &preset) == NV_ENC_SUCCESS;
    if (opened) {
        NV_ENC_CONFIG *config = &encoder->config;
        *config = preset.presetCfg;
        config->profileGUID = full_chroma ? NV_ENC_H264_PROFILE_HIGH_444_GUID : NV_ENC_H264_PROFILE_HIGH_GUID;
        /* One key frame at the start, then every frame refers to the one before it. */
        config->gopLength = NVENC_INFINITE_GOPLENGTH;
        config->frameIntervalP = 1;
        config->rcParams.rateControlMode = NV_ENC_PARAMS_RC_VBR;
        config->rcParams.targetQuality = QUALITY;
        config->rcParams.zeroReorderDelay = 1;
        set_rate(&config->rcParams, kbps, BUFFER_KBITS(kbps));
        NV_ENC_CONFIG_H264 *h264 = &config->encodeCodecConfig.h264Config;
        h264->idrPeriod = NVENC_INFINITE_GOPLENGTH;
        h264->disableSPSPPS = 1; /* the parameter sets travel in the stream's configuration record instead */
        h264->chromaFormatIDC = full_chroma ? 3 : 1;
        h264->sliceMode = 3; /* a fixed number of slices per frame: one */
        h264->sliceModeData = 1;
        NV_ENC_CONFIG_H264_VUI_PARAMETERS *vui = &h264->h264VUIParameters;
        vui->videoSignalTypePresentFlag = 1;
        vui->videoFormat = 5; /* unspecified */
        vui->videoFullRangeFlag = 1;
        vui->colourDescriptionPresentFlag = 1;
        vui->colourPrimaries = vui->transferCharacteristics = vui->colourMatrix = 1; /* BT.709 */
        vui->chromaSampleLocationFlag = 1;
        vui->chromaSampleLocationTop = vui->chromaSampleLocationBot = 1; /* 4:2:0 chroma sits at each block's centre */

        encoder->setup = (NV_ENC_INITIALIZE_PARAMS){
            .version = NV_ENC_INITIALIZE_PARAMS_VER,
            .encodeGUID = NV_ENC_CODEC_H264_GUID,
            .presetGUID = PRESET,
            .tuningInfo = TUNING,
            .encodeWidth = width,
            .encodeHeight = height,
            .darWidth = width,
            .darHeight = height,
            .frameRateNum = fps,
            .frameRateDen = 1,
            .enablePTD = 1, /* the encoder decides each picture's type */
            .encodeConfig = config,
        };
        NV_ENC_CREATE_INPUT_BUFFER input = {
            .version = NV_ENC_CREATE_INPUT_BUFFER_VER,
            .width = width,
            .height = height,
            .bufferFmt = full_chroma ? NV_ENC_BUFFER_FORMAT_YUV444 : NV_ENC_BUFFER_FORMAT_IYUV,
        };
        NV_ENC_CREATE_BITSTREAM_BUFFER output = { .version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER };
        opened = api.nvEncInitializeEncoder(encoder->session, &encoder->setup) == NV_ENC_SUCCESS
            && api.nvEncCreateInputBuffer(encoder->session, &input) == NV_ENC_SUCCESS
            && api.nvEncCreateBitstreamBuffer(encoder->session, &output) == NV_ENC_SUCCESS;
        encoder->input = input.inputBuffer;
        encoder->output = output.bitstreamBuffer;
    }
    leave();
    if (opened)
        return &encoder->base;
failed:
    nvenc_close(&encoder->base);
    return NULL;
}

static uint8_t *nvenc_plane(struct encoder *base, int index, int *stride)
{
    struct nvenc *encoder = (struct nvenc *)base;
    *stride = width_of(encoder, index);
    return encoder->planes[index];
}

/* Where the next start code (00 00 01) at or after `from` begins, or `size` if there is none. */
static int start_code(const uint8_t *bytes, int size, int from)
{
    for (int at = from; at + 3 <= size; at++)
        if (bytes[at] == 0 && bytes[at + 1] == 0 && bytes[at + 2] == 1)
            return at;
    return size;
}

/* Finds the NAL unit that follows the start code at `code`: sets *begin and returns its length. Zero bytes
   in front of the next start code belong to that start code; a NAL unit never ends in one. */
static int nal_unit(const uint8_t *bytes, int size, int code, int *begin, int *next)
{
    *begin = code + 3;
    *next = start_code(bytes, size, *begin);
    int end = *next;
    while (end > *begin && bytes[end - 1] == 0)
        end--;
    return end - *begin;
}

static int nvenc_headers(struct encoder *base, uint8_t **sps, int *sps_size, uint8_t **pps, int *pps_size)
{
    struct nvenc *encoder = (struct nvenc *)base;
    uint32_t size = 0;
    NV_ENC_SEQUENCE_PARAM_PAYLOAD payload = {
        .version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER,
        .inBufferSize = sizeof encoder->headers,
        .spsppsBuffer = encoder->headers,
        .outSPSPPSPayloadSize = &size,
    };
    enter();
    NVENCSTATUS status = api.nvEncGetSequenceParams(encoder->session, &payload);
    leave();
    if (status != NV_ENC_SUCCESS)
        return -1;
    for (int code = start_code(encoder->headers, size, 0), begin, next; code < (int)size; code = next) {
        int length = nal_unit(encoder->headers, size, code, &begin, &next);
        int type = length ? encoder->headers[begin] & 0x1F : 0;
        if (type == NAL_SPS) {
            *sps = encoder->headers + begin;
            *sps_size = length;
        } else if (type == NAL_PPS) {
            *pps = encoder->headers + begin;
            *pps_size = length;
        }
    }
    return 0;
}

/* Copies the driver's output, NAL units separated by start codes, into encoder->unit with each unit's
   length in front instead. Returns the size, or -1 without memory. */
static int length_prefixed(struct nvenc *encoder, const uint8_t *bytes, int size)
{
    /* A three-byte start code becomes a four-byte length, so each unit can grow by one byte. */
    int needed = size + size / 3 + 4;
    if (encoder->capacity < needed) {
        uint8_t *grown = realloc(encoder->unit, needed);
        if (!grown)
            return -1;
        encoder->unit = grown;
        encoder->capacity = needed;
    }
    int written = 0;
    for (int code = start_code(bytes, size, 0), begin, next; code < size; code = next) {
        int length = nal_unit(bytes, size, code, &begin, &next);
        uint8_t *out = encoder->unit + written;
        out[0] = length >> 24;
        out[1] = length >> 16;
        out[2] = length >> 8;
        out[3] = length;
        memcpy(out + 4, bytes + begin, length);
        written += 4 + length;
    }
    return written;
}

static int nvenc_encode(struct encoder *base, int fresh, uint8_t **data, int *keyframe, int *coarse)
{
    (void)coarse; /* stays unset: this encoder does not say */
    struct nvenc *encoder = (struct nvenc *)base;
    int size = -1;
    enter();

    NV_ENC_LOCK_INPUT_BUFFER input = { .version = NV_ENC_LOCK_INPUT_BUFFER_VER, .inputBuffer = encoder->input };
    if (api.nvEncLockInputBuffer(encoder->session, &input) != NV_ENC_SUCCESS)
        goto done;
    /* The driver's buffer holds the planes one after another; its rows are `pitch` apart for the first plane
       and for full-size colour planes, and half that for half-size ones. */
    uint8_t *to = input.bufferDataPtr;
    for (int plane = 0; plane < 3; plane++) {
        int width = width_of(encoder, plane);
        int pitch = plane == 0 || encoder->full_chroma ? input.pitch : input.pitch / 2;
        for (int row = 0; row < rows_of(encoder, plane); row++, to += pitch)
            memcpy(to, encoder->planes[plane] + (size_t)row * width, width);
    }
    api.nvEncUnlockInputBuffer(encoder->session, encoder->input);

    NV_ENC_PIC_PARAMS picture = {
        .version = NV_ENC_PIC_PARAMS_VER,
        .inputWidth = encoder->width,
        .inputHeight = encoder->height,
        .inputPitch = input.pitch,
        .inputBuffer = encoder->input,
        .outputBitstream = encoder->output,
        .bufferFmt = encoder->full_chroma ? NV_ENC_BUFFER_FORMAT_YUV444 : NV_ENC_BUFFER_FORMAT_IYUV,
        .pictureStruct = NV_ENC_PIC_STRUCT_FRAME,
        .inputTimeStamp = encoder->frames++,
        .encodePicFlags = fresh ? NV_ENC_PIC_FLAG_FORCEIDR : 0,
    };
    NV_ENC_LOCK_BITSTREAM stream = { .version = NV_ENC_LOCK_BITSTREAM_VER, .outputBitstream = encoder->output };
    /* Locking the output waits until the frame is encoded. */
    if (api.nvEncEncodePicture(encoder->session, &picture) != NV_ENC_SUCCESS || api.nvEncLockBitstream(encoder->session, &stream) != NV_ENC_SUCCESS)
        goto done;
    size = length_prefixed(encoder, stream.bitstreamBufferPtr, stream.bitstreamSizeInBytes);
    *data = encoder->unit;
    *keyframe = stream.pictureType == NV_ENC_PIC_TYPE_IDR;
    api.nvEncUnlockBitstream(encoder->session, encoder->output);
done:
    leave();
    return size;
}

static int nvenc_set_rate(struct encoder *base, int kbps, int buffer_kbits)
{
    struct nvenc *encoder = (struct nvenc *)base;
    NV_ENC_RC_PARAMS before = encoder->config.rcParams;
    set_rate(&encoder->config.rcParams, kbps, buffer_kbits);
    NV_ENC_RECONFIGURE_PARAMS change = { .version = NV_ENC_RECONFIGURE_PARAMS_VER, .reInitEncodeParams = encoder->setup };
    enter();
    NVENCSTATUS status = api.nvEncReconfigureEncoder(encoder->session, &change);
    leave();
    if (status != NV_ENC_SUCCESS)
        encoder->config.rcParams = before;
    return status == NV_ENC_SUCCESS ? 0 : -1;
}

static const struct encoder_ops nvenc_ops = { nvenc_plane, nvenc_headers, nvenc_encode, nvenc_set_rate, nvenc_close };
